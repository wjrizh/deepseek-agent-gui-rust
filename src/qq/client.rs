//! OneBot v11 正向 WS action 发送与消息分段。

use std::sync::Arc;

use futures_util::SinkExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

pub type WsSender = Arc<
    Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
            Message,
        >,
    >,
>;

const CHUNK_LIMIT: usize = 4000;

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub async fn call_ws(sender: &WsSender, action: &str, params: Value) -> Result<(), String> {
    let frame = json!({
        "action": action,
        "params": params,
        "echo": format!("s-{}", now_ms()),
    })
    .to_string();
    let mut guard = sender.lock().await;
    guard
        .send(Message::Text(frame.into()))
        .await
        .map_err(|e| format!("WS 发送 {action} 失败: {e}"))
}

pub async fn send_group(sender: &WsSender, group_id: i64, text: &str) -> Result<(), String> {
    call_ws(sender, "send_group_msg", json!({ "group_id": group_id, "message": text })).await
}

pub async fn send_private(sender: &WsSender, user_id: i64, text: &str) -> Result<(), String> {
    call_ws(sender, "send_private_msg", json!({ "user_id": user_id, "message": text })).await
}

pub fn split_message(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= CHUNK_LIMIT {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + CHUNK_LIMIT).min(chars.len());
        if end == chars.len() {
            out.push(chars[start..end].iter().collect());
            break;
        }
        let slice = &chars[start..end];
        let mut cut = None;
        for (i, c) in slice.iter().enumerate().rev() {
            if *c == '\n' || *c == '。' || *c == '！' || *c == '？' {
                cut = Some(i + 1);
                break;
            }
        }
        let real_end = match cut {
            Some(i) if i > CHUNK_LIMIT / 2 => start + i,
            _ => end,
        };
        out.push(chars[start..real_end].iter().collect());
        start = real_end;
    }
    out
}