//! GUI 端工具实现。
//!
//! 与 CLI 的库内 `ExecuteCommand`（PTY 版）不同：
//! GUI 不需要真 PTY、不需要用户实时键入，因此改用 `tokio::process`
//! 逐行流式读取 stdout/stderr。这样做：
//! - 保留工具卡的实时输出（`on_output` 回调）；
//! - 无 heredoc 硬拦截、无阻塞型管道剥离（那些是 PTY 的补丁）；
//! - 命令在 `sh -c` 中执行，多行/heredoc 原生可用。

pub mod read_file;
pub mod write_file;
pub mod edit_file;
pub mod remote;

pub use read_file::ReadFile;
pub use write_file::WriteFile;
pub use edit_file::EditFile;

use async_trait::async_trait;
use deepseek_agent::agent::tool::{Tool, MAX_TOOL_OUTPUT};
use deepseek_agent::error::{AgentError, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// 非交互空闲超时（秒）。持续有输出则不中断。
const IDLE_TIMEOUT_SECS: u64 = 300;

/// 内置工具：在项目工作目录执行一条 shell 命令（非 PTY）。
pub struct ExecuteCommand {
    cwd: PathBuf,
    cancel: Option<CancellationToken>,
}

impl ExecuteCommand {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd, cancel: None }
    }

    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

#[async_trait]
impl Tool for ExecuteCommand {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "在项目工作目录执行一条 shell 命令，返回 exit_code 与输出。"
    }

    async fn call(
        &self,
        params: &HashMap<String, String>,
        _live: bool,
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

        let cmd = crate::shell::wrap_elevated(&cmd);
        let script = crate::shell::write_temp_script(&cmd);
        let result = async {
            let (prog, args) = crate::shell::shell_invocation_ex(&cmd, script.as_ref());
            let mut command = Command::new(&prog);
            command
                .args(&args)
                .current_dir(&self.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(windows)]
            {
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                command.creation_flags(CREATE_NO_WINDOW);
            }
            let mut child = command
                .spawn()
                .map_err(|e| AgentError::Other(format!("spawn: {e}")))?;

            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| AgentError::Other("stdout 未捕获".into()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| AgentError::Other("stderr 未捕获".into()))?;

            let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
            spawn_reader(stdout, tx.clone());
            spawn_reader(stderr, tx.clone());
            drop(tx);

            let mut collected = String::new();
            let deadline = Duration::from_secs(IDLE_TIMEOUT_SECS);

            let cancel_watch = self.cancel.clone();

            let status = loop {
                let recv = tokio::time::timeout(deadline, rx.recv());
                let line = match &cancel_watch {
                    Some(c) => {
                        tokio::select! {
                            _ = c.cancelled() => {
                                let _ = child.kill().await;
                                let _ = child.wait().await;
                                break None;
                            }
                            r = recv => r,
                        }
                    }
                    None => recv.await,
                };
                match line {
                    Ok(Some(line)) => {
                        if collected.len() < MAX_TOOL_OUTPUT {
                            collected.push_str(&line);
                            collected.push('\n');
                        }
                        on_output(&line);
                    }
                    Ok(None) => {
                        break Some(child
                            .wait()
                            .await
                            .map_err(|e| AgentError::Other(format!("wait: {e}")))?);
                    }
                    Err(_) => {
                        let _ = child.kill().await;
                        return Err(AgentError::Other(format!(
                            "命令空闲超时（{IDLE_TIMEOUT_SECS}s），已终止"
                        )));
                    }
                }
            };

            let status = match status {
                Some(s) => s,
                None => return Ok("[已停止]\nexit_code=-1\n命令被用户终止".to_string()),
            };

            let code = status.code().unwrap_or(-1);
            if collected.len() > MAX_TOOL_OUTPUT {
                collected.push_str("\n...[输出已截断]");
            }
            Ok::<String, AgentError>(format!("exit_code={code}\n{collected}"))
        }
        .await;
        if let Some(p) = &script {
            let _ = std::fs::remove_file(p);
        }
        result
    }
}

/// 把一个 `AsyncRead` 按行推入 channel（丢掉尾部无换行的残行以外均保留）。
fn spawn_reader<R>(reader: R, tx: tokio::sync::mpsc::Sender<String>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).await.is_err() {
                break;
            }
        }
    });
}