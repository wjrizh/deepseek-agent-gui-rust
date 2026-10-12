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

/// 分支信息。
#[derive(Debug, Clone, Serialize)]
pub struct GitBranch {
    pub name: String,
    pub current: bool,
    pub upstream: String,
}

/// 列出本地分支。
#[tauri::command]
pub async fn ide_git_branches(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<GitBranch>, String> {
    let root = project_root(&state, &project_id).await?;
    if !is_repo(&root) {
        return Ok(Vec::new());
    }
    let cur = run_git(&root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|(_, o, _)| o.trim().to_string())
        .unwrap_or_default();
    let (ok, out, err) = run_git(
        &root,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(upstream:short)",
            "refs/heads",
        ],
    )?;
    if !ok {
        return Err(format!("列出分支失败：{}", err.trim()));
    }
    let mut branches = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (name, upstream) = match line.split_once('\t') {
            Some((n, u)) => (n.to_string(), u.to_string()),
            None => (line.to_string(), String::new()),
        };
        branches.push(GitBranch {
            current: name == cur,
            name,
            upstream,
        });
    }
    branches.sort_by(|a, b| b.current.cmp(&a.current).then_with(|| a.name.cmp(&b.name)));
    Ok(branches)
}

/// 创建并切换到新分支。
#[tauri::command]
pub async fn ide_git_branch_create(
    project_id: String,
    name: String,
    from: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("分支名不能为空".into());
    }
    let root = project_root(&state, &project_id).await?;
    let base = from.unwrap_or_default();
    let args: Vec<&str> = if base.trim().is_empty() {
        vec!["checkout", "-b", name]
    } else {
        vec!["checkout", "-b", name, base.trim()]
    };
    let (ok, _, err) = run_git(&root, &args)?;
    if !ok {
        return Err(format!("创建分支失败：{}", err.trim()));
    }
    Ok(())
}

/// 切换分支。
#[tauri::command]
pub async fn ide_git_checkout(
    project_id: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let (ok, out, err) = run_git(&root, &["checkout", name.trim()])?;
    if !ok {
        return Err(format!("切换分支失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 删除本地分支。
#[tauri::command]
pub async fn ide_git_branch_delete(
    project_id: String,
    name: String,
    force: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = project_root(&state, &project_id).await?;
    let flag = if force { "-D" } else { "-d" };
    let (ok, _, err) = run_git(&root, &["branch", flag, name.trim()])?;
    if !ok {
        return Err(format!("删除分支失败：{}", err.trim()));
    }
    Ok(())
}

/// 提交日志条目。
#[derive(Debug, Clone, Serialize)]
pub struct GitCommit {
    pub hash: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub time: String,
}

/// 读取提交日志。
#[tauri::command]
pub async fn ide_git_log(
    project_id: String,
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<GitCommit>, String> {
    let root = project_root(&state, &project_id).await?;
    if !is_repo(&root) {
        return Ok(Vec::new());
    }
    let n = limit.unwrap_or(100).to_string();
    let (ok, out, err) = run_git(
        &root,
        &[
            "log",
            &format!("-n{n}"),
            "--pretty=format:%H\x1f%h\x1f%s\x1f%an\x1f%ad\x1e",
            "--date=format:%Y-%m-%d %H:%M",
        ],
    )?;
    if !ok {
        if err.contains("does not have any commits") || err.contains("unknown revision") {
            return Ok(Vec::new());
        }
        return Err(format!("读取日志失败：{}", err.trim()));
    }
    Ok(parse_log(&out))
}

/// 解析 `git log --pretty=format:%H\x1f%h\x1f%s\x1f%an\x1f%ad\x1e` 输出。
fn parse_log(raw: &str) -> Vec<GitCommit> {
    let mut commits = Vec::new();
    for rec in raw.split('\u{1e}') {
        let rec = rec.trim_matches(['\n', '\r']);
        if rec.is_empty() {
            continue;
        }
        let parts: Vec<&str> = rec.split('\u{1f}').collect();
        if parts.len() < 5 {
            continue;
        }
        commits.push(GitCommit {
            hash: parts[0].to_string(),
            short: parts[1].to_string(),
            subject: parts[2].to_string(),
            author: parts[3].to_string(),
            time: parts[4].to_string(),
        });
    }
    commits
}

/// 查看某提交的 diff。
#[tauri::command]
pub async fn ide_git_show(
    project_id: String,
    hash: String,
    rel_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let rel = rel_path.unwrap_or_default();
    let mut args = vec!["show", "--no-color", "--no-ext-diff", "--format=", hash.as_str()];
    if !rel.is_empty() {
        args.push("--");
        args.push(&rel);
    }
    let (ok, out, err) = run_git(&root, &args)?;
    if !ok {
        return Err(format!("git show 失败：{}", err.trim()));
    }
    Ok(out)
}

/// 还原：reset 到某提交（soft/mixed/hard）。
#[tauri::command]
pub async fn ide_git_reset(
    project_id: String,
    hash: String,
    mode: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let m = match mode.as_str() {
        "soft" | "mixed" | "hard" => mode.as_str(),
        _ => "mixed",
    };
    let flag = format!("--{m}");
    let (ok, out, err) = run_git(&root, &["reset", &flag, hash.trim()])?;
    if !ok {
        return Err(format!("git reset 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 从远程抓取（不合并）。
#[tauri::command]
pub async fn ide_git_fetch(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let (ok, out, err) = run_git(&root, &["fetch", "--all", "--prune"])?;
    if !ok {
        return Err(format!("git fetch 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 初始化仓库。
#[tauri::command]
pub async fn ide_git_init(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    if is_repo(&root) {
        return Ok("已是 git 仓库".into());
    }
    let (ok, out, err) = run_git(&root, &["init"])?;
    if !ok {
        return Err(format!("git init 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 克隆仓库到工作目录（要求目录为空）。
#[tauri::command]
pub async fn ide_git_clone(
    project_id: String,
    url: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("仓库地址不能为空".into());
    }
    let root = project_root(&state, &project_id).await?;
    if is_repo(&root) {
        return Err("当前目录已是 git 仓库".into());
    }
    let empty = std::fs::read_dir(&root)
        .map_err(|e| format!("读取目录失败：{e}"))?
        .next()
        .is_none();
    if !empty {
        return Err("目录非空，无法克隆".into());
    }
    let parent = root.parent().ok_or_else(|| "无效的工作目录".to_string())?;
    let name = root
        .file_name()
        .ok_or_else(|| "无效的工作目录".to_string())?
        .to_string_lossy()
        .to_string();
    std::fs::remove_dir(&root).map_err(|e| format!("删除空目录失败：{e}"))?;
    let (ok, out, err) = run_git(parent, &["clone", url, &name])?;
    if !ok {
        return Err(format!("git clone 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 合并指定分支到当前分支。发生冲突时返回文本（含 CONFLICT）由前端提示。
#[tauri::command]
pub async fn ide_git_merge(
    project_id: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("分支名不能为空".into());
    }
    let root = project_root(&state, &project_id).await?;
    let (ok, out, err) = run_git(&root, &["merge", "--no-edit", name])?;
    let text = format!("{}{}", out, err);
    if !ok {
        if text.contains("CONFLICT") || text.contains("Automatic merge failed") {
            return Ok(text);
        }
        return Err(format!("git merge 失败：{}", err.trim()));
    }
    Ok(text)
}

/// 中止合并。
#[tauri::command]
pub async fn ide_git_merge_abort(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let (ok, out, err) = run_git(&root, &["merge", "--abort"])?;
    if !ok {
        return Err(format!("git merge --abort 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 冲突解决：按 `ours`/`theirs` 取一侧并暂存该文件。
#[tauri::command]
pub async fn ide_git_resolve(
    project_id: String,
    rel_path: String,
    side: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let flag = match side.as_str() {
        "ours" => "--ours",
        "theirs" => "--theirs",
        _ => return Err("side 只能是 ours 或 theirs".into()),
    };
    let root = project_root(&state, &project_id).await?;
    let (ok, _, err) = run_git(&root, &["checkout", flag, "--", &rel_path])?;
    if !ok {
        return Err(format!("git checkout 冲突侧失败：{}", err.trim()));
    }
    let (ok, _, err) = run_git(&root, &["add", "--", &rel_path])?;
    if !ok {
        return Err(format!("git add 失败：{}", err.trim()));
    }
    Ok(())
}

/// 暂存当前改动（包含未跟踪文件）。
#[tauri::command]
pub async fn ide_git_stash_push(
    project_id: String,
    message: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let msg = message.unwrap_or_default();
    let args: Vec<&str> = if msg.trim().is_empty() {
        vec!["stash", "push", "-u"]
    } else {
        vec!["stash", "push", "-u", "-m", msg.trim()]
    };
    let (ok, out, err) = run_git(&root, &args)?;
    if !ok {
        return Err(format!("git stash 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 暂存列表条目。
#[derive(Debug, Clone, Serialize)]
pub struct GitStash {
    pub index: u32,
    pub name: String,
    pub message: String,
}

/// 列出 stash。
#[tauri::command]
pub async fn ide_git_stash_list(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<GitStash>, String> {
    let root = project_root(&state, &project_id).await?;
    if !is_repo(&root) {
        return Ok(Vec::new());
    }
    let (ok, out, err) = run_git(&root, &["stash", "list", "--format=%gd\u{1f}%s\u{1e}"])?;
    if !ok {
        return Err(format!("git stash list 失败：{}", err.trim()));
    }
    let mut list = Vec::new();
    for (i, rec) in out.split('\u{1e}').enumerate() {
        let rec = rec.trim_matches(['\n', '\r']);
        if rec.is_empty() {
            continue;
        }
        let mut parts = rec.splitn(2, '\u{1f}');
        let name = parts.next().unwrap_or("").trim().to_string();
        let message = parts.next().unwrap_or("").to_string();
        list.push(GitStash {
            index: i as u32,
            name,
            message,
        });
    }
    Ok(list)
}

/// 弹出/应用/删除 stash。`action` 取 pop|apply|drop。
#[tauri::command]
pub async fn ide_git_stash_op(
    project_id: String,
    action: String,
    index: u32,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let act = match action.as_str() {
        "pop" | "apply" | "drop" => action.as_str(),
        _ => return Err("action 只能是 pop/apply/drop".into()),
    };
    let root = project_root(&state, &project_id).await?;
    let spec = format!("stash@{{{index}}}");
    let (ok, out, err) = run_git(&root, &["stash", act, &spec])?;
    if !ok {
        return Err(format!("git stash {act} 失败：{}", err.trim()));
    }
    Ok(format!("{}{}", out, err))
}

/// 单文件提交历史。
#[tauri::command]
pub async fn ide_git_file_log(
    project_id: String,
    rel_path: String,
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<GitCommit>, String> {
    let root = project_root(&state, &project_id).await?;
    if !is_repo(&root) {
        return Ok(Vec::new());
    }
    let n = limit.unwrap_or(100).to_string();
    let (ok, out, err) = run_git(
        &root,
        &[
            "log",
            &format!("-n{n}"),
            "--pretty=format:%H\u{1f}%h\u{1f}%s\u{1f}%an\u{1f}%ad\u{1e}",
            "--date=format:%Y-%m-%d %H:%M",
            "--",
            &rel_path,
        ],
    )?;
    if !ok {
        return Err(format!("读取文件历史失败：{}", err.trim()));
    }
    Ok(parse_log(&out))
}

/// blame 单行。
#[derive(Debug, Clone, Serialize)]
pub struct GitBlameLine {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub time: String,
    pub line: u32,
    pub text: String,
}

/// 逐行 blame。
#[tauri::command]
pub async fn ide_git_blame(
    project_id: String,
    rel_path: String,
    state: State<'_, AppState>,
) -> Result<Vec<GitBlameLine>, String> {
    let root = project_root(&state, &project_id).await?;
    if !is_repo(&root) {
        return Ok(Vec::new());
    }
    let (ok, out, err) = run_git(
        &root,
        &["blame", "--line-porcelain", "--date=format:%Y-%m-%d %H:%M", "--", &rel_path],
    )?;
    if !ok {
        return Err(format!("git blame 失败：{}", err.trim()));
    }
    Ok(parse_blame(&out))
}

/// 解析 `git blame --line-porcelain` 输出。
fn parse_blame(raw: &str) -> Vec<GitBlameLine> {
    let mut lines = Vec::new();
    let mut hash = String::new();
    let mut author = String::new();
    let mut time = String::new();
    let mut lineno = 0u32;
    for line in raw.split('\n') {
        if let Some(rest) = line.strip_prefix("author ") {
            author = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            time = rest.to_string();
        } else if let Some(text) = line.strip_prefix('\t') {
            lines.push(GitBlameLine {
                hash: hash.clone(),
                short: hash.chars().take(8).collect(),
                author: author.clone(),
                time: time.clone(),
                line: lineno,
                text: text.to_string(),
            });
        } else {
            let mut it = line.split_whitespace();
            if let Some(sha) = it.next() {
                if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    hash = sha.to_string();
                    author.clear();
                    time.clear();
                    if let Some(l) = it.nth(1) {
                        lineno = l.parse().unwrap_or(0);
                    }
                }
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{parse_log, parse_status};

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

    #[test]
    fn parses_multiple_log_records() {
        // 记录以 \x1e 分隔，字段以 \x1f 分隔。
        let rec = "\u{1f}";
        let raw = format!(
            "aaa{0}aaaa{0}first{0}alice{0}2026-01-01 10:00\u{1e}bbb{0}bbbb{0}second{0}bob{0}2026-01-02 11:00\u{1e}",
            rec
        );
        let commits = parse_log(&raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "aaa");
        assert_eq!(commits[0].subject, "first");
        assert_eq!(commits[0].author, "alice");
        assert_eq!(commits[1].hash, "bbb");
        assert_eq!(commits[1].subject, "second");
    }

    #[test]
    fn parses_blame_porcelain_line_numbers() {
        let raw = "0123456789abcdef0123456789abcdef01234567 1 1 1\nauthor Alice\nauthor-time 1700000000\n\tfirst line\n89abcdef0123456789abcdef0123456789abcdef 2 2 1\nauthor Bob\nauthor-time 1700000001\n\tsecond line\n";
        let blame = super::parse_blame(raw);
        assert_eq!(blame.len(), 2);
        assert_eq!(blame[0].line, 1);
        assert_eq!(blame[0].text, "first line");
        assert_eq!(blame[1].line, 2);
        assert_eq!(blame[1].text, "second line");
    }
}