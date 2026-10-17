//! SSH 执行层：连接参数组装与远程命令执行。
//!
//! 阶段 1：只提供"结构化 exec"能力，供后续 FileBackend/GitBackend 复用。
//! 连接复用交给 OpenSSH 的 ControlMaster（不自研连接池）。
//! Windows 的 OpenSSH 不支持 ControlMaster，自动跳过复用选项。

use std::time::Duration;

use tokio::process::Command as TokioCommand;

use crate::projects::RemoteSpec;

/// 单条远程命令的默认超时。
const EXEC_TIMEOUT: Duration = Duration::from_secs(30);

/// ControlMaster socket 路径（%C = 连接元组 hash，自动隔离不同主机）。
#[cfg(unix)]
fn control_path() -> String {
    std::env::temp_dir()
        .join("ligong-ssh-%C")
        .to_string_lossy()
        .to_string()
}

/// 连接复用与保活选项。Windows 下 OpenSSH 不支持 ControlMaster，返回空。
pub fn mux_opts() -> Vec<String> {
    #[cfg(unix)]
    {
        vec![
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}", control_path()),
            "-o".into(),
            "ControlPersist=60s".into(),
        ]
    }
    #[cfg(windows)]
    {
        Vec::new()
    }
}

/// ssh 目标串：优先别名（复用 ~/.ssh/config），否则 user@host。
pub fn target_string(spec: &RemoteSpec) -> String {
    if let Some(alias) = spec.ssh_alias.as_deref().filter(|s| !s.trim().is_empty()) {
        return alias.to_string();
    }
    match spec.user.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(u) => format!("{u}@{}", spec.host),
        None => spec.host.clone(),
    }
}

/// 组装基础 ssh 选项（不含目标与命令）。
pub fn base_opts(spec: &RemoteSpec) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-o".into(),
        "BatchMode=yes".into(), // 禁止交互：未知 host key/密码时立即失败，不阻塞
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(), // 首次连接自动接受 host key 并写入 known_hosts，后续严格校验
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
    ];
    args.extend(mux_opts());
    if let Some(pj) = spec.proxy_jump.as_deref().filter(|s| !s.trim().is_empty()) {
        args.push("-J".into());
        args.push(pj.to_string());
    }
    if let Some(p) = spec.port {
        args.push("-p".into());
        args.push(p.to_string());
    }
    args
}

/// 组装交互式 ssh 终端参数（prog = "ssh"）。
///
/// 与 `base_opts` 的差异：
/// - **不含 `BatchMode=yes`**：交互终端需要能输密码 / 接受首次 host key。
/// - 加 `-tt` 强制分配 tty（远端 shell 才能交互）。
/// - 末尾附远端命令：切换到 `spec.root` 后 exec 登录 shell（PTY 的本地 cwd 对远端无意义）。
pub fn terminal_argv(spec: &RemoteSpec) -> (Vec<String>, String) {
    let mut args: Vec<String> = vec![
        "-tt".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
    ];
    args.extend(mux_opts());
    if let Some(pj) = spec.proxy_jump.as_deref().filter(|s| !s.trim().is_empty()) {
        args.push("-J".into());
        args.push(pj.to_string());
    }
    if let Some(p) = spec.port {
        args.push("-p".into());
        args.push(p.to_string());
    }
    let target = target_string(spec);
    args.push(target.clone());
    // 远端登录命令：切到 root（失败则忽略）后 exec 登录 shell。
    let root = spec.root.trim();
    let remote_cmd = if root.is_empty() {
        "exec ${SHELL:-/bin/sh} -l".to_string()
    } else {
        format!(
            "cd {} 2>/dev/null; exec ${{SHELL:-/bin/sh}} -l",
            crate::ide::backend::shell_quote(root)
        )
    };
    args.push(remote_cmd);
    let label = spec
        .ssh_alias
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| spec.host.clone());
    (args, label)
}

/// 远程命令执行结果。
#[derive(Debug, Clone)]
pub struct SshOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl SshOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// 在远程执行一条命令并返回结果。默认 30s 超时。
pub async fn exec(spec: &RemoteSpec, remote_cmd: &str) -> Result<SshOutput, String> {
    exec_timeout(spec, remote_cmd, EXEC_TIMEOUT).await
}

/// 带自定义超时的远程执行。
///
/// 连接层失败（连接未建立 => 命令未执行）会自动重试一次，
/// 命令执行本身的失败不重试（避免副作用重复执行）。
///
/// 若检测到 ControlMaster 复用 socket 损坏（mux_client/Session open refused/already exists），
/// 会清理坏 socket 并以 `ControlMaster=no` 重试一次，避免坏 socket 持续阻塞。
pub async fn exec_timeout(
    spec: &RemoteSpec,
    remote_cmd: &str,
    timeout: Duration,
) -> Result<SshOutput, String> {
    let first = exec_timeout_once(spec, remote_cmd, timeout).await?;
    if is_mux_error(&first) {
        // 复用 socket 坏了：清理后禁用 mux 重试（连接层问题，命令未执行，安全）。
        let _ = close_master(spec).await;
        return exec_timeout_once_nomux(spec, remote_cmd, timeout).await;
    }
    if !is_conn_error(&first) {
        return Ok(first);
    }
    // 首次连接失败：重试一次。
    let r = exec_timeout_once(spec, remote_cmd, timeout).await?;
    if is_mux_error(&r) {
        let _ = close_master(spec).await;
        return exec_timeout_once_nomux(spec, remote_cmd, timeout).await;
    }
    Ok(r)
}

/// `exec_timeout` 的单次执行（不含重试）。
async fn exec_timeout_once(
    spec: &RemoteSpec,
    remote_cmd: &str,
    timeout: Duration,
) -> Result<SshOutput, String> {
    exec_timeout_once_inner(spec, remote_cmd, timeout, true).await
}

/// 禁用 ControlMaster 的单次执行（用于坏 socket 兜底）。
async fn exec_timeout_once_nomux(
    spec: &RemoteSpec,
    remote_cmd: &str,
    timeout: Duration,
) -> Result<SshOutput, String> {
    exec_timeout_once_inner(spec, remote_cmd, timeout, false).await
}

async fn exec_timeout_once_inner(
    spec: &RemoteSpec,
    remote_cmd: &str,
    timeout: Duration,
    use_mux: bool,
) -> Result<SshOutput, String> {
    let mut args = base_opts(spec);
    if !use_mux {
        // 移除 base_opts 里混入的 ControlMaster/ControlPath/ControlPersist，
        // 显式指定不复用。
        args.retain(|a| {
            !a.starts_with("ControlMaster=")
                && !a.starts_with("ControlPath=")
                && !a.starts_with("ControlPersist=")
        });
        args.push("-o".into());
        args.push("ControlMaster=no".into());
        args.push("-o".into());
        args.push("ControlPath=none".into());
    }
    args.push(target_string(spec));
    args.push(remote_cmd.to_string());

    let fut = TokioCommand::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .output();

    let out = match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("启动 ssh 失败: {e}")),
        Err(_) => return Err(format!("远程命令超时（{}s）", timeout.as_secs())),
    };

    Ok(SshOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

/// 判定是否为 ControlMaster 复用 socket 损坏。
/// 特征：mux_client_* / Session open refused by peer / ControlSocket ... already exists。
fn is_mux_error(o: &SshOutput) -> bool {
    if o.ok() {
        return false;
    }
    let s = o.stderr.to_lowercase();
    s.contains("mux_client")
        || s.contains("session open refused")
        || s.contains("already exists")
}

/// 判定是否为连接层失败（stderr 特征匹配）。
/// 连接未建立时命令不会执行，重试安全。
fn is_conn_error(o: &SshOutput) -> bool {
    if o.ok() {
        return false;
    }
    let s = o.stderr.to_lowercase();
    [
        "connection refused",
        "connection reset",
        "connection closed",
        "broken pipe",
        "closed by remote host",
        "connection timed out",
        "no route to host",
        "kex_exchange_identification",
        "connection to",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// 带 stdin 的远程执行：向远端进程喂数据（如 base64 内容），避免命令行长度限制。
pub async fn exec_stdin(
    spec: &RemoteSpec,
    remote_cmd: &str,
    stdin_bytes: &[u8],
    timeout: Duration,
) -> Result<SshOutput, String> {
    let mut args = base_opts(spec);
    args.push(target_string(spec));
    args.push(remote_cmd.to_string());

    let mut child = TokioCommand::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 ssh 失败: {e}"))?;

    let mut stdin = child.stdin.take();
    let data = stdin_bytes.to_vec();
    let writer = tokio::spawn(async move {
        if let Some(s) = stdin.as_mut() {
            use tokio::io::AsyncWriteExt;
            let _ = s.write_all(&data).await;
            let _ = s.shutdown().await;
        }
    });

    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("远程命令失败: {e}")),
        Err(_) => {
            writer.abort();
            return Err(format!("远程命令超时（{}s）", timeout.as_secs()));
        }
    };
    let _ = writer.await;

    Ok(SshOutput {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    })
}

/// 可取消的远程执行：支持外部 `CancellationToken`，取消时 kill ssh 子进程。
///
/// 与 `exec_timeout` 的差异：`spawn` 子进程而非 `output()`，以便中途 kill。
/// `kill_on_drop(true)` 保证即使函数提前返回也会清理子进程。
pub async fn exec_timeout_cancel(
    spec: &RemoteSpec,
    remote_cmd: &str,
    timeout: Duration,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<SshOutput, String> {
    let mut args = base_opts(spec);
    args.push(target_string(spec));
    args.push(remote_cmd.to_string());

    let mut child = TokioCommand::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("启动 ssh 失败: {e}"))?;

    // 取出 stdout/stderr 句柄，避免在 wait() 期间被借用冲突。
    // （wait_with_output 会 move self，导致 cancel 分支无法 kill。）
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    let out = tokio::select! {
        r = tokio::time::timeout(timeout, child.wait()) => match r {
            Ok(Ok(status)) => {
                use tokio::io::AsyncReadExt;
                let mut out_buf = Vec::new();
                let mut err_buf = Vec::new();
                if let Some(mut s) = stdout.take() {
                    let _ = s.read_to_end(&mut out_buf).await;
                }
                if let Some(mut s) = stderr.take() {
                    let _ = s.read_to_end(&mut err_buf).await;
                }
                SshOutput {
                    code: status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&out_buf).to_string(),
                    stderr: String::from_utf8_lossy(&err_buf).to_string(),
                }
            }
            Ok(Err(e)) => return Err(format!("远程命令失败: {e}")),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(format!("远程命令超时（{}s）", timeout.as_secs()));
            }
        },
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("已取消".into());
        }
    };

    Ok(out)
}

/// 把 PowerShell 脚本编码为 UTF-16LE base64，用 `pwsh -EncodedCommand` 包装。
/// 这样远端只需接收 ASCII，彻底规避引号/中文/换行导致的转义问题。
pub fn ps_encoded_command(script: &str) -> String {
    // 强制 UTF-8 输出：pwsh 经 SSH 非交互管道时默认用 OEM 代码页（如 GBK），
    // 会把中文文件名/内容按 GBK 输出，Rust 侧 from_utf8_lossy 变成一串 '?'。
    // 前置设置 OutputEncoding（不覆盖脚本自身的 $ErrorActionPreference）。
    let wrapped = format!(
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\n{script}"
    );
    let mut bytes = Vec::with_capacity(wrapped.len() * 2);
    for unit in wrapped.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    format!("pwsh -NoProfile -NonInteractive -EncodedCommand {b64}")
}

/// 在远端用 pwsh 执行一段 PowerShell 脚本（脚本走 EncodedCommand，无需手动转义）。
pub async fn exec_ps(
    spec: &RemoteSpec,
    script: &str,
    timeout: Duration,
) -> Result<SshOutput, String> {
    exec_timeout(spec, &ps_encoded_command(script), timeout).await
}

/// 可取消的 pwsh 执行（脚本走 EncodedCommand）。
pub async fn exec_ps_cancel(
    spec: &RemoteSpec,
    script: &str,
    timeout: Duration,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<SshOutput, String> {
    exec_timeout_cancel(spec, &ps_encoded_command(script), timeout, cancel).await
}

/// 在远端用 pwsh 执行脚本，并把 stdin 作为数据喂给脚本（`[Console]::In.ReadToEnd()`）。
pub async fn exec_ps_stdin(
    spec: &RemoteSpec,
    script: &str,
    stdin_bytes: &[u8],
    timeout: Duration,
) -> Result<SshOutput, String> {
    exec_stdin(spec, &ps_encoded_command(script), stdin_bytes, timeout).await
}

/// 远程主机密钥指纹（供首次连接 TOFU 确认）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct HostKey {
    /// 密钥类型（ssh-ed25519 / ssh-rsa / ecdsa-…）。
    pub key_type: String,
    /// SHA256 指纹。
    pub fingerprint: String,
}

/// 获取远程主机公钥指纹（不建立可复用会话，仅 keyscan + keygen 计算指纹）。
pub async fn fetch_host_key(spec: &RemoteSpec) -> Result<Vec<HostKey>, String> {
    if spec.host.trim().is_empty() {
        return Err("host 为空".into());
    }
    let mut args = vec!["-T".to_string(), "5".to_string()];
    if let Some(p) = spec.port {
        args.push("-p".to_string());
        args.push(p.to_string());
    }
    args.push(spec.host.clone());

    let scan = TokioCommand::new("ssh-keyscan")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("启动 ssh-keyscan 失败: {e}"))?;
    if !scan.status.success() || scan.stdout.is_empty() {
        return Err(format!(
            "ssh-keyscan 失败: {}",
            String::from_utf8_lossy(&scan.stderr).trim()
        ));
    }

    // 把 known_hosts 格式的公钥喂给 `ssh-keygen -lf -` 得到指纹。
    let mut keygen = TokioCommand::new("ssh-keygen")
        .args(["-l", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 ssh-keygen 失败: {e}"))?;
    if let Some(mut stdin) = keygen.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(&scan.stdout).await;
        let _ = stdin.shutdown().await;
    }
    let out = keygen
        .wait_with_output()
        .await
        .map_err(|e| format!("ssh-keygen 执行失败: {e}"))?;

    let mut keys = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // 格式：`<bits> SHA256:xxxx comment (ED25519)` 或带 host 前缀。
        if let Some((_, rest)) = line.split_once(' ') {
            if let Some((fp, tail)) = rest.split_once(' ') {
                let key_type = tail
                    .rsplit_once('(')
                    .map(|(_, t)| t.trim_end_matches(')').to_lowercase())
                    .unwrap_or_else(|| tail.to_string());
                keys.push(HostKey {
                    key_type,
                    fingerprint: fp.to_string(),
                });
            }
        }
    }
    Ok(keys)
}

/// 关闭（复用）主连接。项目关闭或切换时调用，避免 socket 残留。
///
/// 先尝试 `ssh -O exit` 通过 mux 协议优雅关闭；无论成败，再直接删除
/// `ligong-ssh-*` socket 文件，确保坏 socket 不残留。
pub async fn close_master(spec: &RemoteSpec) -> Result<(), String> {
    let mut args = base_opts(spec);
    args.push("-O".into());
    args.push("exit".into());
    args.push(target_string(spec));
    let _ = TokioCommand::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    // 兜底：直接清理残留 socket 文件（坏 socket 下 -O exit 无效）。
    #[cfg(unix)]
    {
        let tmp = std::env::temp_dir();
        if let Ok(entries) = std::fs::read_dir(&tmp) {
            for e in entries.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    if name.starts_with("ligong-ssh-") {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::RemoteSpec;

    fn spec(alias: Option<&str>, port: Option<u16>, pj: Option<&str>, root: &str) -> RemoteSpec {
        RemoteSpec {
            host: "example.com".into(),
            user: Some("bob".into()),
            port,
            root: root.into(),
            ssh_alias: alias.map(|s| s.to_string()),
            proxy_jump: pj.map(|s| s.to_string()),
            restrict: false,
        }
    }

    #[test]
    fn terminal_argv_has_tt_and_no_batchmode() {
        let s = spec(None, Some(2222), None, "/srv/app");
        let (args, label) = terminal_argv(&s);
        assert_eq!(args[0], "-tt");
        assert!(!args.iter().any(|a| a.contains("BatchMode")), "终端不应禁交互: {args:?}");
        // 端口应出现。
        let pi = args.iter().position(|a| a == "-p").expect("缺 -p");
        assert_eq!(args[pi + 1], "2222");
        // 目标串 + 远端 cd 命令在末尾。
        assert!(args.iter().any(|a| a == "bob@example.com"));
        assert!(args.last().unwrap().contains("/srv/app"), "{}", args.last().unwrap());
        assert_eq!(label, "example.com");
    }

    /// 真机：terminal_argv 的选项组合（mux/port）在 localhost 上可用。
    /// 用 `-T` 覆盖 `-tt` 并替换远端命令为非交互 echo，验证选项合法性。
    #[tokio::test]
    #[ignore]
    async fn terminal_argv_localhost_exec() {
        let s = spec(Some("localhost"), None, None, "/tmp");
        let (mut args, _label) = terminal_argv(&s);
        // 去掉 -tt 与末尾远端命令，改成非交互探测。
        args.retain(|a| a != "-tt");
        args.pop();
        args.push("echo terminal-ok".into());
        let out = TokioCommand::new("ssh")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "ssh 失败: {}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stdout).contains("terminal-ok"));
    }

    #[test]
    fn terminal_argv_prefers_alias_and_proxyjump() {
        let s = spec(Some("gongwei"), None, Some("jump@10.0.0.1"), "/home/bob/x y");
        let (args, label) = terminal_argv(&s);
        let ji = args.iter().position(|a| a == "-J").expect("缺 -J");
        assert_eq!(args[ji + 1], "jump@10.0.0.1");
        assert!(args.iter().any(|a| a == "gongwei"));
        assert_eq!(label, "gongwei");
        // 含空格的 root 应被单引号包裹。
        assert!(args.last().unwrap().contains("'/home/bob/x y'"), "{}", args.last().unwrap());
    }
}