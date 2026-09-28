//! 跨平台 shell 调用封装。
//!
//! Windows 默认优先 PowerShell（pwsh.exe → powershell.exe），回退 cmd。
//! 可用 LIGONG_SHELL 覆盖为任意 shell 路径（bash/pwsh/cmd/...）。

use std::io::Write;
use std::path::PathBuf;

/// 写临时脚本文件并返回路径（Windows 用 .ps1，Unix 用 .sh）。
pub fn write_temp_script(cmd: &str) -> Option<PathBuf> {
    let ext = if cfg!(windows) { "ps1" } else { "sh" };
    let mut path = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!("ligong_cmd_{}_{stamp}.{ext}", std::process::id()));
    let mut file = std::fs::File::create(&path).ok()?;
    if cfg!(windows) {
        file.write_all(b"\xEF\xBB\xBF").ok()?;
        file.write_all(
            "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8\n\
             $OutputEncoding=[System.Text.Encoding]::UTF8\n\
             chcp 65001 > $null\n"
                .as_bytes(),
        )
        .ok()?;
    }
    file.write_all(cmd.as_bytes()).ok()?;
    if !cmd.ends_with('\n') {
        file.write_all(b"\n").ok()?;
    }
    file.flush().ok()?;
    Some(path)
}

/// 返回 (可执行文件, 参数)。script 非空时走临时脚本模式。
pub fn shell_invocation_ex(cmd: &str, script: Option<&PathBuf>) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        if let Ok(shell) = std::env::var("LIGONG_SHELL") {
            let s = shell.trim();
            if !s.is_empty() {
                return (s.to_string(), args_for_shell(s, cmd));
            }
        }
        if let Some(ps) = detect_powershell() {
            let is_ps = ps.contains("pwsh") || ps.contains("powershell");
            if is_ps {
                if let Some(path) = script {
                    return (
                        ps,
                        vec![
                            "-NoProfile".into(),
                            "-ExecutionPolicy".into(),
                            "Bypass".into(),
                            "-File".into(),
                            path.display().to_string(),
                        ],
                    );
                }
                return (ps, vec!["-NoProfile".into(), "-Command".into(), cmd.to_string()]);
            }
            return (ps, vec!["/C".into(), cmd.to_string()]);
        }
        if let Some(path) = script {
            return ("cmd".to_string(), vec!["/C".into(), path.display().to_string()]);
        }
        ("cmd".to_string(), vec!["/C".into(), cmd.to_string()])
    }
    #[cfg(not(windows))]
    {
        let use_sudo = !is_root_unix() && can_sudo_unix();
        if let Some(path) = script {
            if use_sudo {
                return (
                    "sudo".to_string(),
                    vec!["-n".into(), "sh".into(), path.display().to_string()],
                );
            }
            return ("sh".to_string(), vec![path.display().to_string()]);
        }
        if use_sudo {
            return (
                "sudo".to_string(),
                vec!["-n".into(), "sh".into(), "-c".into(), cmd.to_string()],
            );
        }
        ("sh".to_string(), vec!["-c".into(), cmd.to_string()])
    }
}

/// 返回 (可执行文件, 参数列表)。参数列表中已包含待执行命令。
pub fn shell_invocation(cmd: &str) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        if let Ok(shell) = std::env::var("LIGONG_SHELL") {
            let s = shell.trim();
            if !s.is_empty() {
                return (s.to_string(), args_for_shell(s, cmd));
            }
        }
        if let Some(ps) = detect_powershell() {
            return (ps, vec!["-NoProfile".into(), "-Command".into(), cmd.to_string()]);
        }
        ("cmd".to_string(), vec!["/C".into(), cmd.to_string()])
    }
    #[cfg(not(windows))]
    {
        ("sh".to_string(), vec!["-c".into(), cmd.to_string()])
    }
}

/// 依据 shell 可执行文件名推断参数形式。
#[cfg(windows)]
fn args_for_shell(shell: &str, cmd: &str) -> Vec<String> {
    let lower = shell.to_ascii_lowercase();
    if lower.contains("powershell") || lower.ends_with("pwsh.exe") || lower.ends_with("pwsh") {
        vec!["-NoProfile".into(), "-Command".into(), cmd.to_string()]
    } else if lower.ends_with("bash.exe") || lower.ends_with("bash") {
        vec!["-c".into(), cmd.to_string()]
    } else {
        vec!["/C".into(), cmd.to_string()]
    }
}

/// 探测可用的 PowerShell：优先 pwsh.exe（PS7），回退 powershell.exe（5.1）。
#[cfg(windows)]
fn detect_powershell() -> Option<String> {
    for cand in ["pwsh", "powershell"] {
        if which_on_path(cand) {
            return Some(cand.to_string());
        }
    }
    None
}

/// 在 PATH（含 PATHEXT）中查找可执行文件。
#[cfg(windows)]
fn which_on_path(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    for dir in std::env::split_paths(&path) {
        for e in &exts {
            if dir.join(format!("{cmd}{e}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// 用最高权限运行命令，避免 agent 因权限不足反复重试。
/// Windows: 优先 Start-Process -Verb RunAs（触发 UAC，一次性确认）；
///          已提权进程内则直接执行，不再弹窗。
/// Unix: 已是 root 则直接跑；否则探测 sudo -n，可用则提权执行，不可用则降级为普通执行。
/// 用 if/else 而非 `sudo ... || cmd`，避免 `;` 与 `||` 优先级导致同一命令被重复执行。
pub fn wrap_elevated(cmd: &str) -> String {
    #[cfg(windows)]
    {
        if is_elevated_windows() {
            return cmd.to_string();
        }
        let out = std::env::temp_dir().join(format!(
            "ligong_elev_{}_{}.out",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let out_s = out.display().to_string();
        let out_s_esc = out_s.replace('\'', "''");

        let child_script = std::env::temp_dir().join(format!(
            "ligong_payload_{}_{}.ps1",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        {
            use std::io::Write as _;
            let Ok(mut f) = std::fs::File::create(&child_script) else {
                return cmd.to_string();
            };
            let _ = f.write_all(b"\xEF\xBB\xBF");
            let _ = f.write_all(
                format!(
                    "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8\n\
                     & {{\n{cmd}\n}} *> '{out_s_esc}'\n"
                )
                .as_bytes(),
            );
            let _ = f.flush();
        }
        let child_s_esc = child_script.display().to_string().replace('\'', "''");
        format!(
            "$ErrorActionPreference='Continue'; \
             try {{ \
               Start-Process -Verb RunAs -Wait -FilePath powershell \
               -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{child_s_esc}'; \
             }} catch {{ Write-Output ('提权失败: ' + $_.Exception.Message); }}; \
             if (Test-Path '{out_s_esc}') {{ Get-Content -LiteralPath '{out_s_esc}' }}; \
             Remove-Item -LiteralPath '{out_s_esc}','{child_s_esc}' -Force -ErrorAction SilentlyContinue"
        )
    }
    #[cfg(not(windows))]
    {
        cmd.to_string()
    }
}

#[cfg(windows)]
fn is_elevated_windows() -> bool {
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command",
               "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_root_unix() -> bool {
    extern "C" { fn geteuid() -> u32; }
    unsafe { geteuid() == 0 }
}

#[cfg(not(windows))]
fn can_sudo_unix() -> bool {
    std::process::Command::new("sudo")
        .args(["-n", "true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}