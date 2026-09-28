//! QQ 接入（NapCat / OneBot v11 正向 WebSocket）。
//!
//! Tauri 作为 WS 客户端连接 NapCat 的 ws://127.0.0.1:3001，
//! 接收群/私聊消息，路由到对应项目后调用 agent，任务完成后把最终回复发回 QQ。

pub mod client;
pub mod config;
pub mod napcat;
pub mod router;
pub mod ws;

pub use config::QqConfig;
pub use ws::QqHandle;