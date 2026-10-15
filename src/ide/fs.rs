//! IDE 文件系统命令：列目录、读文本、写文本。
//!
//! 全部以 `project_id` 定位工作目录，并遵守该项目的 `restrict` 设置。
//! 依赖 `AppState` 读取项目元信息，因此从命令层（`crate::commands`）调用。

use serde::Serialize;
use tauri::State;

use crate::ide::path;
use crate::state::AppState;

/// 文件树/列表条目。
#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    /// 相对工作目录的路径（正斜杠）。
    pub rel: String,
    /// 文件/目录名。
    pub name: String,
    pub is_dir: bool,
    /// 文件大小（字节）；目录为 0。
    pub size: u64,
    /// 符号链接标记。
    pub is_symlink: bool,
}

/// 读取文本文件的大小上限（超过则拒绝，避免 IDE 卡死）。
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// 自然序比较：连续数字按数值比较（file2 <file10），其余不区分大小写。
fn natord_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ia = a.chars().peekable();
    let mut ib = b.chars().peekable();
    while let (Some(&ca), Some(&cb)) = (ia.peek(), ib.peek()) {
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let mut na: u64 = 0;
            while let Some(d) = ia.peek().and_then(|c| c.to_digit(10)) {
                na = na.saturating_mul(10).saturating_add(d as u64);
                ia.next();
            }
            let mut nb: u64 = 0;
            while let Some(d) = ib.peek().and_then(|c| c.to_digit(10)) {
                nb = nb.saturating_mul(10).saturating_add(d as u64);
                ib.next();
            }
            match na.cmp(&nb) {
                std::cmp::Ordering::Equal => continue,
                ord => return ord,
            }
        }
        let la = ca.to_ascii_lowercase();
        let lb = cb.to_ascii_lowercase();
        match la.cmp(&lb) {
            std::cmp::Ordering::Equal => { ia.next(); ib.next(); }
            ord => return ord,
        }
    }
    ia.count().cmp(&ib.count())
}

/// 对给定的绝对路径及其父目录调用 watcher 抑制，避免 IDE 自身写盘触发回灌事件。
async fn suppress_paths(state: &State<'_, AppState>, paths: &[std::path::PathBuf]) {
    let inner = state.inner.lock().await;
    if let Some(w) = &inner.ide_watcher {
        for p in paths {
            w.suppress_path(p);
            if let Some(parent) = p.parent() {
                w.suppress_path(parent);
            }
        }
    }
}

/// IDE 删除暂存目录名（位于工作目录根，不显示在文件树中）。
pub const TRASH_DIR: &str = ".ligong-ide-trash";

/// 取得项目工作目录（绝对路径）。不存在则报错。
async fn project_root(state: &State<'_, AppState>, project_id: &str) -> Result<std::path::PathBuf, String> {
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

/// 列出一个目录下的条目（不递归）。`rel_path` 为空表示工作目录根。
#[tauri::command]
pub async fn ide_list_dir(
    project_id: String,
    rel_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<DirEntry>, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let rel = rel_path.unwrap_or_default();
    let dir = path::resolve(&root, &rel, restrict, false)?;

    if !dir.is_dir() {
        return Err(format!("不是目录：{}", dir.display()));
    }

    let mut out: Vec<DirEntry> = Vec::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("读取目录失败: {e}"))?;
    for ent in rd {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = ent.file_name().to_string_lossy().to_string();
        if name == TRASH_DIR {
            continue;
        }
        let full = ent.path();
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_symlink = meta.file_type().is_symlink();
        // 对符号链接，尽量取指向目标的类型（失败则按文件处理）
        let is_dir = if is_symlink {
            std::fs::metadata(&full).map(|m| m.is_dir()).unwrap_or(false)
        } else {
            meta.is_dir()
        };
        let size = if is_dir { 0 } else { meta.len() };
        out.push(DirEntry {
            rel: path::to_rel(&root, &full),
            name,
            is_dir,
            size,
            is_symlink,
        });
    }

    // 目录优先，其次按名称自然序（不区分大小写，file2 排在 file10 前）。
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natord_cmp(&a.name, &b.name)));
    Ok(out)
}

/// 读取文本文件内容。二进制文件返回错误提示。
#[tauri::command]
pub async fn ide_read_file(
    project_id: String,
    rel_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let file = path::resolve(&root, &rel_path, restrict, false)?;

    if file.is_dir() {
        return Err(format!("是目录，不是文件：{}", file.display()));
    }
    let meta = std::fs::metadata(&file).map_err(|e| format!("读取失败: {e}"))?;
    if meta.len() > MAX_READ_BYTES {
        return Err(format!(
            "文件过大（{} 字节，上限 {} 字节），IDE 不加载",
            meta.len(),
            MAX_READ_BYTES
        ));
    }

    let bytes = std::fs::read(&file).map_err(|e| format!("读取失败: {e}"))?;
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err("二进制文件，无法以文本打开".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "文件不是合法 UTF-8 文本".to_string())
}

/// 保存（覆盖写）文本文件。父目录不存在则创建。
#[tauri::command]
pub async fn ide_write_file(
    project_id: String,
    rel_path: String,
    content: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let file = path::resolve(&root, &rel_path, restrict, true)?;

    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    // 原子写：先写临时文件再重命名，避免写一半损坏。
    let tmp = file.with_extension("ide-tmp");
    std::fs::write(&tmp, content.as_bytes()).map_err(|e| format!("写入失败: {e}"))?;
    std::fs::rename(&tmp, &file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("保存失败: {e}")
    })?;
    Ok(())
}

/// 删除（移入暂存区 `.ligong-ide-trash/<id>/<name>`，可恢复）。返回暂存相对路径。
#[tauri::command]
pub async fn ide_delete(
    project_id: String,
    rel_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let target = path::resolve(&root, &rel_path, restrict, false)?;

    if !target.exists() {
        return Err(format!("目标不存在：{}", target.display()));
    }
    // 拒绝删除工作目录根本身 / 暂存目录自身。
    let base = root.canonicalize().map_err(|e| e.to_string())?;
    let tcanon = target.canonicalize().map_err(|e| e.to_string())?;
    if tcanon == base {
        return Err("不能删除工作目录根".to_string());
    }
    if tcanon.starts_with(base.join(TRASH_DIR)) {
        return Err("不能删除暂存目录".to_string());
    }
    let name = target
        .file_name()
        .ok_or_else(|| "无法取得名称".to_string())?
        .to_string_lossy()
        .to_string();
    // 用纳秒时间戳做唯一子目录，避免同名覆盖。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("{nanos}");
    let trash_root = root.join(TRASH_DIR);
    let slot = trash_root.join(&id);
    std::fs::create_dir_all(&slot).map_err(|e| format!("创建暂存目录失败: {e}"))?;
    let dest = slot.join(&name);
    suppress_paths(&state, &[target.clone()]).await;
    std::fs::rename(&target, &dest).map_err(|e| format!("移入暂存区失败: {e}"))?;
    // 记录原始相对路径，供恢复时还原位置。
    let orig = path::to_rel(&root, &target);
    std::fs::write(slot.join(".orig"), orig.as_bytes())
        .map_err(|e| format!("记录暂存元信息失败: {e}"))?;
    Ok(format!("{TRASH_DIR}/{id}"))
}

/// 从暂存区恢复到原相对路径（或指定 dest_rel）。`trash_rel` 形如 `.ligong-ide-trash/<id>`。
#[tauri::command]
pub async fn ide_restore(
    project_id: String,
    trash_rel: String,
    dest_rel: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let slot = path::resolve(&root, &trash_rel, restrict, false)?;
    if !slot.is_dir() {
        return Err(format!("暂存项不存在：{}", slot.display()));
    }
    // 找到 slot 下唯一的实体（除 .orig 外）。
    let mut payload: Option<std::path::PathBuf> = None;
    for ent in std::fs::read_dir(&slot).map_err(|e| e.to_string())? {
        let ent = ent.map_err(|e| e.to_string())?;
        if ent.file_name() == ".orig" {
            continue;
        }
        payload = Some(ent.path());
        break;
    }
    let payload = payload.ok_or_else(|| "暂存项为空".to_string())?;
    // 目标位置：显式 dest_rel，或读取 .orig 记录的原始位置。
    let dest_rel = match dest_rel {
        Some(d) => d,
        None => std::fs::read_to_string(slot.join(".orig"))
            .map_err(|e| format!("读取暂存元信息失败: {e}"))?,
    };
    let dest = path::resolve(&root, &dest_rel, restrict, true)?;
    if dest.exists() {
        return Err(format!("恢复位置已存在：{}", path::to_rel(&root, &dest)));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::rename(&payload, &dest).map_err(|e| format!("恢复失败: {e}"))?;
    // 清理空的暂存槽。
    let _ = std::fs::remove_dir_all(&slot);
    Ok(path::to_rel(&root, &dest))
}

/// 彻底清理暂存区（IDE 关闭时调用）。
#[tauri::command]
pub async fn ide_purge_trash(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = project_root(&state, &project_id).await?;
    let trash = root.join(TRASH_DIR);
    if trash.is_dir() {
        let _ = std::fs::remove_dir_all(&trash);
    }
    Ok(())
}

/// 复制文件或目录到 `dest_dir` 下（保持原名）。目录递归复制。
#[tauri::command]
pub async fn ide_copy(
    project_id: String,
    rel_path: String,
    dest_dir: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let src = path::resolve(&root, &rel_path, restrict, false)?;
    let dest = path::resolve(&root, &dest_dir, restrict, true)?;

    if !src.exists() {
        return Err(format!("源不存在：{}", src.display()));
    }
    if !dest.is_dir() {
        return Err(format!("目标目录不存在：{}", dest.display()));
    }
    let name = src
        .file_name()
        .ok_or_else(|| "无法取得源名称".to_string())?;
    let target = dest.join(name);
    if target.exists() {
        return Err(format!("目标已存在：{}", path::to_rel(&root, &target)));
    }
    suppress_paths(&state, &[target.clone()]).await;
    copy_recursive(&src, &target)?;
    Ok(path::to_rel(&root, &target))
}

/// 移动（重命名）文件或目录到 `dest_dir` 下。`new_name` 非空时改名，否则保持原名。
#[tauri::command]
pub async fn ide_move(
    project_id: String,
    rel_path: String,
    dest_dir: String,
    new_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let src = path::resolve(&root, &rel_path, restrict, false)?;
    let dest = path::resolve(&root, &dest_dir, restrict, true)?;

    if !src.exists() {
        return Err(format!("源不存在：{}", src.display()));
    }
    if !dest.is_dir() {
        return Err(format!("目标目录不存在：{}", dest.display()));
    }
    let name = match new_name {
        Some(n) if !n.trim().is_empty() => {
            let n = n.trim().to_string();
            if n.contains('/') || n.contains('\\') {
                return Err("名称不能包含路径分隔符".to_string());
            }
            std::ffi::OsString::from(n)
        }
        _ => src
            .file_name()
            .ok_or_else(|| "无法取得源名称".to_string())?
            .to_os_string(),
    };
    let target = dest.join(&name);
    if target == src {
        return Ok(path::to_rel(&root, &src));
    }
    if target.exists() {
        return Err(format!("目标已存在：{}", path::to_rel(&root, &target)));
    }
    suppress_paths(&state, &[src.clone(), target.clone()]).await;
    std::fs::rename(&src, &target).map_err(|e| format!("移动失败: {e}"))?;
    Ok(path::to_rel(&root, &target))
}

/// 在 `parent_dir` 下新建文件或空目录。`is_dir` 为真时建目录。
#[tauri::command]
pub async fn ide_create(
    project_id: String,
    parent_dir: String,
    name: String,
    is_dir: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let parent = path::resolve(&root, &parent_dir, restrict, true)?;

    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("名称不能为空".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("名称不能包含路径分隔符".to_string());
    }
    if !parent.is_dir() {
        return Err(format!("父目录不存在：{}", parent.display()));
    }
    let target = parent.join(trimmed);
    if target.exists() {
        return Err(format!("已存在：{}", path::to_rel(&root, &target)));
    }
    suppress_paths(&state, &[target.clone()]).await;
    if is_dir {
        std::fs::create_dir(&target).map_err(|e| format!("创建目录失败: {e}"))?;
    } else {
        std::fs::write(&target, b"").map_err(|e| format!("创建文件失败: {e}"))?;
    }
    Ok(path::to_rel(&root, &target))
}

/// 递归复制文件/目录。
fn copy_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("读取源失败: {e}"))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("创建目录失败: {e}"))?;
        let rd = std::fs::read_dir(src).map_err(|e| format!("读取目录失败: {e}"))?;
        for ent in rd {
            let ent = ent.map_err(|e| format!("读取目录项失败: {e}"))?;
            copy_recursive(&ent.path(), &dst.join(ent.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst).map_err(|e| format!("复制文件失败: {e}"))?;
    }
    Ok(())
}