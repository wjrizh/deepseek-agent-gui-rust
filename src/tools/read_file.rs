//! `read_file` 工具：读取文件内容（带行号），模仿 Claude Code 的 Read。
//!
//! 参数：
//! - `file_path`：绝对路径（也容忍相对路径，按 cwd 解析）。
//! - `offset`（可选）：起始行号（1 基）。
//! - `limit`（可选）：读取的最大行数。
//!
//! 行为：
//! - 返回 `行号<TAB>内容` 形式，便于模型精确定位。
//! - 超过单次读取上限时返回 PARTIAL 提示。
//! - 空文件给出明确提示。

use async_trait::async_trait;
use deepseek_agent::agent::tool::{Tool, MAX_TOOL_OUTPUT};
use deepseek_agent::error::{AgentError, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// 单次读取的最大行数（未显式给 limit 时）。
const DEFAULT_LIMIT: usize = 2000;

pub struct ReadFile {
    cwd: PathBuf,
    restrict: bool,
}

impl ReadFile {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd, restrict: false }
    }

    /// 开启后，拒绝解析到项目工作目录之外的路径（含 `..` 逃逸）。
    pub fn with_restrict(mut self, restrict: bool) -> Self {
        self.restrict = restrict;
        self
    }

    fn resolve(&self, raw: &str) -> Result<PathBuf> {
        let p = Path::new(raw);
        let target = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        };
        if self.restrict {
            let base = self.cwd.canonicalize().map_err(|e| {
                AgentError::Other(format!("路径校验失败（工作目录不可解析）: {e}"))
            })?;
            // 目标可能尚不存在（write_file 新建），需回退到其最近的已存在祖先。
            let check_path = if target.exists() {
                target.canonicalize().map_err(|e| {
                    AgentError::Other(format!("路径校验失败: {e}"))
                })?
            } else {
                let mut anc = target.parent();
                let mut resolved = None;
                while let Some(a) = anc {
                    if a.exists() {
                        resolved = Some(a.canonicalize().map_err(|e| {
                            AgentError::Other(format!("路径校验失败: {e}"))
                        })?);
                        break;
                    }
                    anc = a.parent();
                }
                resolved.unwrap_or_else(|| target.clone())
            };
            if !check_path.starts_with(&base) {
                return Err(AgentError::Other(format!(
                    "权限拒绝：路径 {} 超出项目工作目录限制",
                    target.display()
                )));
            }
        }
        Ok(target)
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "读取文件内容并返回带行号的文本。参数：file_path（必填）、offset（可选，1 基起始行）、limit（可选，最大行数）。"
    }

    async fn call(
        &self,
        params: &HashMap<String, String>,
        _live: bool,
        _on_output: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<String> {
        let raw = params
            .get("file_path")
            .or_else(|| params.get("path"))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AgentError::Other("read_file: 缺少非空 file_path".into()))?;
        let path = self.resolve(raw)?;

        if path.is_dir() {
            return Err(AgentError::Other(format!(
                "read_file: {} 是目录，不是文件",
                path.display()
            )));
        }

        // 文件大小仅用于提示，不载入内存。
        let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        let file = std::fs::File::open(&path)
            .map_err(|e| AgentError::Other(format!("read_file: 打开 {} 失败: {e}", path.display())))?;
        let mut reader = BufReader::new(file);

        // 二进制嗅探：查前 512 字节是否含空字节。fill_buf 只预读缓冲，不加载整文件。
        {
            let head = reader.fill_buf().map_err(|e| {
                AgentError::Other(format!("read_file: 读取 {} 失败: {e}", path.display()))
            })?;
            if head.iter().take(512).any(|&b| b == 0) {
                return Ok(format!(
                    "[文件 {} 为二进制格式（{} 字节），无法以文本读取，请使用 upload_file 工具]",
                    path.display(),
                    file_size
                ));
            }
        }

        let offset: usize = params
            .get("offset")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1)
            .max(1);
        let limit: usize = params
            .get("limit")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(DEFAULT_LIMIT)
            .max(1);
        let end_wanted = offset + limit - 1;

        // 流式逐行读取：跳过 offset 之前的行，收集到 limit 为止，内存 O(limit)。
        let mut out = String::new();
        let mut line_no: usize = 0;
        let mut shown = 0usize;
        let mut more_after = false;
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            let n = reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| AgentError::Other(format!("read_file: 读取 {} 失败: {e}", path.display())))?;
            if n == 0 {
                break;
            }
            line_no += 1;
            if line_no < offset {
                continue;
            }
            if line_no > end_wanted {
                more_after = true;
                break;
            }
            // 去掉行尾 \n 与 CRLF 的 \r。
            if buf.last() == Some(&b'\n') {
                buf.pop();
            }
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            let line = String::from_utf8_lossy(&buf);
            out.push_str(&format!("{}\t{}\n", line_no, line));
            shown += 1;
            if out.len() > MAX_TOOL_OUTPUT {
                out.push_str("\n...[输出已截断]");
                return Ok(out);
            }
        }

        if shown == 0 {
            if line_no == 0 {
                return Ok(format!("[文件 {} 存在，但内容为空]", path.display()));
            }
            return Ok(format!(
                "[offset {offset} 超出范围：文件共 {line_no} 行]"
            ));
        }

        if more_after {
            out.push_str(&format!(
                "\n[PARTIAL view：已显示第 {}-{} 行；用 offset/limit 读取其余部分]",
                offset,
                offset + shown - 1
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn call(tool: &ReadFile, params: &[(&str, &str)]) -> Result<String> {
        let map: HashMap<String, String> =
            params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut cb = |_: &str| {};
        tool.call(&map, false, &mut cb).await
    }

    #[tokio::test]
    async fn reads_with_line_numbers_and_limit() {
        let d = std::env::temp_dir().join(format!("ligong_read_test_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("r.txt");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        let t = ReadFile::new(d.clone());
        let out = call(&t, &[("file_path", f.to_str().unwrap()), ("offset", "2"), ("limit", "1")]).await.unwrap();
        assert!(out.contains("2\tb"), "{out}");
        assert!(out.contains("PARTIAL"), "{out}");
    }

    #[tokio::test]
    async fn streams_middle_slice_of_large_file() {
        let d = std::env::temp_dir().join(format!("ligong_read_big_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("big.txt");
        // 生成 1 万行，验证只取中段且不整文件载入。
        let mut content = String::new();
        for i in 1..=10000 {
            content.push_str(&format!("line{i}\n"));
        }
        std::fs::write(&f, content).unwrap();
        let t = ReadFile::new(d.clone());
        let out = call(&t, &[("file_path", f.to_str().unwrap()), ("offset", "5000"), ("limit", "3")]).await.unwrap();
        assert!(out.contains("5000\tline5000"), "{out}");
        assert!(out.contains("5002\tline5002"), "{out}");
        assert!(out.contains("PARTIAL"), "{out}");
        assert!(!out.contains("line4999"), "{out}");
        assert!(!out.contains("line5003"), "{out}");
    }

    #[tokio::test]
    async fn binary_sniff_short_circuits() {
        let d = std::env::temp_dir().join(format!("ligong_read_bin_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("b.bin");
        std::fs::write(&f, b"\x00\x01\x02abc").unwrap();
        let t = ReadFile::new(d.clone());
        let out = call(&t, &[("file_path", f.to_str().unwrap())]).await.unwrap();
        assert!(out.contains("二进制"), "{out}");
        assert!(out.contains("upload_file"), "{out}");
    }

    #[tokio::test]
    async fn restrict_denies_escape_but_allows_inside() {
        let base = std::env::temp_dir().join(format!("ligong_read_restrict_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let inside = base.join("ok.txt");
        std::fs::write(&inside, "hi\n").unwrap();
        let t = ReadFile::new(base.clone()).with_restrict(true);

        // 界内：应通过
        let ok = call(&t, &[("file_path", inside.to_str().unwrap())]).await;
        assert!(ok.is_ok(), "{ok:?}");

        // 越界：相对逃逸
        let esc = call(&t, &[("file_path", "../outside.txt")]).await;
        assert!(esc.is_err(), "{esc:?}");
        assert!(esc.unwrap_err().to_string().contains("权限拒绝"));

        // 越界：绝对路径指向界外
        let abs = call(&t, &[("file_path", "/etc/hostname")]).await;
        assert!(abs.is_err(), "{abs:?}");
    }
}
