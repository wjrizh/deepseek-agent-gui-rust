//! IDE 独立窗口管理：每个项目一个窗口，重复打开则聚焦已有窗口。

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::state::AppState;

/// 每个项目对应的窗口 label。
fn window_label(project_id: &str) -> String {
    format!("ide-{project_id}")
}

/// 打开（或复用）某项目的 IDE 窗口。
///
/// - 已存在：`set_focus` 并返回。
/// - 不存在：新建独立窗口，加载 `ide/index.html?project=<id>&name=<项目名>&restrict=<0|1>`。
#[tauri::command]
pub async fn open_ide_window(
    app: AppHandle,
    project_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let label = window_label(&project_id);

    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.unminimize();
        let _ = win.show();
        win.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    // 取项目名、restrict 与工作目录，供 IDE 显示、越界判断与文件监听。
    let (name, restrict, workdir) = {
        let inner = state.inner.lock().await;
        let name = inner
            .projects
            .project(&project_id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| project_id.clone());
        let restrict = inner.projects.restrict_of(&project_id);
        let workdir = inner.projects.workdir_of(&project_id);
        (name, restrict, workdir)
    };

    let url = format!(
        "ide/index.html?project={}&name={}&restrict={}",
        urlencode(&project_id),
        urlencode(&name),
        if restrict { 1 } else { 0 }
    );

    let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title("力工 IDE")
        .inner_size(1180.0, 760.0)
        .min_inner_size(720.0, 480.0)
        .decorations(false)
        .build()
        .map_err(|e| format!("创建 IDE 窗口失败: {e}"))?;

    win.set_focus().map_err(|e| e.to_string())?;

    // 注册文件监听（若有工作目录），用于外部改动同步。
    if let Some(dir) = workdir {
        let root = std::path::PathBuf::from(dir);
        if root.is_dir() {
            let mut guard = state.inner.lock().await;
            if let Some(w) = guard.ide_watcher.as_mut() {
                if let Err(e) = w.add(&project_id, &root) {
                    eprintln!("注册 IDE 文件监听失败: {e}");
                }
            }
        }
    }
    Ok(())
}

/// 关闭指定项目的 IDE 窗口（若存在）。
#[tauri::command]
pub async fn close_ide_window(
    app: AppHandle,
    project_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let label = window_label(&project_id);
    if let Some(win) = app.get_webview_window(&label) {
        win.close().map_err(|e| e.to_string())?;
    }
    // 移除该项目的文件监听。
    let mut guard = state.inner.lock().await;
    if let Some(w) = guard.ide_watcher.as_mut() {
        w.remove(&project_id);
    }
    Ok(())
}

/// 极简 percent 编码，仅覆盖 URL 查询参数里可能的特殊字符。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}