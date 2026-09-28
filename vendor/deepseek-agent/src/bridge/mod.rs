//! 浏览器 bridge 模块（Node.js + Playwright 方案）。
//!
//! Rust 侧不碰 CDP/浏览器协议，只 spawn 一个 node 子进程，
//! 用 stdio 行协议（JSON）收发 prompt → 纯文本响应。

pub mod model;

pub use model::BridgeModel;

pub fn login_script_path() -> std::path::PathBuf {
    crate::paths::login_script_path()
}

pub fn server_script_path() -> std::path::PathBuf {
    crate::paths::bridge_server_script()
}