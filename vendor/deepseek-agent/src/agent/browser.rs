//! 浏览器工具：桥接 Microsoft Playwright MCP (@playwright/mcp)。
//!
//! 以单个 `browser` 工具暴露，内部用 stdio JSON-RPC 驱动一个长驻的
//! @playwright/mcp 子进程。模型通过 action + params(JSON) 调用底层浏览器
//! 动作（navigate/click/type/snapshot/find/evaluate/fill_form/...）。

use crate::agent::tool::Tool;
use crate::error::{AgentError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

const REQUEST_TIMEOUT_SECS: u64 = 120;

struct McpClient {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
    child: Mutex<Option<Child>>,
    dead: AtomicBool,
}

impl McpClient {
    fn is_alive(&self) -> bool {
        if self.dead.load(Ordering::Relaxed) {
            return false;
        }
        let mut guard = self.child.lock().unwrap();
        match guard.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut line = serde_json::to_string(&msg)?;
        line.push_str("\n");
        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                self.dead.store(true, Ordering::Relaxed);
                return Err(AgentError::Other(format!("mcp 写入失败: {e}")));
            }
            stdin.flush().await.ok();
        }
        match tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), rx).await {
            Ok(Ok(v)) => {
                if let Some(err) = v.get("error") {
                    return Err(AgentError::Other(format!("mcp 错误: {err}")));
                }
                Ok(v.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(_)) => Err(AgentError::Other("mcp 连接已关闭".into())),
            Err(_) => Err(AgentError::Other("mcp 请求超时".into())),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut line = serde_json::to_string(&msg)?;
        line.push_str("\n");
        let mut stdin = self.stdin.lock().await;
        if let Err(e) = stdin.write_all(line.as_bytes()).await {
            self.dead.store(true, Ordering::Relaxed);
            return Err(AgentError::Other(format!("mcp 写入失败: {e}")));
        }
        stdin.flush().await.ok();
        Ok(())
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput> {
        let result = self
            .request("tools/call", json!({"name": name, "arguments": args}))
            .await?;
        Ok(render_result(&result))
    }
}

pub struct ToolOutput {
    pub text: String,
    pub image: Option<String>,
}

fn render_result(result: &Value) -> ToolOutput {
    let mut parts: Vec<String> = Vec::new();
    let mut image: Option<String> = None;
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        for item in content {
            let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ty == "image" {
                let mime = item.get("mimeType").and_then(|m| m.as_str()).unwrap_or("image/png");
                if let Some(data) = item.get("data").and_then(|d| d.as_str()) {
                    image = Some(format!("data:{mime};base64,{data}"));
                }
            } else if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                parts.push(text.to_string());
            } else if !ty.is_empty() {
                parts.push(format!("[{ty}]"));
            }
        }
    }
    let text = if parts.is_empty() {
        serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
    } else {
        parts.join("\n")
    };
    ToolOutput { text, image }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn bundled_runtime_dir() -> Option<PathBuf> {
    let dir = exe_dir()?.join("runtime");
    if dir.is_dir() { Some(dir) } else { None }
}

fn bundled_browsers_dir() -> Option<PathBuf> {
    let dir = bundled_runtime_dir()?.join("browsers");
    if dir.is_dir() { Some(dir) } else { None }
}

fn locate_node() -> String {
    if let Ok(p) = std::env::var("LIGONG_NODE") {
        if !p.trim().is_empty() && PathBuf::from(&p).exists() {
            return p;
        }
    }
    if let Some(rt) = bundled_runtime_dir() {
        #[cfg(windows)]
        let cand = rt.join("node.exe");
        #[cfg(not(windows))]
        let cand = rt.join("node");
        if cand.exists() {
            return cand.display().to_string();
        }
    }
    if let Ok(p) = std::env::var("LIGONG_NODE") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    "node".to_string()
}

fn locate_mcp_cli() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LIGONG_MCP_CLI") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    if let Some(rt) = bundled_runtime_dir() {
        let cli = rt.join("node_modules/@playwright/mcp/cli.js");
        if cli.exists() {
            return Some(cli);
        }
    }
    let home = dirs::home_dir()?;
    #[allow(unused_mut)]
    let mut candidates: Vec<PathBuf> = vec![
        home.join(".ligong-mcp/node_modules/@playwright/mcp/cli.js"),
    ];
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            candidates.push(
                PathBuf::from(&appdata)
                    .join("npm/node_modules/@playwright/mcp/cli.js"),
            );
        }
        candidates.push(
            home.join("AppData/Roaming/npm/node_modules/@playwright/mcp/cli.js"),
        );
    }
    candidates.into_iter().find(|p| p.exists())
}

fn locate_chromium() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LIGONG_CHROMIUM") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }

    fn scan_browsers_dir(dir: &std::path::Path) -> Option<PathBuf> {
        let mut best: Option<(u32, PathBuf)> = None;
        let entries = std::fs::read_dir(dir).ok()?;
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if let Some(ver) = name.strip_prefix("chromium-")
                && let Ok(n) = ver.parse::<u32>()
            {
                let candidates = [
                    e.path().join("chrome-win64/chrome.exe"),
                    e.path().join("chrome-win/chrome.exe"),
                    e.path().join("chrome-linux64/chrome"),
                    e.path().join("chrome-linux/chrome"),
                    e.path().join("chrome-mac/Chromium.app/Contents/MacOS/Chromium"),
                ];
                let exe = match candidates.into_iter().find(|p| p.exists()) {
                    Some(p) => p,
                    None => continue,
                };
                if best.as_ref().is_none_or(|(v, _)| n > *v) {
                    best = Some((n, exe));
                }
            }
        }
        best.map(|(_, p)| p)
    }

    if let Some(dir) = bundled_browsers_dir() {
        if let Some(exe) = scan_browsers_dir(&dir) {
            return Some(exe);
        }
    }

    let home = dirs::home_dir()?;
    #[cfg(windows)]
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join("AppData/Local"))
        .join("ms-playwright");
    #[cfg(not(windows))]
    let base = home.join(".cache/ms-playwright");

    scan_browsers_dir(&base)
}

fn browser_headless() -> bool {
    match std::env::var("LIGONG_BROWSER_HEADLESS").ok().as_deref() {
        Some("1") | Some("true") => true,
        Some("0") | Some("false") => false,
        _ => {
            #[cfg(windows)]
            { false }
            #[cfg(not(windows))]
            {
                std::env::var("DISPLAY").is_err()
                    && std::env::var("WAYLAND_DISPLAY").is_err()
            }
        }
    }
}

/// 持久化 profile 目录；设置 LIGONG_BROWSER_EPHEMERAL=1 则改用内存隔离（不持久化）。
/// key 非空时按 key 派生子目录，实现多项目浏览器实例隔离。
fn browser_profile(key: &str) -> Option<PathBuf> {
    if matches!(std::env::var("LIGONG_BROWSER_EPHEMERAL").ok().as_deref(), Some("1") | Some("true")) {
        return None;
    }
    if let Ok(p) = std::env::var("LIGONG_BROWSER_PROFILE") {
        let base = PathBuf::from(p);
        return Some(if key.is_empty() { base } else { base.join(key) });
    }
    let base = dirs::home_dir()?.join(".ligong-mcp/profile");
    Some(if key.is_empty() { base } else { base.join(key) })
}

/// MCP 输出目录：默认写到系统临时目录，避免污染当前工作目录。
/// key 非空时按 key 派生子目录。
fn browser_output_dir(key: &str) -> Option<PathBuf> {
    let dir = match std::env::var("LIGONG_BROWSER_OUTPUT_DIR") {
        Ok(p) => PathBuf::from(p),
        Err(_) => std::env::temp_dir().join("ligong-browser-mcp"),
    };
    let dir = if key.is_empty() { dir } else { dir.join(key) };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 导航超时（毫秒）：环境变量 LIGONG_BROWSER_NAV_TIMEOUT_MS 覆盖，默认 30000。
fn nav_timeout_ms() -> u64 {
    std::env::var("LIGONG_BROWSER_NAV_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000)
}

/// 单动作超时（毫秒）：环境变量 LIGONG_BROWSER_ACTION_TIMEOUT_MS 覆盖，默认 10000。
fn action_timeout_ms() -> u64 {
    std::env::var("LIGONG_BROWSER_ACTION_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
}

/// 启动 MCP 子进程并完成 initialize 握手，返回客户端句柄。
/// key 非空时使用按 key 隔离的 profile 与输出目录（多项目各用独立浏览器）。
async fn spawn_client(key: &str) -> Result<Arc<McpClient>> {
    let cli = locate_mcp_cli().ok_or_else(|| {
        AgentError::Other(
            "未找到 @playwright/mcp（cli.js）。请运行 scripts/setup_browser_mcp.bat（Windows）或 scripts/setup_browser_mcp.sh（Linux/macOS）安装，或设置 LIGONG_MCP_CLI 指向 cli.js。".into(),
        )
    })?;

    let node = locate_node();
    let mut cmd = Command::new(&node);
    if let Some(browsers) = bundled_browsers_dir() {
        cmd.env("PLAYWRIGHT_BROWSERS_PATH", browsers);
    }
    cmd.arg(&cli)
        .arg("--no-sandbox")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if browser_headless() {
        cmd.arg("--headless");
    }
    match browser_profile(key) {
        Some(profile) => { cmd.arg("--user-data-dir").arg(profile); }
        None => { cmd.arg("--isolated"); }
    }
    if let Some(chrome) = locate_chromium() {
        cmd.arg("--executable-path").arg(chrome);
    }
    if let Some(dir) = browser_output_dir(key) {
        cmd.arg("--output-dir").arg(dir);
    }
    cmd.arg("--timeout-navigation").arg(nav_timeout_ms().to_string());
    cmd.arg("--timeout-action").arg(action_timeout_ms().to_string());
    if let Ok(proxy) = std::env::var("LIGONG_BROWSER_PROXY") {
        if !proxy.trim().is_empty() {
            cmd.arg("--proxy-server").arg(proxy);
        }
    }

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| AgentError::Other(format!("启动 playwright-mcp 失败: {e}")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AgentError::Other("无法获取 mcp stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Other("无法获取 mcp stdout".into()))?;

    let client = Arc::new(McpClient {
        stdin: tokio::sync::Mutex::new(stdin),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        child: Mutex::new(Some(child)),
        dead: AtomicBool::new(false),
    });

    let reader_client = client.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(id) = msg.get("id").and_then(|i| i.as_u64())
                && let Some(tx) = reader_client.pending.lock().unwrap().remove(&id)
            {
                let _ = tx.send(msg);
            }
        }
        reader_client.dead.store(true, Ordering::Relaxed);
        reader_client.pending.lock().unwrap().clear();
    });

    client
        .request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "deepseek-agent", "version": "0.1.0"}
            }),
        )
        .await?;
    client.notify("notifications/initialized", json!({})).await?;
    Ok(client)
}

static CLIENTS: Mutex<Option<HashMap<String, Arc<McpClient>>>> = Mutex::new(None);

async fn client(key: &str) -> Result<Arc<McpClient>> {
    if let Some(existing) = CLIENTS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(key).cloned())
        && existing.is_alive()
    {
        return Ok(existing);
    }
    let fresh = spawn_client(key).await?;
    let mut guard = CLIENTS.lock().unwrap();
    guard.get_or_insert_with(HashMap::new).insert(key.to_string(), fresh.clone());
    Ok(fresh)
}

async fn kill_client(client: Arc<McpClient>) {
    let _ = client.call_tool("browser_close", json!({})).await;
    client.dead.store(true, Ordering::Relaxed);
    client.pending.lock().unwrap().clear();
    let maybe_child = {
        let mut guard = client.child.lock().unwrap();
        guard.take()
    };
    drop(client);
    if let Some(mut child) = maybe_child {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// 关闭所有项目的浏览器实例（进程退出时调用）。
pub async fn shutdown() {
    let all = CLIENTS.lock().unwrap().take();
    let Some(map) = all else { return; };
    for (_, client) in map {
        kill_client(client).await;
    }
}

/// 关闭指定 key 的浏览器实例（项目结束时调用）。
pub async fn shutdown_key(key: &str) {
    let existing = CLIENTS
        .lock()
        .unwrap()
        .as_mut()
        .and_then(|m| m.remove(key));
    if let Some(client) = existing {
        kill_client(client).await;
    }
}

/// 支持的浏览器动作（action → 底层 MCP 工具）。
const ACTIONS: &[(&str, &str)] = &[
    ("navigate", "browser_navigate"),
    ("back", "browser_navigate_back"),
    ("snapshot", "browser_snapshot"),
    ("find", "browser_find"),
    ("click", "browser_click"),
    ("type", "browser_type"),
    ("fill_form", "browser_fill_form"),
    ("select_option", "browser_select_option"),
    ("hover", "browser_hover"),
    ("press_key", "browser_press_key"),
    ("drag", "browser_drag"),
    ("drop", "browser_drop"),
    ("evaluate", "browser_evaluate"),
    ("run_code", "browser_run_code_unsafe"),
    ("upload", "browser_file_upload"),
    ("tabs", "browser_tabs"),
    ("wait_for", "browser_wait_for"),
    ("resize", "browser_resize"),
    ("console", "browser_console_messages"),
    ("network", "browser_network_requests"),
    ("network_detail", "browser_network_request"),
    ("screenshot", "browser_take_screenshot"),
    ("dialog", "browser_handle_dialog"),
    ("close", "browser_close"),
];

/// 搜索类导航默认改走 Bing：仅当目标是 google/duckduckgo 的搜索页时改写，
/// 其它 URL 原样返回。设置 LIGONG_BROWSER_NO_BING=1 可关闭。
fn maybe_redirect_search_to_bing(tool: &str, args: Value) -> Value {
    if tool != "browser_navigate" {
        return args;
    }
    if matches!(
        std::env::var("LIGONG_BROWSER_NO_BING").ok().as_deref(),
        Some("1") | Some("true")
    ) {
        return args;
    }
    let Some(url) = args.get("url").and_then(|u| u.as_str()) else {
        return args;
    };
    let Some(query) = extract_search_query(url) else {
        return args;
    };
    let mut args = args;
    let bing = format!(
        "https://www.bing.com/search?q={}",
        urlencode(&query)
    );
    args["url"] = Value::String(bing);
    args
}

/// 从 google/duckduckgo 的搜索 URL 中提取查询词 q。非搜索页返回 None。
fn extract_search_query(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let is_search_host = lower.contains("google.")
        || lower.contains("duckduckgo.com")
        || lower.starts_with("https://www.google")
        || lower.starts_with("https://google");
    if !is_search_host {
        return None;
    }
    let q_idx = lower.find("q=")?;
    let rest = &url[q_idx + 2..];
    let end = rest.find(&['&', '#'][..]).unwrap_or(rest.len());
    let raw = &rest[..end];
    if raw.is_empty() {
        return None;
    }
    Some(url_decode(raw))
}

/// 最小 URL 解码：%XX 与 + → 空格。
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i <bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 <bytes.len() => {
                let h = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]));
                match h {
                    (Some(a), Some(b)) => {
                        out.push(a * 16 + b);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 最小 URL 编码：保留 unreserved，其余按 UTF-8 字节转 %XX，空格转 +。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

const ACTION_ALIASES: &[(&str, &str)] = &[
    ("goto", "navigate"),
    ("open", "navigate"),
    ("visit", "navigate"),
    ("nav", "navigate"),
    ("go_back", "back"),
    ("goback", "back"),
    ("snap", "snapshot"),
    ("a11y", "snapshot"),
    ("screenshot_page", "screenshot"),
    ("shot", "screenshot"),
    ("screenshot", "screenshot"),
    ("eval", "evaluate"),
    ("js", "evaluate"),
    ("execute_js", "evaluate"),
    ("run_js", "evaluate"),
    ("javascript", "evaluate"),
    ("presskey", "press_key"),
    ("key", "press_key"),
    ("keypress", "press_key"),
    ("fill", "fill_form"),
    ("fillform", "fill_form"),
    ("select", "select_option"),
    ("selectoption", "select_option"),
    ("choose", "select_option"),
    ("find_text", "find"),
    ("search", "find"),
    ("wait", "wait_for"),
    ("waitfor", "wait_for"),
    ("upload_file", "upload"),
    ("file_upload", "upload"),
    ("set_files", "upload"),
    ("tabs_list", "tabs"),
    ("list_tabs", "tabs"),
    ("switch_tab", "tabs"),
    ("console_messages", "console"),
    ("logs", "console"),
    ("network_requests", "network"),
    ("network_request", "network_detail"),
    ("request_detail", "network_detail"),
    ("handle_dialog", "dialog"),
    ("accept_dialog", "dialog"),
    ("close_browser", "close"),
];

fn resolve_action(action: &str) -> Option<&'static str> {
    let norm: String = action
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == '-' || c == ' ' { '_' } else { c })
        .collect();
    let canonical = if let Some((_, target)) = ACTION_ALIASES.iter().find(|(a, _)| *a == norm) {
        *target
    } else {
        norm.as_str()
    };
    ACTIONS
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(canonical))
        .map(|(_, tool)| *tool)
}

fn guess_single_field(canonical_action: &str) -> &'static str {
    match canonical_action {
        "browser_navigate" => "url",
        "browser_click" | "browser_hover" => "target",
        "browser_type" => "text",
        "browser_find" => "text",
        "browser_evaluate" | "browser_run_code_unsafe" => "function",
        "browser_press_key" => "key",
        "browser_select_option" => "value",
        "browser_wait_for" => "text",
        "browser_take_screenshot" => "filename",
        _ => "text",
    }
}

fn normalize_params(canonical_action: &str, mut args: Value) -> Value {
    let Some(obj) = args.as_object_mut() else {
        return args;
    };
    const KEY_ALIASES: &[(&str, &str)] = &[
        ("selector", "target"),
        ("element", "target"),
        ("ref", "target"),
        ("node", "target"),
        ("locator", "target"),
        ("value", "text"),
        ("content", "text"),
        ("input", "text"),
        ("query", "text"),
        ("keyword", "text"),
        ("link", "url"),
        ("href", "url"),
        ("address", "url"),
        ("page_url", "url"),
        ("code", "function"),
        ("script", "function"),
        ("js", "function"),
        ("expression", "expression"),
        ("key_name", "key"),
        ("keycode", "key"),
        ("x", "width"),
        ("y", "height"),
    ];
    for (alias, canon) in KEY_ALIASES {
        if obj.contains_key(*canon) {
            continue;
        }
        if let Some(v) = obj.get(*alias).cloned() {
            obj.insert((*canon).to_string(), v);
        }
    }
    let _ = canonical_action;
    args
}

/// 该 action 是否会改变页面/URL（导航、点击、输入、提交、按键等），
/// 会改变时调用后自动补一份无障碍快照。
fn action_changes_page(tool: &str) -> bool {
    matches!(
        tool,
        "browser_navigate"
            | "browser_navigate_back"
            | "browser_click"
            | "browser_type"
            | "browser_fill_form"
            | "browser_select_option"
            | "browser_hover"
            | "browser_press_key"
            | "browser_drag"
            | "browser_drop"
            | "browser_file_upload"
            | "browser_handle_dialog"
    )
}

/// 改变页面后、抓快照前的等待时长。默认 2s，可用
/// LIGONG_BROWSER_SNAPSHOT_DELAY_MS 覆盖（毫秒，0 表示不等待）。
fn snapshot_delay() -> Duration {
    let ms = std::env::var("LIGONG_BROWSER_SNAPSHOT_DELAY_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(2000);
    Duration::from_millis(ms)
}

fn actions_help() -> String {
    let mut s = String::from("可用 action（params 为 JSON 对象字符串）：");
    for (a, tool) in ACTIONS {
        s.push_str(&format!("- {a}  ({tool})"));
        s.push('\n');
    }
    s.push_str("示例：");
    s.push_str("  <parameter name=\"action\">navigate</parameter>");
    s.push_str("  <parameter name=\"params\">{\"url\":\"https://example.com\"}</parameter>");
    s.push_str("常用 params：navigate{url}、click{target}、type{target,text}、find{text}、evaluate{function}。");
    s.push_str("\n（action 与 params 键名均已做宽松匹配：goto/open→navigate，selector/element/ref→target，value/input→text，link/href→url，code/script→function。）");
    s
}

pub struct Browser {
    key: String,
}

impl Browser {
    pub fn new() -> Self {
        Self { key: String::new() }
    }

    /// 按 key 隔离浏览器实例（多项目各用独立浏览器，避免页面互相错乱）。
    pub fn with_key(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}
#[async_trait]
impl Tool for Browser {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "控制可见浏览器（Playwright）：导航、读取页面无障碍快照、点击、输入、填表、执行 JS 等。参数 action + params(JSON)。"
    }

    async fn call(
        &self,
        params: &HashMap<String, String>,
        _live: bool,
        on_output: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        let action = params.get("action").map(|s| s.trim()).unwrap_or("");
        if action.is_empty() {
            return Ok(actions_help());
        }
        let tool = match resolve_action(action) {
            Some(t) => t,
            None => return Ok(format!("[错误] 未知 action: {action}\n\n{}", actions_help())),
        };
        let mut args: Value = match params.get("params").map(|s| s.trim()) {
            Some(s) if !s.is_empty() => match serde_json::from_str::<Value>(s) {
                Ok(v) if v.is_object() => v,
                Ok(v) if v.is_string() => {
                    json!({ guess_single_field(tool): v })
                }
                Ok(v) => return Ok(format!("[错误] params 必须是 JSON 对象，收到: {v}")),
                Err(_) => json!({}),
            },
            _ => json!({}),
        };

        if args.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            let mut direct = serde_json::Map::new();
            for (k, v) in params {
                if k == "action" || k == "params" {
                    continue;
                }
                direct.insert(k.clone(), Value::String(v.clone()));
            }
            if !direct.is_empty() {
                args = Value::Object(direct);
            }
        }

        args = normalize_params(tool, args);
        let args = maybe_redirect_search_to_bing(tool, args);
        let client = client(&self.key).await?;
        let out = client.call_tool(tool, args).await?;
        if let Some(img) = &out.image {
            on_output(&format!("\u{1}IMAGE\u{1}{img}"));
        }
        // 会改变页面/URL 的 action 之后自动补一次无障碍快照，直接以快照结果作为返回，
        // 省去模型单独再发 snapshot。跳转/点击后页面可能仍在加载，先等一小段再抓快照。
        let text = if action_changes_page(tool) {
            tokio::time::sleep(snapshot_delay()).await;
            match client.call_tool("browser_snapshot", json!({})).await {
                Ok(snap) => snap.text,
                Err(_) => out.text,
            }
        } else {
            out.text
        };
        Ok(crate::agent::tool::truncate_output(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_real_mcp_tools() {
        assert_eq!(resolve_action("screenshot"), Some("browser_take_screenshot"));
        assert_eq!(resolve_action("shot"), Some("browser_take_screenshot"));
        assert_eq!(resolve_action("screenshot_page"), Some("browser_take_screenshot"));
        assert_eq!(resolve_action("snap"), Some("browser_snapshot"));
        assert_eq!(resolve_action("close_browser"), Some("browser_close"));
        assert_eq!(resolve_action("goto"), Some("browser_navigate"));
        assert_eq!(resolve_action("navigate"), Some("browser_navigate"));
    }

    #[test]
    fn google_search_redirected_to_bing() {
        let args = json!({"url": "https://www.google.com/search?q=hello+world"});
        let out = maybe_redirect_search_to_bing("browser_navigate", args);
        assert_eq!(out["url"], "https://www.bing.com/search?q=hello+world");
    }

    #[test]
    fn duckduckgo_search_redirected_to_bing() {
        let args = json!({"url": "https://duckduckgo.com/html/?q=rust+lang"});
        let out = maybe_redirect_search_to_bing("browser_navigate", args);
        assert_eq!(out["url"], "https://www.bing.com/search?q=rust+lang");
    }

    #[test]
    fn google_home_not_redirected() {
        let args = json!({"url": "https://www.google.com/"});
        let out = maybe_redirect_search_to_bing("browser_navigate", args);
        assert_eq!(out["url"], "https://www.google.com/");
    }

    #[test]
    fn non_search_action_untouched() {
        let args = json!({"url": "https://www.google.com/search?q=x"});
        let out = maybe_redirect_search_to_bing("browser_snapshot", args);
        assert_eq!(out["url"], "https://www.google.com/search?q=x");
    }

    #[test]
    fn url_decode_handles_percent_and_plus() {
        assert_eq!(url_decode("a%20b+c"), "a b c");
        assert_eq!(url_encode_wrapper("a b c"), "a+b+c");
    }

    fn url_encode_wrapper(s: &str) -> String {
        urlencode(s)
    }
}