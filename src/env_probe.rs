use std::path::PathBuf;

use crate::projects::RemoteSpec;

pub fn probe() -> String {
    let mut lines = Vec::new();

    lines.push("## 本机环境（自动探测，请据此选择正确的命令与路径语法）".to_string());
    lines.push(format!("- 操作系统: {}", os_desc()));
    lines.push(format!("- 架构: {}", std::env::consts::ARCH));
    lines.push(format!("- 家目录: {}", home_desc()));
    lines.push(format!("- 当前工作目录: {}", cwd_desc()));
    lines.push(format!(
        "- 路径分隔符: {}",
        if cfg!(windows) {
            "\\ (反斜杠)"
        } else {
            "/ (正斜杠)"
        }
    ));
    lines.push(format!("- 默认 shell: {}", shell_desc()));

    let mut tools = Vec::new();
    for t in tool_candidates() {
        if let Some(p) = which(t) {
            tools.push(format!("{t}={}", p.display()));
        }
    }
    if tools.is_empty() {
        lines.push("- 关键工具: 未探测到 node/npm/python/git 等".to_string());
    } else {
        lines.push(format!("- 关键工具: {}", tools.join(", ")));
    }

    lines.push(format!("- @playwright/mcp cli.js: {}", mcp_cli_desc()));
    lines.push(format!("- Chromium 可执行: {}", chromium_desc()));

    if cfg!(windows) {
        let shell = shell_desc();
        let lower = shell.to_ascii_lowercase();
        if lower.contains("pwsh") || lower.contains("powershell") {
            lines.push("- 命令语法: Windows + PowerShell。使用 PowerShell cmdlet（Get-ChildItem/Select-String/Get-Content/Invoke-WebRequest 等）；不要用 sh/ls/cat/grep，也不要用 cmd 的 dir/type。多语句用 `;` 或换行，条件用 `if (...) { }`，变量用 `$name`，路径含空格用单引号包裹。".to_string());
        } else {
            lines.push("- 命令语法: Windows + cmd。使用 cmd 语法（dir/type/findstr），不要用 sh/ls/cat/grep 的 Unix 写法；路径含空格用双引号包裹。".to_string());
        }
    } else {
        lines.push("- 命令语法: Unix 环境，可用 sh 语法。".to_string());
    }

    lines.join("\n")
}

/// 生成远程环境描述。
///
/// `is_win` 由调用方异步探测后传入（`probe` 本身保持同步）。
/// 远程场景下只标注远端的 OS/工作目录/路径分隔符/命令语法；
/// 本机的工具路径（node/git/playwright 等）不在此处列出，
/// 因为 read/write/exec 走远端，而 browser/sub_agent 仍在本机。
pub fn probe_remote(remote: Option<&RemoteSpec>, is_win: bool) -> String {
    let Some(spec) = remote else {
        return probe();
    };
    let mut lines = Vec::new();
    lines.push("## 远程环境（SSH，read/write/execute_command 在远端执行）".to_string());
    lines.push(format!(
        "- 连接: {}",
        spec.ssh_alias
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| spec.host.clone())
    ));
    lines.push(format!(
        "- 操作系统: {}",
        if is_win { "Windows" } else { "Linux/Unix" }
    ));
    lines.push(format!("- 工作根目录: {}", spec.root));
    lines.push(format!(
        "- 路径分隔符: {}",
        if is_win { "\\ (反斜杠)" } else { "/ (正斜杠)" }
    ));
    if is_win {
        lines.push("- 命令语法: Windows + PowerShell（Get-ChildItem/Select-String 等），不要用 sh/ls/cat/grep。".to_string());
    } else {
        lines.push("- 命令语法: Unix 环境，可用 sh 语法。".to_string());
    }
    lines.push("- 注意：browser 与 sub_agent 仍在本机运行，文件读写与命令执行在远端。".to_string());
    lines.join("\n")
}

fn os_desc() -> String {
    let os = std::env::consts::OS;
    let ver = std::env::var("OS").ok();
    match ver {
        Some(v) if !v.is_empty() => format!("{os} ({v})"),
        _ => os.to_string(),
    }
}

fn home_desc() -> String {
    dirs::home_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(未知)".into())
}

fn cwd_desc() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(未知)".into())
}

fn shell_desc() -> String {
    let (prog, _) = crate::shell::shell_invocation("");
    prog
}

fn tool_candidates() -> &'static [&'static str] {
    &[
        "node", "npm", "npx", "python3", "python", "git", "cargo", "rustc", "rg", "fd",
    ]
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        if exts.iter().any(|e| e.is_empty()) {
            let cand = dir.join(cmd);
            if cand.is_file() {
                return Some(cand);
            }
        } else {
            for e in &exts {
                let cand = dir.join(format!("{cmd}{e}"));
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

fn mcp_cli_desc() -> String {
    if let Ok(p) = std::env::var("LIGONG_MCP_CLI") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return format!("{} (来自 LIGONG_MCP_CLI)", pb.display());
        }
        return format!("{p} (LIGONG_MCP_CLI 指向但不存在)");
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".ligong-mcp/node_modules/@playwright/mcp/cli.js"));
        #[cfg(windows)]
        {
            candidates.push(home.join("AppData/Roaming/npm/node_modules/@playwright/mcp/cli.js"));
        }
    }
    #[cfg(windows)]
    if let Ok(appdata) = std::env::var("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("npm/node_modules/@playwright/mcp/cli.js"));
    }
    for c in &candidates {
        if c.exists() {
            return c.display().to_string();
        }
    }
    "未找到（浏览器工具不可用，需先安装 @playwright/mcp）".to_string()
}

fn chromium_desc() -> String {
    if let Ok(p) = std::env::var("LIGONG_CHROMIUM") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return pb.display().to_string();
        }
    }
    let Some(home) = dirs::home_dir() else {
        return "未探测".into();
    };
    #[cfg(windows)]
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join("AppData/Local"))
        .join("ms-playwright");
    #[cfg(not(windows))]
    let base = home.join(".cache/ms-playwright");

    if let Ok(entries) = std::fs::read_dir(&base) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("chromium-") {
                for c in [
                    e.path().join("chrome-win/chrome.exe"),
                    e.path().join("chrome-linux64/chrome"),
                    e.path().join("chrome-linux/headless_shell"),
                ] {
                    if c.exists() {
                        return c.display().to_string();
                    }
                }
            }
        }
    }
    "未找到（需 npx playwright install chromium）".to_string()
}