//! 远程工具执行：agent 工具在远程主机上运行（ssh）。
//!
//! 仅实现 execute_command；文件工具后续按需扩展。
//! 复用 `ide::ssh` 的执行层。

use std::collections::HashMap;
use std::time::Duration;

use deepseek_agent::error::{AgentError, Result};

use crate::projects::RemoteSpec;

/// 在远程主机执行一条命令（非交互），返回合并输出。
/// 支持外部 `cancel` 令牌，取消时 kill 本地 ssh 子进程。
pub async fn exec_remote(
    spec: &RemoteSpec,
    params: &HashMap<String, String>,
    on_output: &mut (dyn for<'a> FnMut(&'a str) + Send),
    cancel: &tokio_util::sync::CancellationToken,
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
        crate::ide::ssh::exec_ps_cancel(spec, &script, Duration::from_secs(300), cancel).await
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
        crate::ide::ssh::exec_timeout_cancel(spec, &full, Duration::from_secs(300), cancel).await
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

/// abs（或 rel）→ rel。先按绝对路径尝试（必须落在 root 下），失败则当 rel 透传。
/// 统一分隔符为 `/`，去前导斜杠；空路径报错。
pub fn resolve_rel(spec: &RemoteSpec, path: &str) -> Result<String> {
    let p = path.trim();
    if let Ok(rel) = crate::ide::backend::to_rel(spec, p) {
        if !rel.is_empty() {
            return Ok(rel);
        }
    }
    let rel = p.replace('\\', "/").trim_start_matches('/').to_string();
    if rel.is_empty() {
        return Err(AgentError::Other(format!("无效路径：{path}")));
    }
    Ok(rel)
}

/// 远程读文本文件：path 可为远程绝对路径或 rel。
pub async fn read_remote(
    spec: &RemoteSpec,
    params: &HashMap<String, String>,
) -> Result<String> {
    let path = params
        .get("file_path")
        .or_else(|| params.get("path"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::Other("read_file: 缺 file_path".into()))?;
    let rel = resolve_rel(spec, path)?;
    crate::ide::backend::read_file(spec, &rel)
        .await
        .map_err(|e| AgentError::Other(format!("远程读取失败：{e}")))
}

/// 远程写文件：path 可为远程绝对路径或 rel。覆盖式写入，自动建父目录。
pub async fn write_remote(
    spec: &RemoteSpec,
    params: &HashMap<String, String>,
) -> Result<String> {
    let path = params
        .get("file_path")
        .or_else(|| params.get("path"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::Other("write_file: 缺 file_path".into()))?;
    let content = params
        .get("content")
        .ok_or_else(|| AgentError::Other("write_file: 缺 content".into()))?;
    let rel = resolve_rel(spec, path)?;
    crate::ide::backend::write_file(spec, &rel, content)
        .await
        .map(|_| format!("[✓] 已写入远程文件: {path}"))
        .map_err(|e| AgentError::Other(format!("远程写入失败：{e}")))
}

/// 远程编辑文件：读 → 精确替换 → 写。
/// 语义对齐本地 `EditFile`：CRLF 自适应、old_string 唯一（或 replace_all=true）。
pub async fn edit_remote(
    spec: &RemoteSpec,
    params: &HashMap<String, String>,
) -> Result<String> {
    let path = params
        .get("file_path")
        .or_else(|| params.get("path"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::Other("edit_file: 缺 file_path".into()))?;
    let old_string = params
        .get("old_string")
        .ok_or_else(|| AgentError::Other("edit_file: 缺 old_string".into()))?;
    let new_string = params
        .get("new_string")
        .ok_or_else(|| AgentError::Other("edit_file: 缺 new_string".into()))?;
    let replace_all = params
        .get("replace_all")
        .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(false);

    if old_string.is_empty() {
        return Err(AgentError::Other(
            "edit_file: old_string 为空（如需整文件覆盖请用 write_file）".into(),
        ));
    }
    if old_string == new_string {
        return Err(AgentError::Other(
            "edit_file: old_string 与 new_string 相同，无需修改".into(),
        ));
    }

    let rel = resolve_rel(spec, path)?;
    let text = crate::ide::backend::read_file(spec, &rel)
        .await
        .map_err(|e| AgentError::Other(format!("远程读取失败：{e}")))?;

    // CRLF 自适应：磁盘可能是 \r\n，模型给的 old/new 几乎总是纯 \n。
    let is_crlf = text.contains("\r\n");
    let norm_text = if is_crlf {
        text.replace("\r\n", "\n")
    } else {
        text.clone()
    };
    let norm_old = old_string.replace("\r\n", "\n");
    let norm_new = new_string.replace("\r\n", "\n");

    let count = norm_text.matches(&norm_old).count();
    if count == 0 {
        return Err(AgentError::Other(
            "edit_file: old_string 未在文件中出现（必须精确匹配，含空白与缩进）".into(),
        ));
    }
    if count > 1 && !replace_all {
        return Err(AgentError::Other(format!(
            "edit_file: old_string 出现 {count} 次，不唯一。请补充更多上下文使其唯一，或设 replace_all=true"
        )));
    }

    let updated_norm = if replace_all {
        norm_text.replace(&norm_old, &norm_new)
    } else {
        norm_text.replacen(&norm_old, &norm_new, 1)
    };
    let final_text = if is_crlf {
        updated_norm.replace('\n', "\r\n")
    } else {
        updated_norm
    };

    crate::ide::backend::write_file(spec, &rel, &final_text)
        .await
        .map_err(|e| AgentError::Other(format!("远程写入失败：{e}")))?;

    if replace_all && count > 1 {
        Ok(format!("[✓] 已替换 {count} 处（远程: {path}）"))
    } else {
        Ok(format!("[✓] 已替换 1 处（远程: {path}）"))
    }
}