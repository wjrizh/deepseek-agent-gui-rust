//! SSH 本地端口转发（ssh -N -L）。
//!
//! 每个转发是一条独立的 ssh 子进程，持有 Child；Drop 即关闭。
//! 由 AppState.forwards 按本地端口持有，供前端开关。

use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio::process::{Child, Command};

use crate::ide::ssh;
use crate::state::AppState;

/// 转发句柄：持有子进程，drop 时杀掉。
pub struct ForwardHandle {
    pub child: Child,
    pub spec: crate::projects::RemoteSpec,
    pub remote_host: String,
    pub remote_port: u16,
}

impl Drop for ForwardHandle {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// 转发信息（返回给前端）。
#[derive(Debug, Clone, Serialize)]
pub struct ForwardInfo {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

/// 打开一个本地端口转发：本地 127.0.0.1:local_port 到远端 remote_host:remote_port。
#[tauri::command]
pub async fn remote_forward_open(
    host: String,
    user: Option<String>,
    port: Option<u16>,
    ssh_alias: Option<String>,
    local_port: u16,
    remote_host: String,
    remote_port: u16,
    state: State<'_, AppState>,
) -> Result<ForwardInfo, String> {
    if local_port == 0 {
        return Err("本地端口不能为 0".into());
    }
    let spec = crate::projects::RemoteSpec {
        host,
        user,
        port,
        root: String::new(),
        ssh_alias,
        proxy_jump: None,
        restrict: false,
    };

    {
        let inner = state.inner.lock().await;
        if inner.forwards.contains_key(&local_port) {
            return Err(format!("本地端口 {local_port} 已被占用"));
        }
    }

    let mut args = ssh::base_opts(&spec);
    args.push("-N".into());
    args.push("-L".into());
    args.push(format!("127.0.0.1:{local_port}:{remote_host}:{remote_port}"));
    args.push("-o".into());
    args.push("ExitOnForwardFailure=yes".into());
    args.push(ssh::target_string(&spec));

    let child = Command::new("ssh")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 ssh 转发失败：{e}"))?;

    let handle = ForwardHandle {
        child,
        spec,
        remote_host: remote_host.clone(),
        remote_port,
    };
    {
        let mut inner = state.inner.lock().await;
        inner.forwards.insert(local_port, handle);
    }

    // 稍等：若 ssh 立即退出（端口被占/认证失败），回传错误。
    tokio::time::sleep(Duration::from_millis(400)).await;
    {
        let mut inner = state.inner.lock().await;
        if let Some(h) = inner.forwards.get_mut(&local_port) {
            if let Ok(Some(status)) = h.child.try_wait() {
                let mut err = String::new();
                if let Some(mut e) = h.child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = e.read_to_string(&mut err).await;
                }
                inner.forwards.remove(&local_port);
                return Err(format!(
                    "转发未建立（退出码 {}）：{}",
                    status.code().unwrap_or(-1),
                    err.trim()
                ));
            }
        }
    }

    Ok(ForwardInfo {
        local_port,
        remote_host,
        remote_port,
    })
}

/// 关闭指定本地端口的转发。
#[tauri::command]
pub async fn remote_forward_close(local_port: u16, state: State<'_, AppState>) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    inner
        .forwards
        .remove(&local_port)
        .map(|_| ())
        .ok_or_else(|| format!("转发不存在：{local_port}"))
}

/// 列出当前所有转发。
#[tauri::command]
pub async fn remote_forward_list(state: State<'_, AppState>) -> Result<Vec<ForwardInfo>, String> {
    let inner = state.inner.lock().await;
    Ok(inner
        .forwards
        .iter()
        .map(|(p, h)| ForwardInfo {
            local_port: *p,
            remote_host: h.remote_host.clone(),
            remote_port: h.remote_port,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 远端起一个 HTTP 服务，经本地转发访问，验证数据通路。
    #[tokio::test]
    #[ignore]
    async fn forward_roundtrip_localhost() {
        // 远端监听 18080 的简易服务。
        let remote_cmd = "python3 -m http.server 18080 --directory /tmp >/dev/null 2>&1 & echo $!";
        let spec = crate::projects::RemoteSpec {
            host: "localhost".into(),
            user: None,
            port: None,
            root: "/tmp".into(),
            ssh_alias: Some("localhost".into()),
            proxy_jump: None,
            restrict: false,
        };
        let pid_out = ssh::exec(&spec, remote_cmd).await.unwrap();
        let pid = pid_out.stdout.trim().to_string();
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 直接用 ssh -N -L 建转发（绕过 Tauri State，验证命令组装正确）。
        let mut args = ssh::base_opts(&spec);
        args.push("-N".into());
        args.push("-L".into());
        args.push("127.0.0.1:18081:127.0.0.1:18080".into());
        args.push("-o".into());
        args.push("ExitOnForwardFailure=yes".into());
        args.push(ssh::target_string(&spec));
        let mut child = Command::new("ssh")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 通过本地端口发 HTTP 请求。
        let resp = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect("127.0.0.1:18081"),
        )
        .await;
        assert!(resp.is_ok() && resp.unwrap().is_ok(), "本地端口未打通");

        let _ = child.start_kill();
        let _ = ssh::exec(&spec, &format!("kill {pid} 2>/dev/null || true")).await;
    }
}
