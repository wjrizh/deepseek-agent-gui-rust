//! NapCat（QQ 机器人框架）的自动安装、注入与启动。
//!
//! Linux：下载 NapCat.Shell，注入本机 QQ（/opt/QQ），用 xvfb-run 启动，
//!        二维码显示在独立终端窗口。
//! Windows：使用官方一键包（内置 QQ），用 cmd 启动。
//!
//! 目录约定：
//!   ~/.ligong/napcat/            NapCat 工作根目录
//!   ~/.ligong/napcat/config/     配置（onebot11 等，端口 3000/3001）
//!   ~/.ligong/napcat/linux/      Linux Shell 解压目录
//!   ~/.ligong/napcat/win/        Windows 一键包解压目录
//! 下载走 GitHub 镜像回退。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

const SHELL_URL: &str =
    "https://github.com/NapNeko/NapCatQQ/releases/download/v4.18.28/NapCat.Shell.zip";
const WIN_ONEKEY_URL: &str =
    "https://github.com/NapNeko/NapCatQQ/releases/download/v4.18.28/NapCat.Shell.Windows.OneKey.zip";

/// GitHub 加速镜像前缀。
const MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://ghproxy.net/",
    "https://github.moeyy.xyz/",
];

/// 默认 QQ 安装目录（Linux）。
const DEFAULT_QQ_DIR: &str = "/opt/QQ";

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn napcat_root() -> PathBuf {
    home().join(".ligong").join("napcat")
}

/// NapCat 注入版 QQ 独立的用户数据目录（避免与正常 QQ 冲突）。
pub fn qq_data_dir() -> PathBuf {
    napcat_root().join("qq-data")
}

/// NapCat 实际读取的配置目录（位于解压目录内）。
pub fn config_dir() -> PathBuf {
    if cfg!(windows) {
        win_dir().join("config")
    } else {
        linux_dir().join("config")
    }
}

pub fn linux_dir() -> PathBuf {
    napcat_root().join("linux")
}

pub fn win_dir() -> PathBuf {
    napcat_root().join("win")
}

/// NapCat 工作是否已就绪（本体已下载解压）。
pub fn is_installed() -> bool {
    if cfg!(windows) {
        win_dir().join("napcat.bat").exists() || win_dir().join("NapCatInstaller.exe").exists()
    } else {
        linux_dir().join("napcat.mjs").exists()
    }
}

/// 组装带镜像回退的下载 URL 列表。
fn mirror_urls(origin: &str) -> Vec<String> {
    let mut v = Vec::new();
    for m in MIRRORS {
        v.push(format!("{m}{origin}"));
    }
    v.push(origin.to_string());
    v
}

/// 用 curl 下载 url 到 dest，失败自动尝试镜像。
async fn download(origin: &str, dest: &Path) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut last_err = String::new();
    for url in mirror_urls(origin) {
        let status = Command::new("curl")
            .arg("-fSL")
            .arg("--retry")
            .arg("2")
            .arg("--connect-timeout")
            .arg("20")
            .arg("--max-time")
            .arg("600")
            .arg("-o")
            .arg(dest)
            .arg(&url)
            .stdin(Stdio::null())
            .status()
            .await
            .map_err(|e| format!("无法执行 curl：{e}"))?;
        if status.success() && dest.exists() {
            let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            if size > 1024 {
                return Ok(());
            }
        }
        last_err = format!("下载失败：{url}");
    }
    Err(last_err)
}

/// 解压 zip 到目标目录（用系统 unzip）。
async fn unzip(zip: &Path, dest: &Path) -> Result<(), String> {    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;    #[cfg(windows)]    let mut cmd = {        let ps = format!("Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force", zip.display(), dest.display());        let mut c = Command::new("powershell");        c.arg("-NoProfile").arg("-NonInteractive").arg("-Command").arg(ps);        c    };    #[cfg(not(windows))]    let mut cmd = {        let mut c = Command::new("unzip");        c.arg("-o").arg("-q").arg(zip).arg("-d").arg(dest);        c    };    let status = cmd        .stdin(Stdio::null())        .status()        .await        .map_err(|e| format!("无法执行解压命令：{e}"))?;    if status.success() { Ok(()) } else { Err(format!("解压失败：{}", zip.display())) }}

/// 确保 Linux NapCat 本体已安装（下载 + 解压）。
pub async fn ensure_linux_body() -> Result<(), String> {
    let dir = linux_dir();
    if dir.join("napcat.mjs").exists() {
        return Ok(());
    }
    let zip = napcat_root().join("NapCat.Shell.zip");
    download(SHELL_URL, &zip).await?;
    unzip(&zip, &dir).await?;
    let _ = std::fs::remove_file(&zip);
    if !dir.join("napcat.mjs").exists() {
        return Err("解压后未找到 napcat.mjs".into());
    }
    Ok(())
}

/// 确保 Windows NapCat 一键包已安装。
pub async fn ensure_win_body() -> Result<(), String> {
    let dir = win_dir();
    if dir.join("NapCatInstaller.exe").exists() || dir.join("napcat.bat").exists() {
        return Ok(());
    }
    let zip = napcat_root().join("NapCat.Shell.Windows.OneKey.zip");
    download(WIN_ONEKEY_URL, &zip).await?;
    unzip(&zip, &dir).await?;
    let _ = std::fs::remove_file(&zip);
    Ok(())
}

/// 写入预置配置（onebot11 端口 3000/3001 + napcat.json）。
pub fn write_default_config() -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let napcat_json = dir.join("napcat.json");
    if !napcat_json.exists() {
        std::fs::write(&napcat_json, DEFAULT_NAPCAT_JSON).map_err(|e| e.to_string())?;
    }

    let onebot = dir.join("onebot11.json");
    if !onebot.exists() {
        std::fs::write(&onebot, DEFAULT_ONEBOT_JSON).map_err(|e| e.to_string())?;
    }
    Ok(())
}

const DEFAULT_NAPCAT_JSON: &str = r#"{
  "fileLog": false,
  "consoleLog": true,
  "fileLogLevel": "debug",
  "consoleLogLevel": "info",
  "packetBackend": "auto",
  "packetServer": "",
  "o3HookMode": 1
}"#;

const DEFAULT_ONEBOT_JSON: &str = r#"{
  "network": {
    "httpServers": [
      {
        "enable": true,
        "name": "agent-http",
        "host": "0.0.0.0",
        "port": 3000,
        "token": "",
        "enableHeart": false,
        "enablePost": false,
        "messagePostFormat": "array",
        "debug": false
      }
    ],
    "httpSseServers": [],
    "httpClients": [],
    "websocketServers": [
      {
        "enable": true,
        "name": "agent",
        "host": "0.0.0.0",
        "port": 3001,
        "reportSelfMessage": false,
        "enableForcePushEvent": true,
        "messagePostFormat": "array",
        "token": "",
        "debug": false,
        "heartInterval": 30000
      }
    ],
    "websocketClients": [],
    "plugins": []
  },
  "musicSignUrl": "",
  "enableLocalFile2Url": false,
  "parseMultMsg": false,
  "imageDownloadProxy": "",
  "timeout": {
    "baseTimeout": 10000,
    "uploadSpeedKBps": 256,
    "downloadSpeedKBps": 256,
    "maxTimeout": 1800000
  }
}"#;

// ============================ Linux 注入与启动 ============================

fn qq_app_dir() -> PathBuf {
    PathBuf::from(std::env::var("LIGONG_QQ_DIR").unwrap_or_else(|_| DEFAULT_QQ_DIR.into()))
        .join("resources")
        .join("app")
}

/// 检测本机 Linux QQ 是否可用。
pub fn detect_linux_qq() -> Option<PathBuf> {
    let app = qq_app_dir();
    if app.join("package.json").exists() {
        return Some(app);
    }
    None
}

/// 生成注入加载器内容（指向 NapCat 的 napcat.mjs）。
fn load_napcat_js(napcat_mjs: &Path) -> String {
    format!(
        r#"const fs = require("fs");
const path = require("path");
const CurrentPath = path.dirname(__filename);
const hasNapcatParam = process.argv.includes("--no-sandbox");
if (hasNapcatParam) {{
  (async () => {{
    await import("file://{}");
  }})();
}} else {{
  require("./application.asar/app_launcher/index.js");
}}
"#,
        napcat_mjs.display()
    )
}

/// 生成修改后的 QQ package.json 内容（main 指向注入器）。
fn patched_package_json(app_dir: &Path) -> Result<String, String> {
    let pkg = app_dir.join("package.json");
    let raw = std::fs::read_to_string(&pkg).map_err(|e| format!("读取 QQ package.json 失败：{e}"))?;
    let mut v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 QQ package.json 失败：{e}"))?;
    v["main"] = serde_json::Value::String("./loadNapCat.cjs".into());
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

/// 执行 Linux 注入：写 loadNapCat.cjs + 改 package.json。通过 pkexec 提权。
pub fn inject_linux() -> Result<(), String> {
    let app = detect_linux_qq().ok_or_else(|| {
        format!(
            "未检测到 Linux QQ（{}）。请先安装 Linux 版 QQ。",
            qq_app_dir().display()
        )
    })?;
    let napcat_mjs = linux_dir().join("napcat.mjs");
    if !napcat_mjs.exists() {
        return Err("NapCat 本体未安装，请先下载解压".into());
    }

    // 已注入则跳过。
    let pkg = app.join("package.json");
    if let Ok(raw) = std::fs::read_to_string(&pkg) {
        if raw.contains("\"./loadNapCat.cjs\"") {
            return Ok(());
        }
    }

    // 在临时目录生成待写入内容。
    let tmp = std::env::temp_dir().join(format!("ligong-napcat-inject-{}", std::process::id()));
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let loader_src = tmp.join("loadNapCat.cjs");
    let pkg_src = tmp.join("package.json");
    std::fs::write(&loader_src, load_napcat_js(&napcat_mjs)).map_err(|e| e.to_string())?;
    std::fs::write(&pkg_src, patched_package_json(&app)?).map_err(|e| e.to_string())?;

    // 备份原 package.json（若尚未备份）。
    let bak = app.join("package.json.ligong.bak");
    let app_dir = app.display().to_string();
    let script = format!(
        "set -e; \
         if [ ! -f '{bak}' ]; then cp '{pkg}' '{bak}'; fi; \
         cp '{loader}' '{app}/loadNapCat.cjs'; \
         cp '{newpkg}' '{pkg}'",
        bak = bak.display(),
        pkg = pkg.display(),
        loader = loader_src.display(),
        newpkg = pkg_src.display(),
        app = app_dir,
    );

    let status = std::process::Command::new("pkexec")
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .status()
        .map_err(|e| format!("执行 pkexec 失败：{e}"))?;
    if !status.success() {
        return Err("提权注入失败（已取消或密码错误）".into());
    }

    // 校验注入结果。
    if let Ok(raw) = std::fs::read_to_string(&pkg) {
        if raw.contains("\"./loadNapCat.cjs\"") {
            return Ok(());
        }
    }
    Err("注入后校验失败".into())
}

/// 检测已登录过的 QQ 号（通过 NapCat 生成的 napcat_<uin>.json 判断）。
pub fn detect_logged_in_uin() -> Option<String> {
    let dir = config_dir();
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("napcat_") else { continue };
        let Some(uin) = rest.strip_suffix(".json") else { continue };
        if !uin.is_empty() && uin.chars().all(|c| c.is_ascii_digit()) {
            return Some(uin.to_string());
        }
    }
    None
}

/// 启动 Linux NapCat：终端里用 xvfb-run 跑 QQ（带注入）。
pub fn launch_linux_terminal() -> Result<(), String> {
    let app = qq_app_dir();
    let root = app
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| "无法定位 QQ 根目录".to_string())?;
    let qq_bin = root.join("qq");
    if !qq_bin.exists() {
        return Err(format!("QQ 可执行文件不存在：{}", qq_bin.display()));
    }

    let napcat_dir = linux_dir();
    let qq_data = qq_data_dir();
    // 若曾登录过，带上 -q <uin>，让 NapCat 走快速登录（免扫码）。
    let quick = match detect_logged_in_uin() {
        Some(uin) => format!(" -q {}", uin),
        None => String::new(),
    };
    let inner = format!(
        "cd '{}' && HOME='{}' xvfb-run -a '{}' --no-sandbox --user-data-dir='{}'{}; echo; echo '[NapCat] 进程已退出，按回车关闭'; read _",
        napcat_dir.display(),
        qq_data.display(),
        qq_bin.display(),
        qq_data.display(),
        quick
    );
    // 依次尝试常见终端，避免因缺少某个终端而失败。
    let candidates: &[(&str, &[&str])] = &[
        ("alacritty", &["-e", "sh", "-c"]),
        ("x-terminal-emulator", &["-e", "sh", "-c"]),
        ("gnome-terminal", &["--", "sh", "-c"]),
        ("konsole", &["-e", "sh", "-c"]),
        ("xterm", &["-e", "sh", "-c"]),
    ];
    let mut last_err = String::from("未找到可用终端（请安装 alacritty 或 xterm）");
    for (bin, args) in candidates {
        if std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {}", bin))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|st| !st.success())
            .unwrap_or(true)
        {
            continue;
        }
        let mut cmd = Command::new(bin);
        for a in *args {
            cmd.arg(a);
        }
        let child = cmd
            .arg(&inner)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(_) => return Ok(()),
            Err(e) => last_err = format!("启动终端 {} 失败：{e}", bin),
        }
    }
    Err(last_err)
}

// ============================ Windows 启动 ============================

/// 启动 Windows NapCat（cmd 新窗口）。
pub fn launch_win_terminal() -> Result<(), String> {
    let dir = win_dir();
    let bat = if dir.join("napcat.bat").exists() {
        dir.join("napcat.bat")
    } else {
        return Err("Windows NapCat 未就绪（缺少 napcat.bat）".into());
    };
    // 若曾登录过，带上 -q <uin>，让 NapCat 走快速登录（免扫码）。
    let quick = match detect_logged_in_uin() {
        Some(uin) => format!(" -q {}", uin),
        None => String::new(),
    };
    // cmd /k 对带引号的命令需要外层再包一层引号，否则解析错误直接退出。
    // 形式：cmd /k ""C:\path\napcat.bat" -q 123"
    let inner = format!("\"{}\"{}", bat.display(), quick);
    let child = Command::new("cmd")
        .arg("/k")
        .arg(format!("\"{inner}\""))
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("启动 cmd 失败：{e}")),
    }
}

/// 跨平台：确保 NapCat 就绪（下载 + 配置 + 注入），并启动终端。
pub async fn ensure_and_launch() -> Result<(), String> {
    write_default_config()?;
    if cfg!(windows) {
        ensure_win_body().await?;
        launch_win_terminal()
    } else {
        ensure_linux_body().await?;
        inject_linux()?;
        launch_linux_terminal()
    }
}