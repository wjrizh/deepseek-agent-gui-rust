//! BridgeModel：通过 Node.js + Playwright bridge 实现的 Model。
//!
//! Rust 侧不碰 CDP/浏览器协议，只 spawn 一个常驻 node 子进程，
//! 用 stdio 行协议（JSON）收发 prompt → 纯文本响应。

use crate::agent::model::{Model, ModelReply};
use crate::api::types::{CompletionReq, DeltaKind};
use crate::error::{AgentError, Result};
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use std::fs::OpenOptions;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

fn bridge_stderr(profile_dir: &Path) -> Stdio {
    let path = std::env::var("LIGONG_BRIDGE_LOG").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        let account = profile_dir.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "unknown".to_string());
        std::path::PathBuf::from(format!("/tmp/bridge-{account}.log"))
    });
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => Stdio::from(f),
        Err(_) => Stdio::inherit(),
    }
}

pub struct BridgeModel {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    req_seq: std::sync::atomic::AtomicU64,
}

impl BridgeModel {
    pub async fn launch(profile_dir: &Path, state_file: Option<&Path>) -> Result<Arc<Self>> {
        // 清理失效的 profile 锁（上次 bridge 被 kill 但 chromium 未清理时残留）。
        // 仅在 Unix 下执行：Windows 无此机制，且 libc 为 Unix 专属依赖。
        #[cfg(unix)]
        {
            let lock = profile_dir.join("SingletonLock");
            if let Ok(target) = std::fs::read_link(&lock) {
                if let Some(pid_str) = target.to_string_lossy().rsplit('-').next() {
                    if let Ok(pid) = pid_str.parse::<i32>() {
                        let alive = unsafe { libc::kill(pid, 0) } == 0;
                        if !alive {
                            let _ = std::fs::remove_file(&lock);
                            let _ = std::fs::remove_file(profile_dir.join("SingletonSocket"));
                            let _ = std::fs::remove_file(profile_dir.join("SingletonCookie"));
                        }
                    }
                }
            }
        }

        let script = crate::paths::bridge_server_script();
        let mut cmd = Command::new(crate::paths::node_bin());
        cmd.arg(&script)
            .env("LIGONG_PROFILE_DIR", profile_dir)
            .env("LIGONG_PLAYWRIGHT", crate::paths::playwright_dir());
        if let Some(sf) = state_file {
            cmd.env("LIGONG_STATE_FILE", sf);
        }
        if let Some(ch) = crate::paths::chromium_exec() {
            cmd.env("LIGONG_CHROMIUM", ch);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(bridge_stderr(profile_dir));
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| AgentError::Other(format!("启动 bridge node 进程失败: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::Other("bridge 无 stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Other("bridge 无 stdout".into()))?;
        let mut stdout = BufReader::new(stdout);

        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .await
            .map_err(|e| AgentError::Other(format!("bridge 读 ready 行失败: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(line.trim())
            .map_err(|e| AgentError::Other(format!("bridge ready JSON 解析失败: {e}, line={line}")))?;
        if v.get("ready") != Some(&serde_json::Value::Bool(true)) {
            return Err(AgentError::Other(format!("bridge 未就绪: {line}")));
        }

        Ok(Arc::new(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            req_seq: std::sync::atomic::AtomicU64::new(0),
        }))
    }

    pub async fn stop(&self) {
        let mut stdin = self.stdin.lock().await;
        let msg = serde_json::json!({ "stop": true });
        let mut buf = msg.to_string();
        buf.push('\n');
        let _ = stdin.write_all(buf.as_bytes()).await;
        let _ = stdin.flush().await;
    }

    pub async fn shutdown(&self) {
        {
            let mut stdin = self.stdin.lock().await;
            let msg = serde_json::json!({ "quit": true });
            let mut buf = msg.to_string();
            buf.push('\n');
            let _ = stdin.write_all(buf.as_bytes()).await;
            let _ = stdin.flush().await;
        }
        let mut child = self.child.lock().await;
        // node 由 launch 时 process_group(0) 设为组长，pgid == 自身 pid，
        // chromium 及子孙默认继承同组。先杀整组，再 wait，避免孤儿 chromium 占 profile。
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }

    pub async fn set_toggles(&self, thinking: bool, search: bool) -> Result<()> {
        // 单向写入，不等响应：切换开关须在 ask 进行中也能即时生效，
        // 若在此读 stdout 会与在途的 ask 争锁而被阻塞。
        let mut stdin = self.stdin.lock().await;
        let msg = serde_json::json!({ "toggles": { "thinking": thinking, "search": search } });
        let mut buf = msg.to_string();
        buf.push('\n');
        stdin
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| AgentError::Other(format!("bridge toggle 写入失败: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Other(format!("bridge toggle flush 失败: {e}")))?;
        Ok(())
    }

    async fn ask(
        &self,
        prompt: &str,
        thinking: bool,
        search: bool,
        session_id: Option<&str>,
        _parent_message_id: Option<i64>,
        files: &[String],
    ) -> Result<(String, String, Option<i64>, Option<String>)> {
        let rid = self
            .req_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let req = serde_json::json!({
            "prompt": prompt,
            "thinking": thinking,
            "search": search,
            "session_id": session_id,
            "parent_message_id": _parent_message_id,
            "files": files,
            "req_id": rid,
        });
        let mut buf = req.to_string();
        buf.push('\n');
        stdin
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| AgentError::Other(format!("bridge 写入失败: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Other(format!("bridge flush 失败: {e}")))?;
        drop(stdin);

        let v: serde_json::Value = loop {
            let mut line = String::new();
            let n = stdout
                .read_line(&mut line)
                .await
                .map_err(|e| AgentError::Other(format!("bridge 读响应失败: {e}")))?;
            if n == 0 {
                return Err(AgentError::Other("bridge 已关闭".into()));
            }
            let parsed: serde_json::Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match parsed.get("req_id").and_then(|x| x.as_u64()) {
                Some(v) if v == rid => break parsed,
                Some(_) => continue,
                None => break parsed,
            }
        };

        if v.get("ok") == Some(&serde_json::Value::Bool(true)) {
            let answer = v["text"].as_str().unwrap_or("").to_string();
            let think = v["think"].as_str().unwrap_or("").to_string();
            let mid = v["message_id"].as_i64();
            let sid = v["session_id"].as_str().map(|s| s.to_string());
            Ok((think, answer, mid, sid))
        } else {
            let err = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            Err(AgentError::Other(format!("bridge error: {err}")))
        }
    }

    pub async fn bridge_message_count(&self, session_id: &str) -> Result<Option<i64>> {
        let rid = self
            .req_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let req = serde_json::json!({ "message_count": true, "session_id": session_id, "req_id": rid });
        let mut buf = req.to_string();
        buf.push('\n');
        stdin
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| AgentError::Other(format!("bridge message_count 写入失败: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Other(format!("bridge message_count flush 失败: {e}")))?;
        drop(stdin);

        let v: serde_json::Value = loop {
            let mut line = String::new();
            let n = stdout
                .read_line(&mut line)
                .await
                .map_err(|e| AgentError::Other(format!("bridge message_count 读响应失败: {e}")))?;
            if n == 0 {
                return Ok(None);
            }
            let parsed: serde_json::Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match parsed.get("req_id").and_then(|x| x.as_u64()) {
                Some(v) if v == rid => break parsed,
                Some(_) => continue,
                None => break parsed,
            }
        };
        if v.get("ok") != Some(&serde_json::Value::Bool(true)) {
            return Ok(None);
        }
        Ok(v["count"].as_i64())
    }
    pub async fn bridge_list_sessions(&self) -> Result<Vec<(String, String)>> {
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let mut buf = serde_json::json!({ "list_sessions": true }).to_string();
        buf.push('\n');
        stdin.write_all(buf.as_bytes()).await
            .map_err(|e| AgentError::Other(format!("list_sessions 写入失败: {e}")))?;
        stdin.flush().await
            .map_err(|e| AgentError::Other(format!("list_sessions flush 失败: {e}")))?;
        drop(stdin);
        let mut line = String::new();
        stdout.read_line(&mut line).await
            .map_err(|e| AgentError::Other(format!("list_sessions 读响应失败: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(line.trim())
            .map_err(|e| AgentError::Other(format!("list_sessions 解析失败: {e}")))?;
        Ok(v["sessions"].as_array().map(|a| a.iter().filter_map(|x| {
            Some((x["id"].as_str()?.to_string(), x["title"].as_str().unwrap_or("").to_string()))
        }).collect()).unwrap_or_default())
    }

    pub async fn bridge_delete_session(&self, session_id: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let req = serde_json::json!({ "delete_session": true, "session_id": session_id });
        let mut buf = req.to_string();
        buf.push('\n');
        stdin
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| AgentError::Other(format!("bridge delete 写入失败: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Other(format!("bridge delete flush 失败: {e}")))?;
        drop(stdin);
        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .await
            .map_err(|e| AgentError::Other(format!("bridge delete 读响应失败: {e}")))?;
        Ok(())
    }
    pub async fn bridge_upload_files(&self, session_id: Option<&str>, paths: &[String]) -> Result<usize> {
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let req = serde_json::json!({ "upload_files": paths, "session_id": session_id });
        let mut buf = req.to_string();
        buf.push('\n');
        stdin
            .write_all(buf.as_bytes())
            .await
            .map_err(|e| AgentError::Other(format!("bridge upload 写入失败: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Other(format!("bridge upload flush 失败: {e}")))?;
        drop(stdin);
        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .await
            .map_err(|e| AgentError::Other(format!("bridge upload 读响应失败: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(line.trim())
            .map_err(|e| AgentError::Other(format!("bridge upload 解析失败: {e}, line={line}")))?;
        Ok(v["uploaded"].as_u64().unwrap_or(0) as usize)
    }

    pub async fn bridge_list_files(&self, session_id: Option<&str>) -> Result<Vec<String>> {
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let req = serde_json::json!({ "list_files": true, "session_id": session_id });
        let mut buf = req.to_string();
        buf.push('\n');
        stdin.write_all(buf.as_bytes()).await
            .map_err(|e| AgentError::Other(format!("bridge list_files 写入失败: {e}")))?;
        stdin.flush().await
            .map_err(|e| AgentError::Other(format!("bridge list_files flush 失败: {e}")))?;
        drop(stdin);
        let mut line = String::new();
        stdout.read_line(&mut line).await
            .map_err(|e| AgentError::Other(format!("bridge list_files 读响应失败: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(line.trim())
            .map_err(|e| AgentError::Other(format!("bridge list_files 解析失败: {e}, line={line}")))?;
        Ok(v["files"].as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default())
    }

    pub async fn bridge_remove_file(&self, session_id: Option<&str>, name: &str) -> Result<bool> {
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let req = serde_json::json!({ "remove_file": name, "session_id": session_id });
        let mut buf = req.to_string();
        buf.push('\n');
        stdin.write_all(buf.as_bytes()).await
            .map_err(|e| AgentError::Other(format!("bridge remove_file 写入失败: {e}")))?;
        stdin.flush().await
            .map_err(|e| AgentError::Other(format!("bridge remove_file flush 失败: {e}")))?;
        drop(stdin);
        let mut line = String::new();
        stdout.read_line(&mut line).await
            .map_err(|e| AgentError::Other(format!("bridge remove_file 读响应失败: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(line.trim())
            .map_err(|e| AgentError::Other(format!("bridge remove_file 解析失败: {e}, line={line}")))?;
        Ok(v["removed"].as_bool().unwrap_or(false))
    }

    pub async fn bridge_clear_files(&self, session_id: Option<&str>) -> Result<usize> {
        let mut stdin = self.stdin.lock().await;
        let mut stdout = self.stdout.lock().await;
        let req = serde_json::json!({ "clear_files": true, "session_id": session_id });
        let mut buf = req.to_string();
        buf.push('\n');
        stdin.write_all(buf.as_bytes()).await
            .map_err(|e| AgentError::Other(format!("bridge clear_files 写入失败: {e}")))?;
        stdin.flush().await
            .map_err(|e| AgentError::Other(format!("bridge clear_files flush 失败: {e}")))?;
        drop(stdin);
        let mut line = String::new();
        stdout.read_line(&mut line).await
            .map_err(|e| AgentError::Other(format!("bridge clear_files 读响应失败: {e}")))?;
        let v: serde_json::Value = serde_json::from_str(line.trim())
            .map_err(|e| AgentError::Other(format!("bridge clear_files 解析失败: {e}, line={line}")))?;
        Ok(v["cleared"].as_u64().unwrap_or(0) as usize)
    }

    fn chunk_text(text: &str, size: usize) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        chars.chunks(size).map(|c| c.iter().collect()).collect()
    }
}

#[async_trait]
impl Model for BridgeModel {
    async fn complete(
        &self,
        req: &CompletionReq,
        on_delta: &mut (dyn for<'a> FnMut(DeltaKind, &'a str) + Send),
    ) -> Result<ModelReply> {
        let sid_opt = if req.chat_session_id.is_empty() {
            None
        } else {
            Some(req.chat_session_id.as_str())
        };
        let (think, answer, mid, sid) = self
            .ask(
                &req.prompt,
                req.thinking_enabled,
                req.search_enabled,
                sid_opt,
                req.parent_message_id,
                &req.ref_file_ids,
            )
            .await?;

        for chunk in Self::chunk_text(&think, 24) {
            on_delta(DeltaKind::Thinking, &chunk);
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
        for chunk in Self::chunk_text(&answer, 24) {
            on_delta(DeltaKind::Answer, &chunk);
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }

        let content = if think.is_empty() {
            answer.clone()
        } else {
            format!("<think>\n{think}\n</think>\n{answer}")
        };
        Ok(ModelReply {
            content,
            message_id: mid,
            title: None,
            session_id: sid,
        })
    }

    async fn new_session(&self) -> Result<String> {
        Ok(String::new())
    }

    async fn sync_parent(&self, _session_id: &str, local: Option<i64>) -> Result<Option<i64>> {
        Ok(local)
    }

    async fn stop(&self) {
        BridgeModel::stop(self).await;
    }

    async fn message_count(&self, session_id: &str) -> Result<Option<i64>> {
        BridgeModel::bridge_message_count(self, session_id).await
    }

    async fn delete_session(&self, session_id: &str) -> Result<()> {
        BridgeModel::bridge_delete_session(self, session_id).await
    }

    async fn set_toggles(&self, thinking: bool, search: bool) -> Result<()> {
        BridgeModel::set_toggles(self, thinking, search).await
    }

    async fn upload_files(&self, session_id: Option<&str>, paths: &[String]) -> Result<usize> {
        BridgeModel::bridge_upload_files(self, session_id, paths).await
    }

    async fn list_files(&self, session_id: Option<&str>) -> Result<Vec<String>> {
        BridgeModel::bridge_list_files(self, session_id).await
    }

    async fn remove_file(&self, session_id: Option<&str>, name: &str) -> Result<bool> {
        BridgeModel::bridge_remove_file(self, session_id, name).await
    }

    async fn clear_files(&self, session_id: Option<&str>) -> Result<usize> {
        BridgeModel::bridge_clear_files(self, session_id).await
    }
}

impl Drop for BridgeModel {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.start_kill();
        }
    }
}