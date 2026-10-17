//! IDE 文件传输：本地 ↔ 远程项目目录的上传/下载。
//!
//! 上传：读本地文件 → base64 → 远端写二进制（POSIX 走 base64 -d，Windows 走 [IO.File]）。
//! 下载：读远端文件（复用 backend::read_binary）→ 写本地。
//! 单文件上限与 preview 一致（64 MiB）。

use serde::Serialize;
use tauri::State;

use crate::ide::backend;
use crate::ide::fs;
use crate::ide::ssh;
use crate::state::AppState;

/// 单文件传输上限（与 preview 一致）。
const MAX_XFER_BYTES: u64 = 64 * 1024 * 1024;

/// 上传结果（成功条数 + 失败明细）。
#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub uploaded: usize,
    pub failed: Vec<String>,
}

/// 把本地文件写入远程项目目录（rel_dest 为远程相对路径）。
#[tauri::command]
pub async fn ide_upload_file(
    project_id: String,
    local_path: String,
    rel_dest: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let spec = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .remote_of(&project_id)
            .ok_or_else(|| "仅远程项目支持上传".to_string())?
    };
    let meta = std::fs::metadata(&local_path).map_err(|e| format!("读取本地文件失败：{e}"))?;
    if meta.len() > MAX_XFER_BYTES {
        return Err(format!(
            "文件过大（{} 字节，上限 {MAX_XFER_BYTES} 字节）",
            meta.len()
        ));
    }
    let bytes = std::fs::read(&local_path).map_err(|e| format!("读取本地文件失败：{e}"))?;
    write_remote_binary(&spec, &rel_dest, &bytes).await
}

/// 从远程项目目录下载文件到本地路径。
#[tauri::command]
pub async fn ide_download_file(
    project_id: String,
    rel_path: String,
    local_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let spec = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .remote_of(&project_id)
            .ok_or_else(|| "仅远程项目支持下载".to_string())?
    };
    let (bytes, _size) = backend::read_binary(&spec, &rel_path).await?;
    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&local_path, &bytes).map_err(|e| format!("写入本地文件失败：{e}"))?;
    Ok(())
}

/// 从远程取文件到本地临时目录，返回本地绝对路径。
///
/// 供 `upload_file` 等需要本地文件的场景使用。临时文件落到
/// `~/.deepseek-agent/remote-uploads/<project_id>/<hash>_<name>`，
/// 按内容路径 hash 天然去重，同名不冲突；按项目分目录便于统一清理。
#[tauri::command]
pub async fn remote_fetch_to_local(
    project_id: String,
    rel: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let spec = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .remote_of(&project_id)
            .ok_or_else(|| "仅远程项目支持".to_string())?
    };
    let (bytes, _size) = backend::read_binary(&spec, &rel)
        .await
        .map_err(|e| format!("远程读取失败：{e}"))?;

    let name = rel.rsplit(['/', '\\']).next().unwrap_or("file");
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        rel.hash(&mut h);
        format!("{:x}", h.finish())
    };
    let dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".deepseek-agent")
        .join("remote-uploads")
        .join(&project_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
    let local = dir.join(format!("{hash}_{name}"));
    std::fs::write(&local, &bytes)
        .map_err(|e| format!("上传失败：写入临时文件出错：{e}"))?;
    Ok(local.to_string_lossy().to_string())
}

/// 批量上传：`local_paths` 中每个文件放到远程目录 `rel_dir`（同名）。
#[tauri::command]
pub async fn ide_upload_paths(
    project_id: String,
    local_paths: Vec<String>,
    rel_dir: String,
    state: State<'_, AppState>,
) -> Result<UploadResult, String> {
    let spec = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .remote_of(&project_id)
            .ok_or_else(|| "仅远程项目支持上传".to_string())?
    };
    let base = rel_dir.trim_matches(['/', '\\']);
    let mut result = UploadResult {
        uploaded: 0,
        failed: Vec::new(),
    };
    for lp in local_paths {
        let name = match std::path::Path::new(&lp).file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => {
                result.failed.push(format!("{lp}：无法取文件名"));
                continue;
            }
        };
        let rel = if base.is_empty() {
            name
        } else {
            format!("{base}/{name}")
        };
        let bytes = match std::fs::read(&lp) {
            Ok(b) => b,
            Err(e) => {
                result.failed.push(format!("{lp}：{e}"));
                continue;
            }
        };
        if bytes.len() as u64 > MAX_XFER_BYTES {
            result.failed.push(format!("{lp}：文件过大"));
            continue;
        }
        match write_remote_binary(&spec, &rel, &bytes).await {
            Ok(()) => result.uploaded += 1,
            Err(e) => result.failed.push(format!("{lp}：{e}")),
        }
    }
    Ok(result)
}

/// 列出远程目录下的普通文件（相对路径），供“下载目录”批量化（此处仅单层）。
#[tauri::command]
pub async fn ide_list_remote_files(
    project_id: String,
    rel_dir: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let entries = fs::ide_list_dir(project_id, Some(rel_dir), state).await?;
    Ok(entries
        .into_iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.rel)
        .collect())
}

/// 把字节写入远程（按远端平台分派 POSIX/Windows）。
pub async fn write_remote_binary(
    spec: &crate::projects::RemoteSpec,
    rel: &str,
    bytes: &[u8],
) -> Result<(), String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);

    if backend::is_windows_remote(spec).await {
        let abs = backend::win_resolve(&spec.root, rel, spec.restrict)?;
        let script = r#"
$ErrorActionPreference='Stop'
$p=__P__
$parent=Split-Path -Parent $p
if ($parent -and -not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$b64=[Console]::In.ReadToEnd()
[IO.File]::WriteAllBytes($p,[Convert]::FromBase64String($b64.Trim()))
"#
        .replace("__P__", &backend::ps_lit(&abs));
        let out = ssh::exec_ps_stdin(spec, &script, b64.as_bytes(), std::time::Duration::from_secs(60)).await?;
        if !out.ok() {
            return Err(format!("远程写入失败：{}", out.stderr.trim()));
        }
        return Ok(());
    }

    let abs = backend::posix_resolve(&spec.root, rel, spec.restrict)?;
    let parent = match abs.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => abs[..i].to_string(),
    };
    let cmd = format!(
        "mkdir -p {p} && base64 -d > {a}",
        p = backend::shell_quote(&parent),
        a = backend::shell_quote(&abs),
    );
    let out = ssh::exec_stdin(spec, &cmd, b64.as_bytes(), std::time::Duration::from_secs(60)).await?;
    if !out.ok() {
        return Err(format!("远程写入失败：{}", out.stderr.trim()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::RemoteSpec;

    fn localhost_spec(root: &str) -> RemoteSpec {
        RemoteSpec {
            host: "localhost".into(),
            user: None,
            port: None,
            root: root.to_string(),
            ssh_alias: Some("localhost".into()),
            proxy_jump: None,
            restrict: true,
        }
    }

    #[tokio::test]
    #[ignore]
    async fn remote_write_binary_localhost() {
        let dir = format!("/tmp/ligong-xfer-test-{}", std::process::id());
        let spec = localhost_spec(&dir);
        ssh::exec(&spec, &format!("rm -rf {d} && mkdir -p {d}", d = dir))
            .await
            .unwrap();
        let payload: Vec<u8> = (0u8..=255).collect();
        write_remote_binary(&spec, "bin/data.bin", &payload).await.unwrap();
        let (got, _) = backend::read_binary(&spec, "bin/data.bin").await.unwrap();
        assert_eq!(got, payload);
        ssh::exec(&spec, &format!("rm -rf {dir}")).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn remote_write_binary_windows() {
        let spec = RemoteSpec {
            host: "gongwei".into(),
            user: None,
            port: None,
            root: "C:/Users/z/ligong-xfer-test".into(),
            ssh_alias: Some("gongwei".into()),
            proxy_jump: None,
            restrict: true,
        };
        assert!(backend::is_windows_remote(&spec).await);
        let payload: Vec<u8> = vec![0, 1, 2, 255, 254, 10, 13];
        write_remote_binary(&spec, "sub/中文.bin", &payload).await.unwrap();
        let (got, _) = backend::read_binary(&spec, "sub/中文.bin").await.unwrap();
        assert_eq!(got, payload);
        backend::delete(&spec, "sub").await.unwrap();
    }
}