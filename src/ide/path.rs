//! IDE 文件访问的路径解析与安全校验。
//!
//! 所有 IDE 读写都以"项目工作目录"为根：
//! - 输入是**项目内相对路径**（正斜杠），绝对路径也会被规范化到根下。
//! - `restrict` 开启时，拒绝任何解析到工作目录之外的路径（含 `..` 逃逸、符号链接逃逸）。
//!
//! 该逻辑与 `crate::tools::read_file::ReadFile::resolve` 同源，独立一份以便
//! IDE 侧复用且不依赖工具实例。

use std::path::{Path, PathBuf};

/// 把 IDE 传入的相对路径解析成绝对路径，并按 `restrict` 校验。
///
/// - `root`：项目工作目录（绝对路径）。
/// - `rel`：相对路径（允许为空表示根）。
/// - `restrict`：为真时，解析结果必须落在 `root` 之内。
/// - `for_write`：写入场景允许目标尚不存在，校验回退到最近的已存在祖先。
pub fn resolve(root: &Path, rel: &str, restrict: bool, for_write: bool) -> Result<PathBuf, String> {
    let cleaned = rel.replace('\\', "/");
    let rel_trim = cleaned.trim_start_matches('/');
    let target = if rel_trim.is_empty() {
        root.to_path_buf()
    } else {
        lexical_normalize(&root.join(rel_trim))
    };

    if !restrict {
        return Ok(target);
    }

    let base = root
        .canonicalize()
        .map_err(|e| format!("路径校验失败（工作目录不可解析）: {e}"))?;

    let check = if target.exists() {
        target
            .canonicalize()
            .map_err(|e| format!("路径校验失败: {e}"))?
    } else if for_write {
        nearest_existing_ancestor(&target)?
    } else {
        // 读场景下不存在即报错，交给调用方处理
        return Ok(target);
    };

    if !check.starts_with(&base) {
        return Err(format!("权限拒绝：路径 {} 超出项目工作目录限制", target.display()));
    }
    Ok(target)
}

/// 词法规范化：折叠 `.` 与 `..`，不访问文件系统（可处理尚不存在的路径）。
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                match out.last() {
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                    _ => out.push(comp),
                }
            }
            c => out.push(c),
        }
    }
    let mut buf = PathBuf::new();
    for c in out {
        buf.push(c.as_os_str());
    }
    buf
}

/// 向上找到最近的存在祖先并 canonicalize。
fn nearest_existing_ancestor(target: &Path) -> Result<PathBuf, String> {
    let mut anc = target.parent();
    while let Some(a) = anc {
        if a.exists() {
            return a.canonicalize().map_err(|e| format!("路径校验失败: {e}"));
        }
        anc = a.parent();
    }
    Ok(target.to_path_buf())
}

/// 把绝对路径转成相对工作目录根的、正斜杠分隔的路径。
/// 若不在根内，返回带 `..` 的相对路径（供 IDE 的“上一层”浏览父级）。
pub fn to_rel(root: &Path, abs: &Path) -> String {
    if let Ok(p) = abs.strip_prefix(root) {
        return p.to_string_lossy().replace('\\', "/");
    }
    // 越出根：手工构造 ../ 前缀。
    let mut ups = 0usize;
    let mut cur = root.to_path_buf();
    loop {
        // 走到共同祖先
        if abs.starts_with(&cur) {
            let tail = abs
                .strip_prefix(&cur)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let prefix = "../".repeat(ups);
            return format!("{prefix}{tail}");
        }
        match cur.parent() {
            Some(p) if p != cur => {
                cur = p.to_path_buf();
                ups += 1;
            }
            _ => return abs.to_string_lossy().replace('\\', "/"),
        }
    }
}