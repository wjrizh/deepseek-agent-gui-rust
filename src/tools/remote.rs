//! 远程工具执行：agent 工具在远程主机上运行（ssh）。
//!
//! 仅实现 execute_command；文件工具后续按需扩展。
//! 复用 `ide::ssh` 的执行层。

use std::collections::HashMap;
use std::time::Duration;

use deepseek_agent::error::{AgentError, Result};

use crate::projects::RemoteSpec;

/// 在远程主机执行一条命令（非交互），返回合并输出。
pub async fn exec_remote(
    spec: &RemoteSpec,
    params: &HashMap<String, String>,
    on_output: &mut (dyn for<'a> FnMut(&'a str) + Send),
) -> Result<String> {
    const CMD_KEYS: &[&str] = &[
        "command", "cmd", "shell", "script", "bash", "sh", "run", "exec", "code",
    ];
    let cmd = CMD_KEYS
        .iter()
        .find_map(|k| params.get(*k))
        .map(|s| s.trim().to_string())
        .or_else(|| {
            if params.len() == 1 {
                params.values().next().map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    if cmd.is_empty() {
        return Err(AgentError::Other("execute_command: 空命令".into()));
    }

    // 切到项目 root 再执行（跨平台）。
    let is_win = crate::ide::backend::is_windows_remote(spec).await;
    let root = spec.root.trim();

    let out = if is_win {
        // Windows 远端必须走 exec_ps：pwsh 默认按 OEM 代码页（GBK）输出，
        // 经 UTF-16LE EncodedCommand 包装后可强制 UTF-8，避免中文乱码。
        let script = if root.is_empty() {
            cmd.clone()
        } else {
            format!(
                "Set-Location -LiteralPath {}; {}",
                crate::ide::backend::ps_lit(root),
                cmd
            )
        };
        crate::ide::ssh::exec_ps(spec, &script, Duration::from_secs(300)).await
    } else {
        let full = if root.is_empty() {
            cmd.clone()
        } else {
            format!(
                "cd {} && {}",
                crate::ide::backend::shell_quote(root),
                cmd
            )
        };
        crate::ide::ssh::exec_timeout(spec, &full, Duration::from_secs(300)).await
    }
    .map_err(AgentError::Other)?;

    let mut combined = String::new();
    if !out.stdout.is_empty() {
        on_output(&out.stdout);
        combined.push_str(&out.stdout);
    }
    if !out.stderr.is_empty() {
        on_output(&out.stderr);
        combined.push_str(&out.stderr);
    }
    combined.push_str(&format!("\n[exit_code: {}]\n", out.code));
    Ok(combined)
}