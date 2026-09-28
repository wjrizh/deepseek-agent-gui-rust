//! QQ ~/.ligong/qq_map.json
use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QqConfig {
    #[serde(default = "default_ws_url")]
    pub ws_url: String,
    #[serde(default = "default_http_url")]
    pub http_url: String,
    #[serde(default)]
    pub mapping: HashMap<String, String>,
    /// 新建项目时使用的默认账号；已绑定项目以项目自身 account 为准。
    #[serde(default = "default_account")]
    pub account: String,
}

fn default_ws_url() -> String { "ws://127.0.0.1:3001".to_string() }
fn default_http_url() -> String { "http://127.0.0.1:3000".to_string() }
fn default_account() -> String { "Y".to_string() }

impl Default for QqConfig {
    fn default() -> Self {
        Self {
            ws_url: default_ws_url(),
            http_url: default_http_url(),
            mapping: HashMap::new(),
            account: default_account(),
        }
    }
}

fn map_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".ligong")
        .join("qq_map.json")
}

impl QqConfig {
    pub fn load() -> Self {
        let p = map_path();
        std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> Result<(), String> {
        let p = map_path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&p, s).map_err(|e| e.to_string())
    }
    pub fn project_of(&self, session_key: &str) -> Option<&String> {
        self.mapping.get(session_key)
    }
    pub fn bind(&mut self, session_key: &str, project_id: &str) {
        self.mapping
            .insert(session_key.to_string(), project_id.to_string());
    }
    pub fn account_name(&self) -> &str {
        &self.account
    }
    pub fn set_account(&mut self, name: &str) {
        self.account = name.to_string();
    }
}