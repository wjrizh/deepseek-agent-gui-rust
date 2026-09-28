//! QQ 消息路由：解析命令、绑定项目、驱动 agent。

use serde_json::Value;

/// 一条入站消息的解析结果。
#[derive(Debug, Clone)]
pub struct Incoming {
    /// 群聊为 "group_<群号>"，私聊为 "private_<QQ号>"。
    pub session_key: String,
    pub group_id: Option<i64>,
    pub user_id: i64,
    /// 去掉 @机器人 前缀与首尾空白后的正文。
    pub text: String,
}

/// 从 OneBot 事件 JSON 解析出站消息；非消息事件返回 None。
pub fn parse_event(ev: &Value, self_id: i64) -> Option<Incoming> {
    let post_type = ev.get("post_type").and_then(|v| v.as_str()).unwrap_or("");
    if post_type != "message" {
        return None;
    }
    let message_type = ev.get("message_type").and_then(|v| v.as_str()).unwrap_or("");
    let user_id = ev.get("user_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let raw_text = extract_text(ev);

    match message_type {
        "group" => {
            let group_id = ev.get("group_id").and_then(|v| v.as_i64()).unwrap_or(0);
            let (mentioned, text) = strip_at(&raw_text, self_id);
            if !mentioned {
                return None;
            }
            let text = text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(Incoming {
                session_key: format!("group_{group_id}"),
                group_id: Some(group_id),
                user_id,
                text,
            })
        }
        "private" => {
            let text = raw_text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(Incoming {
                session_key: format!("private_{user_id}"),
                group_id: None,
                user_id,
                text,
            })
        }
        _ => None,
    }
}

/// 从 message 数组提取纯文本（含 CQ:at 转成占位）。
fn extract_text(ev: &Value) -> String {
    let mut out = String::new();
    if let Some(arr) = ev.get("message").and_then(|v| v.as_array()) {
        for seg in arr {
            let t = seg.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match t {
                "text" => {
                    if let Some(s) = seg.get("data").and_then(|d| d.get("text")).and_then(|v| v.as_str()) {
                        out.push_str(s);
                    }
                }
                "at" => {
                    let qq = seg.get("data").and_then(|d| d.get("qq")).and_then(|v| v.as_str()).unwrap_or("");
                    out.push_str(&format!("[CQ:at,qq={qq}]"));
                }
                _ => {}
            }
        }
    }
    out
}

/// 判断是否 @了机器人；返回 (是否@, 去掉@后的文本)。
fn strip_at(text: &str, self_id: i64) -> (bool, String) {
    let marker = format!("[CQ:at,qq={self_id}]");
    if text.contains(&marker) {
        (true, text.replace(&marker, ""))
    } else {
        (false, text.to_string())
    }
}
