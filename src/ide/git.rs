//! IDE Git 命令：状态、diff、暂存、提交、推送/拉取。
//!
//! 全部以 `project_id` 定位工作目录，直接调用系统 `git`。
//! 输出解析遵循 porcelain 稳定格式。

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

/// 单个文件的工作区状态。
#[derive(Debug, Clone, Serialize)]
pub struct GitFile {
    /// 相对工作目录的路径（正斜杠）。
    pub rel: String,
    /// 索引区状态码（porcelain 第一列）。
    pub staged: String,
    /// 工作区状态码（porcelain 第二列）。
    pub worktree: String,
    /// 是否为未跟踪文件。
    pub untracked: bool,
    /// 是否处于冲突状态。
    pub conflicted: bool,
}

/// 仓库整体状态。
#[derive(Debug, Clone, Serialize)]
pub struct GitStatus {
    /// 是否为 git 仓库。
    pub is_repo: bool,
    /// 当前分支名（detached 时为空）。
    pub branch: String,
    /// 上游分支名（无则空）。
    pub upstream: String,
    /// 相对上游的领先/落后提交数。
    pub ahead: i64,
    pub behind: i64,
    /// 文件状态列表。
    pub files: Vec<GitFile>,
}

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

/// 同步执行 git 命令，返回 (成功?, stdout, stderr)。
fn run_git(root: &Path, args: &[&str]) -> Result<(bool, String, String), String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let out = command
        .output()
        .map_err(|e| format!("执行 git 失败：{e}"))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

/// 是否为 git 仓库。
fn is_repo(root: &Path) -> bool {
    matches!(run_git(root, &["rev-parse", "--is-inside-work-tree"]), Ok((true, _, _)))
}

/// 读取仓库状态。
#[tauri::command]
pub async fn ide_git_status(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<GitStatus, String> {
    let root = project_root(&state, &project_id).await?;
    if !is_repo(&root) {
        return Ok(GitStatus {
            is_repo: false,
            branch: String::new(),
            upstream: String::new(),
            ahead: 0,
            behind: 0,
            files: Vec::new(),
        });
    }

    let branch = run_git(&root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|(_, o, _)| o.trim().to_string())
        .unwrap_or_default();
    let branch = if branch == "HEAD" { String::new() } else { branch };

    let upstream = run_git(
        &root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .ok()
    .filter(|(ok, _, _)| *ok)
    .map(|(_, o, _)| o.trim().to_string())
    .unwrap_or_default();

    let mut ahead = 0i64;
    let mut behind = 0i64;
    if !upstream.is_empty() {
        if let Ok((true, o, _)) = run_git(
            &root,
            &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
        ) {
            let parts: Vec<&str> = o.split_whitespace().collect();
            if parts.len() == 2 {
                ahead = parts[0].parse().unwrap_or(0);
                behind = parts[1].parse().unwrap_or(0);
            }
        }
    }

    let (_, out, _) = run_git(&root, &["status", "--porcelain=v1", "-z"])?;
    let files = parse_status(&out);

    Ok(GitStatus {
        is_repo: true,
        branch,
        upstream,
        ahead,
        behind,
        files,
    })
}

/// 解析 `git status --porcelain=v1 -z` 输出。
fn parse_status(raw: &str) -> Vec<GitFile> {
    let mut files = Vec::new();
    let mut it = raw.split('\0');
    while let Some(entry) = it.next() {
        if entry.is_empty() {
            continue;
        }
        // 形如 "XY path"；rename/copy 时后随原路径条目，跳过。
        let bytes = entry.as_bytes();
        if bytes.len() <3 {
            continue;
        }
        let x = &entry[0..1];
        let y = &entry[1..2];
        let mut path = entry[3..].to_string();
        if x == "R" || x == "C" {
            // 下一个以 \0 分隔的条目是原路径，跳过它。
            let _ = it.next();
        }
        path = path.trim_matches('"').to_string();
        let staged = x.to_string();
        let worktree = y.to_string();
        let untracked = x == "?";
        let conflicted = matches!(x, "U") || matches!(y, "U") || (x == "A" && y == "A") || (x == "D" && y == "D");
        files.push(GitFile {
            rel: path,
            staged,
            worktree,
            untracked,
            conflicted,
        });
    }
    files
}

/// 取某文件的 diff 文本。`staged` 为真时取索引 vs HEAD，否则工作区 vs 索引。
#[tauri::command]
pub async fn ide_git_diff(
    project_id: String,
    rel_path: String,
    staged: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    if !is_repo(&root) {
        return Err("不是 git 仓库".into());
    }
    let mut args = vec!["diff", "--no-color", "--no-ext-diff"];
    if staged {
        args.push("--cached");
    }
    args.push("--");
    args.push(&rel_path);
    let (ok, out, err) = run_git(&root, &args)?;
    if !ok {
        return Err(format!("git diff 失败：{}", err.trim()));
    }
    Ok(out)
}

/// 暂存文件（`rel` 为空表示全部）。
#[tauri::command]
pub async fn ide_git_stage(
    project_id: String,
    rel_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = project_root(&state, &project_id).await?;
    let rel = rel_path.unwrap_or_default();
    let (ok, _, err) = if rel.is_empty() {
        run_git(&root, &["add", "-A"])?
    } else {
        run_git(&root, &["add", "--", &rel])?
    };
    if !ok {
        return Err(format!("git add 失败：{}", err.trim()));
    }
    Ok(())
}

/// 取消暂存（`rel` 为空表示全部）。
#[tauri::command]
pub async fn ide_git_unstage(
    project_id: String,
    rel_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = project_root(&state, &project_id).await?;
    let rel = rel_path.unwrap_or_default();
    let (ok, _, err) = if rel.is_empty() {
        run_git(&root, &["reset", "HEAD", "--", "."])?
    } else {
        run_git(&root, &["reset", "HEAD", "--", &rel])?
    };
    if !ok {
        return Err(format!("git reset 失败：{}", err.trim()));
    }
    Ok(())
}

/// 丢弃工作区更改（未跟踪文件则删除）。
#[tauri::command]
pub async fn ide_git_discard(
    project_id: String,
    rel_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = project_root(&state, &project_id).await?;
    let full = root.join(&rel_path);
    // 未跟踪文件：直接删除。
    let (_, ls, _) = run_git(&root, &["ls-files", "--error-unmatch", "--", &rel_path])
        .unwrap_or((false, String::new(), String::new()));
    if ls.trim().is_empty() {
        if full.is_dir() {
            std::fs::remove_dir_all(&full).map_err(|e| format!("删除失败：{e}"))?;
        } else if full.exists() {
            std::fs::remove_file(&full).map_err(|e| format!("删除失败：{e}"))?;
        }
        return Ok(());
    }
    let (ok, _, err) = run_git(&root, &["checkout", "HEAD", "--", &rel_path])?;
    if !ok {
        return Err(format!("git checkout 失败：{}", err.trim()));
    }
    Ok(())
}

/// 提交已暂存更改。
#[tauri::command]
pub async fn ide_git_commit(
    project_id: String,
    message: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("提交信息不能为空".into());
    }
    let root = project_root(&state, &project_id).await?;
    let (ok, out, err) = run_git(&root, &["commit", "-m", &message])?;
    if !ok {
        return Err(format!("git commit 失败：{}", err.trim()));
    }
    Ok(out)
}

/// 推送到上游。
#[tauri::command]
pub async fn ide_git_push(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let (ok, out, err) = run_git(&root, &["push"])?;
    if !ok {
        return Err(format!("git push 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 从上游拉取（快进合并）。
#[tauri::command]
pub async fn ide_git_pull(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let (ok, out, err) = run_git(&root, &["pull", "--ff-only"])?;
    if !ok {
        return Err(format!("git pull 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

#[cfg(test)]
mod tests {
    use super::parse_status;

    #[test]
    fn parses_modified_and_untracked() {
        let raw = " M src/lib.rs\0?? new.txt\0";
        let files = parse_status(raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].rel, "src/lib.rs");
        assert_eq!(files[0].staged, " ");
        assert_eq!(files[0].worktree, "M");
        assert!(!files[0].untracked);
        assert_eq!(files[1].rel, "new.txt");
        assert!(files[1].untracked);
    }

    #[test]
    fn parses_rename_keeping_new_path() {
        // -z 下 rename 形如 "R  new\0old\0"，新路径在前。
        let raw = "R  new.txt\0old.txt\0";
        let files = parse_status(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].rel, "new.txt");
        assert_eq!(files[0].staged, "R");
    }

    #[test]
    fn parses_conflict() {
        let raw = "UU both.txt\0";
        let files = parse_status(raw);
        assert_eq!(files.len(), 1);
        assert!(files[0].conflicted);
    }

    #[test]
    fn parses_staged_only() {
        let raw = "A  added.txt\0";
        let files = parse_status(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].staged, "A");
        assert_eq!(files[0].worktree, " ");
        assert!(!files[0].untracked);
    }
}