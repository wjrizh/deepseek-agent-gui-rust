//! `edit_file` 工具：精确字符串替换，模仿 Claude Code 的 Edit。
//!
//! 参数：
//! - `file_path`：绝对路径（相对路径按 cwd 解析）。
//! - `old_string`：要被替换的原文（必须精确匹配）。
//! - `new_string`：替换后的文本。
//! - `replace_all`（可选，默认 false）：为真时替换全部出现，否则要求唯一。
//!
//! 行为（对齐 Claude Code）：
//! - 不使用正则、不做模糊匹配。
//! - `old_string` 必须在文件中精确出现。
//! - 默认要求唯一；出现多次且未设 replace_all 时报错，提示补充上下文或 replace_all。
//! - 文件被修改前需先 read_file（read-before-edit）。

use async_trait::async_trait;
use deepseek_agent::agent::tool::Tool;
use deepseek_agent::error::{AgentError, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct EditFile {
    cwd: PathBuf,
    restrict: bool,
}

impl EditFile {
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
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "对文件做精确字符串替换（非正则）。参数：file_path（必填）、old_string（必填，须精确匹配且在文件中唯一，除非 replace_all=true）、new_string（必填）、replace_all（可选，默认 false）。"
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
            .ok_or_else(|| AgentError::Other("edit_file: 缺少非空 file_path".into()))?;
        let old_string = params
            .get("old_string")
            .ok_or_else(|| AgentError::Other("edit_file: 缺少 old_string".into()))?;
        let new_string = params
            .get("new_string")
            .ok_or_else(|| AgentError::Other("edit_file: 缺少 new_string".into()))?;
        let replace_all = params
            .get("replace_all")
            .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
            .unwrap_or(false);

        if old_string.is_empty() {
            return Err(AgentError::Other(
                "edit_file: old_string 为空（如需整文件覆盖请用 write_file）".into(),
            ));
        }
        if old_string == new_string {
            return Err(AgentError::Other(
                "edit_file: old_string 与 new_string 相同，无需修改".into(),
            ));
        }

        let path = self.resolve(raw)?;
        if !path.is_file() {
            return Err(AgentError::Other(format!(
                "edit_file: {} 不是已存在的文件",
                path.display()
            )));
        }

        let bytes = std::fs::read(&path)
            .map_err(|e| AgentError::Other(format!("edit_file: 读取 {} 失败: {e}", path.display())))?;
        let text = String::from_utf8(bytes)
            .map_err(|e| AgentError::Other(format!("edit_file: {} 非 UTF-8 文本: {e}", path.display())))?;

        // CRLF 自适应：Windows / core.autocrlf 仓库里磁盘是 \r\n，
        // 而模型输出的 old_string/new_string 几乎总是纯 \n。
        // 匹配前统一归一化为 \n，写回时按原文件风格还原。
        let is_crlf = text.contains("\r\n");
        let norm_text = if is_crlf {
            text.replace("\r\n", "\n")
        } else {
            text.clone()
        };
        let norm_old = old_string.replace("\r\n", "\n");
        let norm_new = new_string.replace("\r\n", "\n");

        let count = norm_text.matches(&norm_old).count();
        if count == 0 {
            return Err(AgentError::Other(
                "edit_file: old_string 未在文件中出现（必须精确匹配，含空白与缩进）".into(),
            ));
        }
        if count > 1 && !replace_all {
            return Err(AgentError::Other(format!(
                "edit_file: old_string 出现 {count} 次，不唯一。请补充更多上下文使其唯一，或设 replace_all=true"
            )));
        }

        let updated_norm = if replace_all {
            norm_text.replace(&norm_old, &norm_new)
        } else {
            norm_text.replacen(&norm_old, &norm_new, 1)
        };

        let final_text = if is_crlf {
            updated_norm.replace('\n', "\r\n")
        } else {
            updated_norm
        };

        crate::tools::write_file::atomic_write(&path, final_text.as_bytes())?;

        if replace_all && count > 1 {
            Ok(format!("已替换 {} 处（{}）", count, path.display()))
        } else {
            Ok(format!("已替换 1 处（{}）", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ligong_edit_test_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    async fn call(tool: &EditFile, params: &[(&str, &str)]) -> Result<String> {
        let map: HashMap<String, String> =
            params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut cb = |_: &str| {};
        tool.call(&map, false, &mut cb).await
    }

    #[tokio::test]
    async fn replaces_unique() {
        let f = tmp("unique.txt");
        std::fs::write(&f, "hello world\n").unwrap();
        let t = EditFile::new(std::env::temp_dir());
        let r = call(&t, &[("file_path", f.to_str().unwrap()), ("old_string", "world"), ("new_string", "rust")]).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello rust\n");
    }

    #[tokio::test]
    async fn rejects_non_unique() {
        let f = tmp("dup.txt");
        std::fs::write(&f, "aa\naa\n").unwrap();
        let t = EditFile::new(std::env::temp_dir());
        let r = call(&t, &[("file_path", f.to_str().unwrap()), ("old_string", "aa"), ("new_string", "bb")]).await;
        assert!(r.is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "aa\naa\n");
    }

    #[tokio::test]
    async fn replace_all_ok() {
        let f = tmp("all.txt");
        std::fs::write(&f, "aa\naa\n").unwrap();
        let t = EditFile::new(std::env::temp_dir());
        let r = call(&t, &[("file_path", f.to_str().unwrap()), ("old_string", "aa"), ("new_string", "bb"), ("replace_all", "true")]).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "bb\nbb\n");
    }

    #[tokio::test]
    async fn rejects_missing_old() {
        let f = tmp("miss.txt");
        std::fs::write(&f, "hello\n").unwrap();
        let t = EditFile::new(std::env::temp_dir());
        let r = call(&t, &[("file_path", f.to_str().unwrap()), ("old_string", "nope"), ("new_string", "x")]).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn crlf_adapts_and_preserves_style() {
        let f = tmp("crlf.txt");
        std::fs::write(&f, "a\r\nb\r\nc\r\n").unwrap();
        let t = EditFile::new(std::env::temp_dir());
        // 模型给的是纯 \n 的 old_string，磁盘是 \r\n：应能匹配并保持 CRLF 写回。
        let r = call(&t, &[("file_path", f.to_str().unwrap()), ("old_string", "b\nc"), ("new_string", "B\nC")]).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(std::fs::read(&f).unwrap(), b"a\r\nB\r\nC\r\n");
    }
}
