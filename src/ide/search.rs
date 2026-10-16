//! IDE 内容搜索：本地 rg（回退 grep），远程经 ssh 执行 rg/grep（Windows 用 Select-String）。
//!
//! 按 project_id 分派：本地项目走本机进程，远程项目走 ssh 在远端执行。
//! 结果条数有上限；查询以固定字符串（-F）匹配，不走正则。

use std::time::Duration;

use serde::Serialize;
use tauri::State;

use crate::ide::backend;
use crate::ide::ssh;
use crate::projects::RemoteSpec;
use crate::state::AppState;

/// 单条搜索结果。
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// 相对工作目录的路径（正斜杠）。
    pub rel: String,
    /// 行号（1 起）。
    pub line: u32,
    /// 列号（1 起；0 表示工具未提供）。
    pub col: u32,
    /// 命中行文本。
    pub text: String,
}

/// 结果上限。
const MAX_HITS: usize = 500;
/// 搜索超时。
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);

/// 去掉结果行前缀的 root 路径，统一为正斜杠相对路径。
fn strip_root(line: &str, root: &str) -> String {
    let line_u = line.replace('\\', "/");
    let root_u = root.replace('\\', "/");
    let r = root_u.trim_end_matches('/');
    let s = line_u.strip_prefix(r).unwrap_or(&line_u);
    s.trim_start_matches('/').to_string()
}

/// 解析单行结果：path:line:col:text（rg）或 path:line:text（grep/Select-String）。
fn parse_line(line: &str, root: &str) -> Option<SearchHit> {
    if line.is_empty() {
        return None;
    }
    let s = strip_root(line, root);
    let mut it = s.splitn(3, ':');
    let rel = it.next()?;
    let line_no: u32 = it.next()?.trim().parse().ok()?;
    let rem = it.next().unwrap_or("");
    let (col, text) = match rem.split_once(':') {
        Some((c, t)) if !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()) => {
            (c.parse::<u32>().unwrap_or(0), t.to_string())
        }
        _ => (0, rem.to_string()),
    };
    if rel.is_empty() {
        return None;
    }
    Some(SearchHit {
        rel: rel.to_string(),
        line: line_no,
        col,
        text,
    })
}

fn parse_output(out: &str, root: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for line in out.lines() {
        if hits.len() >= MAX_HITS {
            break;
        }
        if let Some(h) = parse_line(line, root) {
            hits.push(h);
        }
    }
    hits
}

/// 本机执行一条搜索工具，返回 (退出码, stdout)。
async fn run_local(
    prog: &str,
    args: &[String],
    cwd: &std::path::Path,
) -> std::io::Result<(i32, String)> {
    let out = tokio::process::Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    ))
}

/// 本地搜索：优先 rg，缺失或出错时回退 grep。
async fn local_search(root: &std::path::Path, query: &str) -> Result<Vec<SearchHit>, String> {
    let root_s = root.to_string_lossy().to_string();
    let rg_args: Vec<String> = [
        "--no-heading",
        "-n",
        "--column",
        "--color",
        "never",
        "-F",
        "-e",
        query,
        "--",
        &root_s,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Ok((code, out)) = run_local("rg", &rg_args, root).await {
        if code == 0 || code == 1 {
            return Ok(parse_output(&out, &root_s));
        }
    }

    let grep_args: Vec<String> = ["-rn", "-F", "-e", query, "--", &root_s]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (_, out) = run_local("grep", &grep_args, root)
        .await
        .map_err(|e| format!("搜索失败（rg/grep 均不可用）：{e}"))?;
    Ok(parse_output(&out, &root_s))
}

/// 远程搜索。
async fn remote_search(spec: &RemoteSpec, query: &str) -> Result<Vec<SearchHit>, String> {
    let root = spec.root.trim_end_matches(['/', '\\']).to_string();
    if backend::is_windows_remote(spec).await {
        let script = format!(
            "$ErrorActionPreference='SilentlyContinue'; $base=(Resolve-Path -LiteralPath {r}).Path; \
             Get-ChildItem -LiteralPath $base -Recurse -File -Force | \
             Select-String -Pattern {q} -SimpleMatch | ForEach-Object {{ \
             $rel=$_.Path.Substring($base.Length).TrimStart('\\','/'); \"${{rel}}:$($_.LineNumber):$($_.Line)\" }}",
            r = backend::ps_lit(&root),
            q = backend::ps_lit(query),
        );
        let out = ssh::exec_ps(spec, &script, SEARCH_TIMEOUT).await?;
        if !out.ok() {
            return Err(format!("远程搜索失败：{}", out.stderr.trim()));
        }
        return Ok(parse_output(&out.stdout, &root));
    }

    let cmd = format!(
        "if command -v rg >/dev/null 2>&1; then \
           rg --no-heading -n --column --color never -F -e {q} -- {r}; \
         else grep -rn -F -e {q} -- {r}; fi",
        q = backend::shell_quote(query),
        r = backend::shell_quote(&root),
    );
    let out = ssh::exec_timeout(spec, &cmd, SEARCH_TIMEOUT).await?;
    if !out.ok() && out.code != 1 {
        return Err(format!("远程搜索失败：{}", out.stderr.trim()));
    }
    Ok(parse_output(&out.stdout, &root))
}

/// 在项目工作目录内按固定字符串搜索内容。
#[tauri::command]
pub async fn ide_search_content(
    project_id: String,
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<SearchHit>, String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let remote = {
        let inner = state.inner.lock().await;
        inner.projects.remote_of(&project_id)
    };
    if let Some(spec) = remote {
        return remote_search(&spec, q).await;
    }
    let root = {
        let inner = state.inner.lock().await;
        let dir = inner
            .projects
            .workdir_of(&project_id)
            .ok_or_else(|| "项目未设置工作目录".to_string())?;
        std::path::PathBuf::from(dir)
    };
    if !root.is_dir() {
        return Err("工作目录不存在".to_string());
    }
    local_search(&root, q).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rg_output_with_column() {
        let hit = parse_line("/root/src/a.rs:12:5:let x = 1;", "/root").unwrap();
        assert_eq!(hit.rel, "src/a.rs");
        assert_eq!(hit.line, 12);
        assert_eq!(hit.col, 5);
        assert_eq!(hit.text, "let x = 1;");
    }

    #[test]
    fn parses_grep_output_without_column() {
        let hit = parse_line("/root/readme.md:3:hello world", "/root").unwrap();
        assert_eq!(hit.rel, "readme.md");
        assert_eq!(hit.line, 3);
        assert_eq!(hit.col, 0);
        assert_eq!(hit.text, "hello world");
    }

    #[test]
    fn parses_windows_relative_path() {
        let hit = parse_line("sub\\a.txt:7:foo: bar", "C:\\Users\\z").unwrap();
        assert_eq!(hit.rel, "sub/a.txt");
        assert_eq!(hit.line, 7);
        assert_eq!(hit.text, "foo: bar");
    }

    #[tokio::test]
    #[ignore]
    async fn remote_search_localhost() {
        use crate::projects::RemoteSpec;
        let dir = format!("/tmp/ligong-search-test-{}", std::process::id());
        let spec = RemoteSpec {
            host: "localhost".into(),
            user: None,
            port: None,
            root: dir.clone(),
            ssh_alias: Some("localhost".into()),
            proxy_jump: None,
            restrict: true,
        };
        crate::ide::ssh::exec(&spec, &format!("rm -rf {d} && mkdir -p {d}/sub", d = dir))
            .await
            .unwrap();
        crate::ide::backend::write_file(&spec, "a.txt", "hello world\nsecond line\n")
            .await
            .unwrap();
        crate::ide::backend::write_file(&spec, "sub/b.txt", "no match here\nhello again\n")
            .await
            .unwrap();
        let hits = remote_search(&spec, "hello").await.unwrap();
        assert_eq!(hits.len(), 2, "hits: {hits:?}");
        let rels: Vec<String> = hits.iter().map(|h| h.rel.clone()).collect();
        assert!(rels.contains(&"a.txt".to_string()));
        assert!(rels.contains(&"sub/b.txt".to_string()));
        crate::ide::ssh::exec(&spec, &format!("rm -rf {dir}")).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn remote_search_windows() {
        use crate::projects::RemoteSpec;
        let spec = RemoteSpec {
            host: "gongwei".into(),
            user: None,
            port: None,
            root: "C:/Users/z/ligong-search-test".into(),
            ssh_alias: Some("gongwei".into()),
            proxy_jump: None,
            restrict: true,
        };
        assert!(crate::ide::backend::is_windows_remote(&spec).await);
        crate::ide::backend::write_file(&spec, "x.txt", "alpha\nbeta hello\n")
            .await
            .unwrap();
        let hits = remote_search(&spec, "hello").await.unwrap();
        assert!(hits.iter().any(|h| h.rel.ends_with("x.txt")), "hits: {hits:?}");
        crate::ide::backend::delete(&spec, "x.txt").await.unwrap();
    }
}
