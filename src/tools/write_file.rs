//! `write_file` 工具：创建或整文件覆盖写入，模仿 Claude Code 的 Write。
//!
//! 参数：
//! - `file_path`：绝对路径（相对路径按 cwd 解析）。
//! - `content`：完整文件内容（不追加、不合并）。
//!
//! 行为：
//! - 自动创建父目录。
//! - 覆盖前若文件已存在，返回说明（覆盖了多少字节 → 多少字节）。
//! - 内容经工具参数传入，不经过 shell，天然免疫 `$`/反引号/`<tag>` 干扰。

use async_trait::async_trait;
use deepseek_agent::agent::tool::Tool;
use deepseek_agent::error::{AgentError, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct WriteFile {
    cwd: PathBuf,
    restrict: bool,
}

impl WriteFile {
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
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "创建或整文件覆盖写入文件。参数：file_path（必填，绝对路径）、content（必填，完整内容）。不追加、不合并；覆盖已存在文件需先 read_file 读过。"
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
            .ok_or_else(|| AgentError::Other("write_file: 缺少非空 file_path".into()))?;
        let content = params
            .get("content")
            .ok_or_else(|| AgentError::Other("write_file: 缺少 content".into()))?;
        let path = self.resolve(raw)?;

        if path.is_dir() {
            return Err(AgentError::Other(format!(
                "write_file: {} 是目录，不能写入",
                path.display()
            )));
        }

        let existed = path.exists();
        let old_len = if existed {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AgentError::Other(format!("write_file: 创建目录 {} 失败: {e}", parent.display()))
                })?;
            }
        }

        atomic_write(&path, content.as_bytes())?;

        let new_len = content.len();
        if existed {
            Ok(format!(
                "已覆盖写入 {}（{} → {} 字节）",
                path.display(),
                old_len,
                new_len
            ))
        } else {
            Ok(format!("已创建 {}（{} 字节）", path.display(), new_len))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn call(tool: &WriteFile, params: &[(&str, &str)]) -> Result<String> {
        let map: HashMap<String, String> =
            params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut cb = |_: &str| {};
        tool.call(&map, false, &mut cb).await
    }

    #[tokio::test]
    async fn creates_and_overwrites() {
        let d = std::env::temp_dir().join(format!("ligong_write_test_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a/b.txt");
        let t = WriteFile::new(d.clone());
        let r = call(&t, &[("file_path", f.to_str().unwrap()), ("content", "line1\n$VAR `x` <tag>\n")]).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "line1\n$VAR `x` <tag>\n");
        let r2 = call(&t, &[("file_path", f.to_str().unwrap()), ("content", "new")]).await;
        assert!(r2.is_ok(), "{r2:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
    }

    #[tokio::test]
    async fn atomic_write_leaves_no_temp_and_overwrites() {
        let d = std::env::temp_dir().join(format!("ligong_atomic_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("x.txt");
        std::fs::write(&f, "old-content").unwrap();
        atomic_write(&f, b"new-content").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new-content");
        // 同目录不应残留 .tmp_ 临时文件
        let leftover: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp_"))
            .collect();
        assert!(leftover.is_empty(), "残留临时文件: {leftover:?}");
    }
}

/// 原子写入：先写同目录临时文件，再 rename 覆盖，避免中途失败截断原文件。
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = parent.unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());
    let tmp = dir.join(format!(".{file_name}.tmp_{pid}_{nonce}"));
    std::fs::write(&tmp, bytes).map_err(|e| {
        AgentError::Other(format!("write_file: 写临时文件 {} 失败: {e}", tmp.display()))
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AgentError::Other(format!("write_file: 替换 {} 失败: {e}", path.display()))
    })?;
    Ok(())
}
