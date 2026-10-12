//! IDE 模块：为每个项目提供独立窗口的文件树 + Monaco 编辑器。
//!
//! 拆分：
//! - `path`：路径解析与 restrict 安全校验。
//! - `fs`：列目录 / 读文件 / 写文件命令。
//! - `window`：打开 / 复用 IDE 独立窗口。

pub mod fs;
pub mod git;
pub mod path;
pub mod preview;
pub mod term;
pub mod watch;
pub mod window;