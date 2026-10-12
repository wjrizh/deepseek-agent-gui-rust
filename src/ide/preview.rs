//! IDE 文件预览命令：二进制读取（图片/PDF）与"用系统程序打开"。
//!
//! 预览只读，安全由 `path::resolve` + 项目 restrict 统一保证。
//! 二进制内容以 base64 返回，前端拼 `data:` URL 或交给 pdf.js。

use base64::Engine;
use serde::Serialize;
use tauri::State;

use crate::ide::path;
use crate::state::AppState;

/// 二进制预览的大小上限（超过拒绝，避免内存/传输爆掉）。
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

/// 二进制读取结果：MIME 类型 + base64 内容。
#[derive(Debug, Clone, Serialize)]
pub struct BinaryData {
    /// 推测的 MIME 类型（按扩展名）。
    pub mime: String,
    /// base64 编码的文件内容。
    pub base64: String,
    /// 文件大小（字节）。
    pub size: u64,
}

async fn project_root(
    state: &State<'_, AppState>,
    project_id: &str,
) -> Result<std::path::PathBuf, String> {
    let inner = state.inner.lock().await;
    let dir = inner
        .projects
        .workdir_of(project_id)
        .ok_or_else(|| "项目未设置工作目录".to_string())?;
    let root = std::path::PathBuf::from(dir);
    if !root.is_dir() {
        return Err(format!("工作目录不存在：{}", root.display()));
    }
    Ok(root)
}

async fn project_restrict(state: &State<'_, AppState>, project_id: &str) -> bool {
    let inner = state.inner.lock().await;
    inner.projects.restrict_of(project_id)
}

/// 按扩展名推测 MIME 类型。
fn mime_for_path(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// 读取文件为 base64（供图片、PDF 预览）。
#[tauri::command]
pub async fn ide_read_binary(
    project_id: String,
    rel_path: String,
    state: State<'_, AppState>,
) -> Result<BinaryData, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let file = path::resolve(&root, &rel_path, restrict, false)?;

    if file.is_dir() {
        return Err(format!("是目录，不是文件：{}", file.display()));
    }
    let meta = std::fs::metadata(&file).map_err(|e| format!("读取失败: {e}"))?;
    if meta.len() > MAX_BINARY_BYTES {
        return Err(format!(
            "文件过大（{} 字节，上限 {} 字节），IDE 不预览",
            meta.len(),
            MAX_BINARY_BYTES
        ));
    }

    let bytes = std::fs::read(&file).map_err(|e| format!("读取失败: {e}"))?;
    let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(BinaryData {
        mime: mime_for_path(&file),
        base64,
        size: meta.len(),
    })
}

/// 用系统默认程序打开文件（图片/PDF/Office 等进入外部编辑器）。
#[tauri::command]
pub async fn ide_open_external(
    project_id: String,
    rel_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let file = path::resolve(&root, &rel_path, restrict, false)?;

    if !file.exists() {
        return Err(format!("文件不存在：{}", file.display()));
    }

    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";

    let mut cmd = tokio::process::Command::new(program);
    cmd.arg(&file);
    // 分离进程，不等待其退出（外部编辑器通常长时间驻留）。
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008); // DETACHED_PROCESS
    }
    cmd.spawn()
        .map_err(|e| format!("无法用系统程序打开（{}）: {e}", program))?;
    Ok(())
}