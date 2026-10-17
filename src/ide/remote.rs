//! 远程（SSH）项目支持：能力探测与连接测试。
//!
//! 阶段 0：仅提供"探测本机远程工具链"与"用 ssh -G 校验远程配置"两个只读命令，
//! 不改变任何现有本地项目行为。后续阶段再引入 Backend 分派与远程文件操作。

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::projects::RemoteSpec;

/// 单个外部工具的探测结果。
#[derive(Debug, Clone, Serialize)]
pub struct ToolProbe {
    pub name: String,
    pub found: bool,
    /// 版本首行（截断）。
    pub version: Option<String>,
}

/// 远程工具链探测汇总。
#[derive(Debug, Clone, Serialize)]
pub struct RemoteProbe {
    pub os: String,
    pub tools: Vec<ToolProbe>,
    /// 是否具备最小可用条件（ssh 存在）。
    pub ssh_ready: bool,
}

/// 在 PATH 中查找可执行文件。
fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(cmd);
        if cand.is_file() {
            return Some(cand);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let c = dir.join(format!("{cmd}.{ext}"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// 取工具版本首行（不阻塞过久；失败返回 None）。
fn tool_version(cmd: &str, arg: &str) -> Option<String> {
    let out = std::process::Command::new(cmd)
        .arg(arg)
        .output()
        .ok()?;
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    text.lines().next().map(|s| s.trim().to_string())
}

fn probe_tool(name: &str, version_arg: &str) -> ToolProbe {
    match which(name) {
        Some(_) => ToolProbe {
            name: name.to_string(),
            found: true,
            version: tool_version(name, version_arg),
        },
        None => ToolProbe {
            name: name.to_string(),
            found: false,
            version: None,
        },
    }
}

/// 探测本机远程工具链：ssh / sftp / ssh-keygen / git / rg / tailscale。
#[tauri::command]
pub async fn remote_probe() -> Result<RemoteProbe, String> {
    let tools = vec![
        probe_tool("ssh", "-V"),
        probe_tool("sftp", "-h"),
        probe_tool("ssh-keygen", "-?"),
        probe_tool("git", "--version"),
        probe_tool("rg", "--version"),
        probe_tool("tailscale", "version"),
    ];
    let ssh_ready = tools.iter().any(|t| t.name == "ssh" && t.found);
    Ok(RemoteProbe {
        os: std::env::consts::OS.to_string(),
        tools,
        ssh_ready,
    })
}

/// 连接测试结果。
#[derive(Debug, Clone, Serialize)]
pub struct RemoteTestResult {
    pub ok: bool,
    /// ssh -G 解析出的有效 hostname。
    pub hostname: Option<String>,
    /// 有效 user。
    pub user: Option<String>,
    /// 有效 port。
    pub port: Option<u16>,
    /// 失败原因（ok=false 时）。
    pub error: Option<String>,
}

/// 用 `ssh -G <target>` 校验远程配置并回读 OpenSSH 解析后的有效参数。
///
/// 该调用**不建立连接**，仅让 OpenSSH 解析 ~/.ssh/config 与命令行参数，
/// 因此不会触发认证、也不会阻塞等待密码。用于创建远程项目前的轻量校验。
#[tauri::command]
pub async fn remote_test_connection(spec: RemoteSpec) -> Result<RemoteTestResult, String> {
    if which("ssh").is_none() {
        return Ok(RemoteTestResult {
            ok: false,
            hostname: None,
            user: None,
            port: None,
            error: Some("本机未找到 ssh 可执行文件".into()),
        });
    }
    if spec.host.trim().is_empty() && spec.ssh_alias.as_deref().unwrap_or("").trim().is_empty() {
        return Ok(RemoteTestResult {
            ok: false,
            hostname: None,
            user: None,
            port: None,
            error: Some("host 与 ssh_alias 均为空".into()),
        });
    }

    // 目标：优先别名（复用 config），否则 user@host。
    let target = match spec.ssh_alias.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(alias) => alias.to_string(),
        None => match spec.user.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(u) => format!("{u}@{}", spec.host),
            None => spec.host.clone(),
        },
    };

    let out = std::process::Command::new("ssh")
        .arg("-G")
        .arg(&target)
        .output()
        .map_err(|e| format!("执行 ssh -G 失败: {e}"))?;
    if !out.status.success() {
        return Ok(RemoteTestResult {
            ok: false,
            hostname: None,
            user: None,
            port: None,
            error: Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        });
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut hostname = None;
    let mut user = None;
    let mut port = None;
    for line in text.lines() {
        let mut it = line.splitn(2, char::is_whitespace);
        let key = it.next().unwrap_or("");
        let val = it.next().unwrap_or("").trim();
        match key {
            "hostname" => hostname = Some(val.to_string()),
            "user" => user = Some(val.to_string()),
            "port" => port = val.parse::<u16>().ok(),
            _ => {}
        }
    }
    Ok(RemoteTestResult {
        ok: hostname.is_some(),
        hostname,
        user,
        port,
        error: None,
    })
}

/// 获取远程主机公钥指纹（首次连接 TOFU 确认用）。
#[tauri::command]
pub async fn remote_host_key(spec: RemoteSpec) -> Result<Vec<crate::ide::ssh::HostKey>, String> {
    crate::ide::ssh::fetch_host_key(&spec).await
}

/// 远程执行一条命令（阶段 1 自测/后续 Backend 复用）。返回退出码与输出。
#[tauri::command]
pub async fn remote_exec(spec: RemoteSpec, command: String) -> Result<RemoteExecOutput, String> {
    let out = crate::ide::ssh::exec(&spec, &command).await?;
    Ok(RemoteExecOutput {
        code: out.code,
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteExecOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// 设置项目的远程规格（将其标记为远程项目）。仅存非秘密配置。
#[tauri::command]
pub async fn set_project_remote(
    app: AppHandle,
    project_id: String,
    spec: RemoteSpec,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<(), String> {
    let root = spec.root.clone();
    {
        let mut inner = state.inner.lock().await;
        inner.projects.set_remote_of(&project_id, spec);
        inner.projects.save()?;
        // 清掉已广播的工作目录缓存，让下一轮 agent 重新注入新 root。
        if let Some(ctx) = inner.running.get_mut(&project_id) {
            ctx.session.announced_workdir = None;
        }
    }
    // 广播给主窗口，使 GUI 顶栏工作目录实时跟随（IDE 里切换远程 root 走这里）。
    let _ = app.emit(
        "agent://workdir-changed",
        serde_json::json!({ "project_id": project_id, "path": root }),
    );
    Ok(())
}

/// 读取项目的远程规格；None=本地项目。
#[tauri::command]
pub async fn get_project_remote(
    project_id: String,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<Option<RemoteSpec>, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.remote_of(&project_id))
}

/// 清除项目的远程规格（转回本地项目）。
#[tauri::command]
pub async fn clear_project_remote(
    project_id: String,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    inner.projects.clear_remote_of(&project_id);
    inner.projects.save()
}

/// 把"远程绝对路径或相对 root 的 rel"解析为规范化 rel。
///
/// 先按绝对路径尝试（必须落在 root 下），失败则当 rel 透传。
/// 返回的 rel 统一用 `/` 分隔、不带前导 `/`，可直接喂给 win_resolve/posix_resolve。
#[tauri::command]
pub async fn remote_resolve_path(
    project_id: String,
    path: String,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<String, String> {
    let spec = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .remote_of(&project_id)
            .ok_or_else(|| "当前项目不是远程项目".to_string())?
    };
    let p = path.trim();
    if p.is_empty() {
        return Err("路径为空".to_string());
    }
    // 先按绝对路径（在 root 下）；成功且非 root 本身则直接用。
    if let Ok(rel) = crate::ide::backend::to_rel(&spec, p) {
        if !rel.is_empty() {
            return Ok(rel);
        }
    }
    // 当 rel 用：统一分隔符、去掉前导斜杠（交给 resolve 再校验越界）。
    Ok(p.replace('\\', "/").trim_start_matches('/').to_string())
}

/// SSH config 中的一条主机记录。
#[derive(Debug, Clone, Serialize)]
pub struct SshHost {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

/// 解析 ~/.ssh/config 的 Host 条目（跳过通配）。
#[tauri::command]
pub async fn remote_ssh_hosts() -> Result<Vec<SshHost>, String> {
    let path = dirs::home_dir()
        .ok_or("无法解析 HOME")?
        .join(".ssh")
        .join("config");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };
    let mut hosts: Vec<SshHost> = Vec::new();
    let mut cur: Option<SshHost> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut it = line.splitn(2, char::is_whitespace);
        let key = it.next().unwrap_or("").to_lowercase();
        let val = it.next().unwrap_or("").trim().to_string();
        match key.as_str() {
            "host" => {
                if let Some(h) = cur.take() { hosts.push(h); }
                if !val.contains('*') && !val.contains('?') && !val.is_empty() {
                    cur = Some(SshHost { alias: val.clone(), hostname: val, user: None, port: None });
                }
            }
            "hostname" => if let Some(h) = cur.as_mut() { h.hostname = val; },
            "user" => if let Some(h) = cur.as_mut() { h.user = Some(val); },
            "port" => if let Some(h) = cur.as_mut() { h.port = val.parse().ok(); },
            _ => {}
        }
    }
    if let Some(h) = cur.take() { hosts.push(h); }
    Ok(hosts)
}

/// Tailscale 设备。
#[derive(Debug, Clone, Serialize)]
pub struct TsDevice {
    pub name: String,
    pub dns_name: String,
    pub ip: String,
    pub online: bool,
    pub os: String,
    pub is_self: bool,
}

/// 通过 `tailscale status --json` 列出 tailnet 设备。
#[tauri::command]
pub async fn remote_tailscale_devices() -> Result<Vec<TsDevice>, String> {
    let out = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .map_err(|e| format!("tailscale 不可用: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("解析失败: {e}"))?;

    fn parse(v: &serde_json::Value, is_self: bool) -> TsDevice {
        let dns = v["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_string();
        let ip = v["TailscaleIPs"].as_array()
            .and_then(|a| a.iter().find_map(|x| x.as_str()).map(String::from))
            .unwrap_or_default();
        TsDevice {
            name: v["HostName"].as_str().unwrap_or("").to_string(),
            dns_name: dns,
            ip,
            online: v["Online"].as_bool().unwrap_or(false) || is_self,
            os: v["OS"].as_str().unwrap_or("").to_string(),
            is_self,
        }
    }
    let mut list = Vec::new();
    if let Some(s) = v.get("Self") { list.push(parse(s, true)); }
    if let Some(peers) = v.get("Peer").and_then(|p| p.as_object()) {
        for (_, p) in peers { list.push(parse(p, false)); }
    }
    Ok(list)
}

/// 远程目录浏览结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BrowseResult {
    /// 当前目录的绝对路径（后端真实解析结果）。
    pub path: String,
    /// 子目录绝对路径列表。
    pub dirs: Vec<String>,
}

/// 浏览远程目录（返回子目录绝对路径，供选择根目录）。自动适配 Linux/Windows。
#[tauri::command]
pub async fn remote_browse(
    spec: RemoteSpec,
    path: String,
) -> Result<BrowseResult, String> {
    let p = path.trim();
    if crate::ide::backend::is_windows_remote(&spec).await {
        let arg = if p.is_empty() {
            "$env:USERPROFILE".to_string()
        } else {
            format!("'{}'", p.replace('\'', "''"))
        };
        let script = format!(
            "$ErrorActionPreference='Stop'; $p = {arg}; if (-not $p) {{ $p = $env:USERPROFILE }}; \
             Set-Location -LiteralPath $p; (Get-Location).Path; \
             Get-ChildItem -LiteralPath $p -Directory -Force | ForEach-Object {{ $_.FullName }}"
        );
        let out = crate::ide::ssh::exec_ps(&spec, &script, std::time::Duration::from_secs(20)).await?;
        if !out.ok() { return Err(format!("浏览失败: {}", out.stderr.trim())); }
        let mut lines = out.stdout.lines();
        let base = lines.next().unwrap_or("").trim().to_string();
        return Ok(BrowseResult {
            path: base,
            dirs: lines.map(|l| l.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        });
    }
    let dir = if p.is_empty() {
        match spec.user.as_deref().filter(|u| !u.is_empty()) {
            Some(u) => format!("/home/{u}"),
            None => "~".to_string(),
        }
    } else { p.to_string() };
    let cmd = format!(
        "cd {} 2>/dev/null && pwd && ls -d */ 2>/dev/null",
        crate::ide::backend::shell_quote(&dir)
    );
    let out = crate::ide::ssh::exec_timeout(&spec, &cmd, std::time::Duration::from_secs(20)).await?;
    if !out.ok() { return Err(format!("浏览失败: {}", out.stderr.trim())); }
    let mut lines = out.stdout.lines();
    let base = lines.next().unwrap_or("").trim().to_string();
    let mut dirs = Vec::new();
    for l in lines {
        let name = l.trim().trim_end_matches('/');
        if name.is_empty() { continue; }
        dirs.push(if base.is_empty() { name.to_string() } else { format!("{base}/{name}") });
    }
    Ok(BrowseResult { path: base, dirs })
}

/// 远程交互终端的启动参数（供前端 term_open 使用）。
#[derive(Debug, Clone, Serialize)]
pub struct TerminalArgv {
    /// 要运行的程序（当前固定 ssh；后续可扩展 tailscale ssh）。
    pub prog: String,
    /// 透传给 prog 的参数。
    pub args: Vec<String>,
    /// 终端标签（主机别名或地址）。
    pub label: String,
    /// 远端工作根目录（仅用于展示，PTY 本地 cwd 对远端无效）。
    pub root: String,
}

/// 为一个远程项目构造交互式 ssh 终端参数。
///
/// 复用 `ssh::terminal_argv`（去 BatchMode + `-tt` + 远端 cd 到 root）。
/// 本地项目返回错误。
#[tauri::command]
pub async fn remote_terminal_argv(
    project_id: String,
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<TerminalArgv, String> {
    let spec = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .remote_of(&project_id)
            .ok_or_else(|| "当前项目不是远程项目".to_string())?
    };
    let (mut args, label) = crate::ide::ssh::terminal_argv(&spec);
    // Windows 远端：POSIX 的 `cd/exec $SHELL -l` 会被 PowerShell 解析而失败，
    // 改为让远端起一个交互 PowerShell（pwsh 优先，回退 powershell）。
    if crate::ide::backend::is_windows_remote(&spec).await {
        args.pop(); // 丢弃 POSIX 远端命令
        args.push(windows_terminal_remote_cmd(&spec.root));
    }
    Ok(TerminalArgv {
        prog: "ssh".to_string(),
        args,
        label,
        root: spec.root.clone(),
    })
}

/// PowerShell 单引号字面量转义（内部单引号翻倍）。
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Windows 远端的 ssh 登录命令：切到 root 后进入交互 PowerShell（pwsh 优先）。
fn windows_terminal_remote_cmd(root: &str) -> String {
    let mut cmd = String::new();
    let root = root.trim();
    if !root.is_empty() {
        cmd.push_str(&format!(
            "Set-Location -LiteralPath {} -ErrorAction SilentlyContinue; ",
            ps_quote(root)
        ));
    }
    cmd.push_str("if (Get-Command pwsh -ErrorAction SilentlyContinue) { pwsh } else { powershell }");
    cmd
}

// —— 独立 SSH 文件浏览器（不依赖项目）——
// 约定：spec.root 即"当前浏览目录"的绝对路径，rel 传空。

/// `ssh_list_dir` 的返回：真实解析后的当前目录路径 + 条目。
///
/// 前端传空 root 时，后端会用 `remote_browse` 探测默认目录，
/// 并把真实路径回传，前端据此同步 `curAbs`。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SshListResult {
    pub path: String,
    pub entries: Vec<crate::ide::fs::DirEntry>,
}

#[tauri::command]
pub async fn ssh_list_dir(spec: RemoteSpec, rel: String) -> Result<SshListResult, String> {
    // root 为空：用 remote_browse 探测远端默认目录（Windows: USERPROFILE；POSIX: ~/home/<user>）。
    let mut spec = spec;
    if spec.root.trim().is_empty() {
        let p = remote_browse(spec.clone(), String::new())
            .await
            .map_err(|e| format!("无法确定默认目录：{e}"))?;
        if p.path.trim().is_empty() {
            return Err("远端未能确定默认目录，请手动输入路径".to_string());
        }
        spec.root = p.path;
    }
    let entries = crate::ide::backend::list_dir(&spec, &rel).await?;
    Ok(SshListResult {
        path: spec.root,
        entries,
    })
}

#[tauri::command]
pub async fn ssh_create(spec: RemoteSpec, rel: String, is_dir: bool) -> Result<(), String> {
    crate::ide::backend::create(&spec, &rel, is_dir).await
}

#[tauri::command]
pub async fn ssh_delete(spec: RemoteSpec, rel: String) -> Result<(), String> {
    crate::ide::backend::delete(&spec, &rel).await
}

#[tauri::command]
pub async fn ssh_rename(
    spec: RemoteSpec,
    from_rel: String,
    to_rel: String,
) -> Result<(), String> {
    crate::ide::backend::rename(&spec, &from_rel, &to_rel).await
}

/// 移动远程文件/目录到目标目录（绝对路径入参；用远端命令，支持目录）。
#[tauri::command]
pub async fn ssh_move(
    spec: RemoteSpec,
    from_abs: String,
    to_dir_abs: String,
) -> Result<(), String> {
    let sep = if from_abs.contains('\\') || from_abs.as_bytes().get(1) == Some(&b':') {
        "\\"
    } else {
        "/"
    };
    let name = from_abs
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return Err("源路径无效".into());
    }
    let to_abs = format!(
        "{}{}{}",
        to_dir_abs.trim_end_matches(['/', '\\']),
        sep,
        name
    );
    if from_abs == to_abs {
        return Ok(());
    }
    if crate::ide::backend::is_windows_remote(&spec).await {
        let script = format!(
            "if (-not (Test-Path -LiteralPath {f})) {{ [Console]::Error.Write('源不存在'); exit 2 }}; \
             Move-Item -LiteralPath {f} -Destination {t} -Force",
            f = crate::ide::backend::ps_lit(&from_abs),
            t = crate::ide::backend::ps_lit(&to_abs),
        );
        let out = crate::ide::ssh::exec_ps(&spec, &script, std::time::Duration::from_secs(60)).await?;
        if !out.ok() {
            return Err(format!("移动失败：{}", out.stderr.trim()));
        }
    } else {
        let cmd = format!(
            "mv -f {} {}",
            crate::ide::backend::shell_quote(&from_abs),
            crate::ide::backend::shell_quote(&to_abs)
        );
        let out = crate::ide::ssh::exec_timeout(&spec, &cmd, std::time::Duration::from_secs(60)).await?;
        if !out.ok() {
            return Err(format!("移动失败：{}", out.stderr.trim()));
        }
    }
    Ok(())
}

/// 读取远程二进制文件（base64），供 SSH 面板预览图片/PDF。
#[tauri::command]
pub async fn ssh_read_binary_b64(spec: RemoteSpec, abs: String) -> Result<String, String> {
    let (root, rel) = split_abs(&abs)?;
    let mut s = spec;
    s.root = root;
    let (bytes, _) = crate::ide::backend::read_binary(&s, &rel).await?;
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

#[tauri::command]
pub async fn ssh_download(
    spec: RemoteSpec,
    rel: String,
    local_path: String,
) -> Result<(), String> {
    let (bytes, _) = crate::ide::backend::read_binary(&spec, &rel).await?;
    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&local_path, &bytes).map_err(|e| format!("写入本地文件失败：{e}"))
}

#[tauri::command]
pub async fn ssh_upload(
    spec: RemoteSpec,
    local_path: String,
    rel_dest: String,
) -> Result<(), String> {
    const MAX_XFER: u64 = 64 * 1024 * 1024;
    let meta = std::fs::metadata(&local_path).map_err(|e| format!("读取本地文件失败：{e}"))?;
    if meta.len() > MAX_XFER {
        return Err(format!(
            "文件过大（{} 字节，上限 {MAX_XFER} 字节）",
            meta.len()
        ));
    }
    let bytes = std::fs::read(&local_path).map_err(|e| format!("读取本地文件失败：{e}"))?;
    crate::ide::transfer::write_remote_binary(&spec, &rel_dest, &bytes).await
}

/// 读取远程文件文本（供 SSH 面板编辑器用）。
#[tauri::command]
pub async fn ssh_read_text(spec: RemoteSpec, abs: String) -> Result<String, String> {
    // abs 是绝对路径；转成 (root=父目录, rel=文件名) 交给 backend。
    let (root, rel) = split_abs(&abs)?;
    let mut s = spec;
    s.root = root;
    crate::ide::backend::read_file(&s, &rel).await
}

/// 写远程文件文本（供 SSH 面板编辑器保存）。
#[tauri::command]
pub async fn ssh_write_text(
    spec: RemoteSpec,
    abs: String,
    content: String,
) -> Result<(), String> {
    let (root, rel) = split_abs(&abs)?;
    let mut s = spec;
    s.root = root;
    crate::ide::backend::write_file(&s, &rel, &content).await
}

/// 拆分绝对路径为 (父目录, 文件名)，跨平台。
fn split_abs(abs: &str) -> Result<(String, String), String> {
    let p = abs.trim();
    if p.is_empty() {
        return Err("路径为空".into());
    }
    let win = p.as_bytes().get(1) == Some(&b':') || p.contains('\\');
    let sep = if win { '\\' } else { '/' };
    let norm = p.replace(['\\', '/'], &sep.to_string());
    match norm.rfind(sep) {
        Some(i) if i > 0 => {
            let root = norm[..i].to_string();
            let name = norm[i + 1..].to_string();
            if name.is_empty() {
                return Err("路径无效".into());
            }
            Ok((root, name))
        }
        _ => Err(format!("无法拆分路径：{abs}")),
    }
}

/// 按 RemoteSpec 直接构造终端参数（供独立 SSH 面板用，不依赖项目）。
#[tauri::command]
pub async fn ssh_terminal_argv(spec: RemoteSpec) -> Result<TerminalArgv, String> {
    let (mut args, label) = crate::ide::ssh::terminal_argv(&spec);
    if crate::ide::backend::is_windows_remote(&spec).await {
        args.pop();
        args.push(windows_terminal_remote_cmd(&spec.root));
    }
    Ok(TerminalArgv {
        prog: "ssh".to_string(),
        args,
        label,
        root: spec.root.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_terminal_cmd_no_posix() {
        let c = windows_terminal_remote_cmd("C:\\Users\\z\\Desktop");
        assert!(c.contains("Set-Location -LiteralPath 'C:\\Users\\z\\Desktop'"), "{c}");
        assert!(c.contains("Get-Command pwsh"), "{c}");
        assert!(!c.contains("exec "), "不应含 POSIX exec: {c}");
        assert!(!c.contains("/dev/null"), "{c}");
    }

    #[test]
    fn windows_terminal_cmd_empty_root() {
        let c = windows_terminal_remote_cmd("   ");
        assert!(!c.contains("Set-Location"), "{c}");
        assert!(c.contains("powershell"), "{c}");
    }

    /// 真机：Windows 远端（gongwei）终端登录命令的探测版能正常执行。
    #[tokio::test]
    #[ignore]
    async fn terminal_cmd_windows_remote() {
        let spec = crate::projects::RemoteSpec {
            host: "gongwei".into(),
            user: None,
            port: None,
            root: "C:\\Users\\z".into(),
            ssh_alias: Some("gongwei".into()),
            proxy_jump: None,
            restrict: false,
        };
        assert!(crate::ide::backend::is_windows_remote(&spec).await);
        // 探测版：把末尾“进入 pwsh”替换为输出标记，验证 Set-Location 语法可执行。
        let cmd = windows_terminal_remote_cmd(&spec.root)
            .replace("if (Get-Command pwsh -ErrorAction SilentlyContinue) { pwsh } else { powershell }", "'win-term-ok'");
        let out = crate::ide::ssh::exec_timeout(&spec, &cmd, std::time::Duration::from_secs(30))
            .await
            .unwrap();
        assert!(out.ok(), "执行失败: {}", out.stderr);
        assert!(out.stdout.contains("win-term-ok"), "stdout={}", out.stdout);
    }
}