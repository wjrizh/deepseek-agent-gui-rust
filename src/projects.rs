//! 项目/对话持久化：~/.deepseek-agent/projects.json

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 默认的上下文压缩阈值（消息条数）。每项目可在项目设置里单独调整。
pub const COMPACT_THRESHOLD_DEFAULT: i64 = 300;

/// 每项目子 agent 并发上限的缺省值。
pub const SUB_LIMIT_DEFAULT: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub last_parent_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentConv {
    pub session_id: String,
    #[serde(default)]
    pub parent_message_id: Option<i64>,
    pub created_at: u64,
    #[serde(default)]
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub duty: String,
    #[serde(default)]
    pub account: String,
    pub created_at: u64,
    #[serde(default)]
    pub conversations: Vec<SubAgentConv>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub conversations: Vec<Conversation>,
    #[serde(default)]
    pub sub_agents: Vec<SubAgent>,
    #[serde(default)]
    pub sub_accounts: Vec<String>,
    #[serde(default)]
    pub sub_account_limits: std::collections::HashMap<String, u32>,
    /// 账号池计费口径：permanent（默认，历史绑定长期占用名额）/ active（仅运行中的子 agent 占用）。
    #[serde(default = "default_sub_account_mode")]
    pub sub_account_mode: String,
}

fn default_sub_account_mode() -> String {
    "permanent".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectStore {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub active_project: Option<String>,
    #[serde(default)]
    pub active_conversation: Option<String>,
    #[serde(default)]
    pub active_by_project: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub free_by_project: std::collections::HashMap<String, bool>,
    #[serde(default)]
    pub workdir_by_project: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub restrict_by_project: std::collections::HashMap<String, bool>,
    /// 每项目的上下文压缩阈值（消息条数）。缺省为 COMPACT_THRESHOLD_DEFAULT。
    #[serde(default)]
    pub compact_by_project: std::collections::HashMap<String, i64>,
    /// 每项目子 agent 并发上限。缺省为 SUB_LIMIT_DEFAULT。
    #[serde(default)]
    pub max_sub_by_project: std::collections::HashMap<String, u32>,
    /// 每项目是否预启动主 bridge（默认关，减少启动延迟）。key=project_id。
    #[serde(default)]
    pub warmup_by_project: std::collections::HashMap<String, bool>,
    #[serde(default)]
    pub conversation_seq: u64,
    /// 存储格式版本号，用于未来迁移与兼容判断。
    #[serde(default)]
    pub version: u64,
}

/// 当前存储格式版本。旧文件缺省为 0，加载后升级为当前值。
pub const STORE_VERSION: u64 = 1;

fn store_path() -> PathBuf {
    let dir = dirs::home_dir().unwrap_or_default();
    dir.join(".deepseek-agent").join("gui-projects.json")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 极简 id：时间戳 + 计数器，避免引入 uuid 依赖。
fn gen_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{t:x}-{n:x}")
}

/// 对话内容文件目录：~/.deepseek-agent/conversations/
fn conversations_dir() -> Result<PathBuf, String> {
    let dir = dirs::home_dir().ok_or_else(|| "无法解析 HOME 目录".to_string())?;
    Ok(dir.join(".deepseek-agent").join("conversations"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    #[serde(default)]
    pub text: String,
    pub ts: u64,
    /// 同秒内稳定序号（追加顺序），用于排序去重。旧数据缺省为 0。
    #[serde(default)]
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRecord {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub ms: u64,
    #[serde(default)]
    pub error: bool,
}

/// 单条对话文件路径（conv_id 做了字符白名单，防目录穿越）。
pub fn conversation_file(conv_id: &str) -> Result<PathBuf, String> {
    if conv_id.is_empty()
        || !conv_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("非法对话 id".into());
    }
    Ok(conversations_dir()?.join(format!("{conv_id}.jsonl")))
}
pub fn rename_conversation_file(old_id: &str, new_id: &str) -> Result<(), String> {
    if old_id.is_empty() || new_id.is_empty() || old_id == new_id {
        return Ok(());
    }
    let old_path = conversation_file(old_id)?;
    let new_path = conversation_file(new_id)?;
    if old_path.exists() {
        std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())?;
        clear_msg_seq_cache(&old_path);
        clear_msg_seq_cache(&new_path);
    }
    Ok(())
}
/// 每对话文件的已写行数缓存：避免每次追加都读取整个 JSONL 文件（O(n²)）。
static MSG_SEQ_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
    std::sync::OnceLock::new();

fn msg_seq_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, u64>> {
    MSG_SEQ_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 追加时的下一个稳定序号（文件当前行数）。
/// 使用内存缓存：首次读取文件计数，之后自增；文件被删除/重命名时清理缓存。
fn next_message_seq(path: &std::path::Path) -> u64 {
    let key = path.to_string_lossy().to_string();
    let mut cache = match msg_seq_cache().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(n) = cache.get(&key) {
        let next = *n;
        cache.insert(key, next + 1);
        return next;
    }
    // 首次：读文件计数初始化。
    let count = match std::fs::read_to_string(path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).count() as u64,
        Err(_) => 0,
    };
    cache.insert(key, count + 1);
    count
}

/// 清理某对话文件的 seq 缓存（删除/重命名时调用）。
fn clear_msg_seq_cache(path: &std::path::Path) {
    if let Ok(mut cache) = msg_seq_cache().lock() {
        cache.remove(&path.to_string_lossy().to_string());
    }
}

pub fn append_message(conv_id: &str, role: &str, text: &str) -> Result<(), String> {
    let path = conversation_file(conv_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let msg = StoredMessage {
        role: role.to_string(),
        text: text.to_string(),
        ts: now_secs(),
        seq: next_message_seq(&path),
        tool: None,
    };
    let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    line.push('\n');
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn append_tool(
    conv_id: &str,
    name: &str,
    command: &str,
    output: &str,
    ms: u64,
    error: bool,
) -> Result<(), String> {
    let path = conversation_file(conv_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let msg = StoredMessage {
        role: "tool".to_string(),
        text: String::new(),
        ts: now_secs(),
        seq: next_message_seq(&path),
        tool: Some(ToolRecord {
            name: name.to_string(),
            command: command.to_string(),
            output: output.to_string(),
            ms,
            error,
        }),
    };
    let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    line.push('\n');
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

/// 读取对话文件全部消息（文件不存在返回空）。
pub fn load_messages(conv_id: &str) -> Result<Vec<StoredMessage>, String> {
    let path = conversation_file(conv_id)?;
    let s = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(m) = serde_json::from_str::<StoredMessage>(line) {
            out.push(m);
        }
    }
    // 按 (ts, seq) 稳定排序；同秒消息用 seq 保序，旧数据 seq=0 维持原序。
    out.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.seq.cmp(&b.seq)));
    Ok(out)
}

/// 删除对话文件（不存在也视为成功）。
pub fn delete_conversation_file(conv_id: &str) -> Result<(), String> {
    let path = conversation_file(conv_id)?;
    clear_msg_seq_cache(&path);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn bump_conversation_seq(store: &mut ProjectStore) {
    let mut max_seq = store.conversation_seq;
    for p in &store.projects {
        for c in &p.conversations {
            if let Some(rest) = c.title.strip_prefix("对话") {
                if let Ok(n) = rest.trim().parse::<u64>() {
                    if n > max_seq {
                        max_seq = n;
                    }
                }
            }
        }
    }
    store.conversation_seq = max_seq;
}

impl ProjectStore {
    pub fn load() -> Self {
        let path = store_path();
        let mut store: Self = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        bump_conversation_seq(&mut store);
        if store.version == 0 {
            store.version = STORE_VERSION;
        }
        store
    }

    pub fn free_mode_of(&self, project_id: &str) -> bool {
        self.free_by_project.get(project_id).copied().unwrap_or(false)
    }

    pub fn set_free_mode_of(&mut self, project_id: &str, free: bool) {
        self.free_by_project.insert(project_id.to_string(), free);
    }

    pub fn save(&self) -> Result<(), String> {
        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // 写前备份上一份（保留最近一次良好状态，损坏时可回退）。
        // 节流：距上次备份 <5s 则跳过，避免高频 save 反复复制整个 JSON。
        if path.is_file() {
            let now = std::time::Instant::now();
            let should_backup = {
                static LAST_BACKUP: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
                    std::sync::OnceLock::new();
                let m = LAST_BACKUP.get_or_init(|| std::sync::Mutex::new(None));
                let mut g = match m.lock() { Ok(g) => g, Err(p) => p.into_inner() };
                let ok = g.map(|t| now.duration_since(t) >= std::time::Duration::from_secs(5)).unwrap_or(true);
                if ok { *g = Some(now); }
                ok
            };
            if should_backup {
                let bak = path.with_extension("json.bak");
                let _ = std::fs::copy(&path, &bak);
            }
        }
        let mut snapshot = self.clone();
        snapshot.version = STORE_VERSION;
        let json = serde_json::to_string_pretty(&snapshot).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 生成项目内下一个对话标题：对话N，N = 该项目现有最大编号 + 1。
    pub fn next_conversation_title(&mut self, project_id: &str) -> String {
        let max = self
            .project(project_id)
            .map(|p| {
                p.conversations
                    .iter()
                    .filter_map(|c| {
                        c.title.strip_prefix("对话")
                            .and_then(|r| r.trim().parse::<u64>().ok())
                    })
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        format!("对话{}", max + 1)
    }

    pub fn workdir_of(&self, project_id: &str) -> Option<String> {
        self.workdir_by_project.get(project_id).cloned()
    }

    pub fn set_workdir_of(&mut self, project_id: &str, dir: &str) {
        self.workdir_by_project
            .insert(project_id.to_string(), dir.to_string());
    }

    pub fn restrict_of(&self, project_id: &str) -> bool {
        self.restrict_by_project.get(project_id).copied().unwrap_or(false)
    }

    pub fn set_restrict_of(&mut self, project_id: &str, on: bool) {
        self.restrict_by_project.insert(project_id.to_string(), on);
    }

    pub fn compact_of(&self, project_id: &str) -> i64 {
        self.compact_by_project
            .get(project_id)
            .copied()
            .filter(|v| *v > 0)
            .unwrap_or(COMPACT_THRESHOLD_DEFAULT)
    }

    pub fn set_compact_of(&mut self, project_id: &str, n: i64) {
        self.compact_by_project
            .insert(project_id.to_string(), n);
    }

    pub fn max_sub_of(&self, project_id: &str) -> u32 {
        self.max_sub_by_project
            .get(project_id)
            .copied()
            .filter(|v| *v > 0)
            .unwrap_or(SUB_LIMIT_DEFAULT)
    }

    pub fn set_max_sub_of(&mut self, project_id: &str, n: u32) {
        self.max_sub_by_project
            .insert(project_id.to_string(), n);
    }

    pub fn warmup_of(&self, project_id: &str) -> bool {
        self.warmup_by_project.get(project_id).copied().unwrap_or(false)
    }

    pub fn set_warmup_of(&mut self, project_id: &str, on: bool) {
        self.warmup_by_project.insert(project_id.to_string(), on);
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }
    pub fn project_mut(&mut self, id: &str) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.id == id)
    }

    pub fn active_conversation_of(&self, project_id: &str) -> Option<String> {
        if let Some(cid) = self.active_by_project.get(project_id) {
            return Some(cid.clone());
        }
        if self.active_project.as_deref() == Some(project_id) {
            if let Some(cid) = &self.active_conversation {
                let ok = self
                    .project(project_id)
                    .map(|p| p.conversations.iter().any(|c| &c.id == cid))
                    .unwrap_or(false);
                if ok {
                    return Some(cid.clone());
                }
            }
        }
        None
    }

    pub fn set_active_conversation_of(&mut self, project_id: &str, conv_id: Option<&str>) {
        match conv_id {
            Some(cid) => {
                self.active_by_project
                    .insert(project_id.to_string(), cid.to_string());
            }
            None => {
                self.active_by_project.remove(project_id);
            }
        }
        self.active_project = Some(project_id.to_string());
        self.active_conversation = conv_id.map(|s| s.to_string());
    }

    pub fn create_project(&mut self, name: &str, account: &str) -> Project {
        let p = Project {
            id: gen_id("proj"),
            name: name.trim().to_string(),
            created_at: now_secs(),
            account: account.to_string(),
            conversations: Vec::new(),
            sub_agents: Vec::new(),
            sub_accounts: Vec::new(),
            sub_account_limits: std::collections::HashMap::new(),
            sub_account_mode: "permanent".to_string(),
        };
        self.projects.insert(0, p.clone());
        self.active_project = Some(p.id.clone());
        self.active_conversation = None;
        self.active_by_project.remove(&p.id);
        p
    }

    pub fn rename_project(&mut self, id: &str, name: &str) -> Result<(), String> {
        let p = self.project_mut(id).ok_or("项目不存在")?;
        p.name = name.trim().to_string();
        Ok(())
    }

    pub fn delete_project(&mut self, id: &str) -> Result<(), String> {
        if let Some(p) = self.project(id) {
            for c in &p.conversations {
                let _ = delete_conversation_file(&c.id);
            }
        }
        self.projects.retain(|p| p.id != id);
        self.active_by_project.remove(id);
        if self.active_project.as_deref() == Some(id) {
            self.active_project = self.projects.first().map(|p| p.id.clone());
            self.active_conversation = None;
        }
        Ok(())
    }

    /// 追加对话到项目，并设为当前激活。
    pub fn add_conversation(
        &mut self,
        project_id: &str,
        session_id: &str,
        title: &str,
    ) -> Result<Conversation, String> {
        let conv = Conversation {
            id: session_id.to_string(),
            title: title.trim().to_string(),
            created_at: now_secs(),
            message_count: 0,
            last_parent_id: None,
        };
        let p = self.project_mut(project_id).ok_or("项目不存在")?;
        p.conversations.push(conv.clone());
        self.active_project = Some(project_id.to_string());
        self.active_conversation = Some(session_id.to_string());
        self.active_by_project
            .insert(project_id.to_string(), session_id.to_string());
        Ok(conv)
    }

    pub fn delete_conversation(&mut self, project_id: &str, conv_id: &str) -> Result<(), String> {
        let was_active = self
            .active_by_project
            .get(project_id)
            .map(|id| id == conv_id)
            .unwrap_or(false)
            || (self.active_project.as_deref() == Some(project_id)
                && self.active_conversation.as_deref() == Some(conv_id));

        let p = self.project_mut(project_id).ok_or("项目不存在")?;
        p.conversations.retain(|c| c.id != conv_id);
        // 删除后按当前顺序重排编号，保持 对话1..对话N 连续。
        for (i, c) in p.conversations.iter_mut().enumerate() {
            c.title = format!("对话{}", i + 1);
        }
        let _ = delete_conversation_file(conv_id);

        if was_active {
            let fallback = p.conversations.last().map(|c| c.id.clone());
            match fallback {
                Some(cid) => {
                    self.active_by_project
                        .insert(project_id.to_string(), cid.clone());
                    if self.active_project.as_deref() == Some(project_id) {
                        self.active_conversation = Some(cid);
                    }
                }
                None => {
                    self.active_by_project.remove(project_id);
                    if self.active_project.as_deref() == Some(project_id) {
                        self.active_conversation = None;
                    }
                }
            }
        }
        Ok(())
    }

    /// 仅把对话从项目中摘除，不删除本地 jsonl，供迁移到其他项目使用。
    pub fn detach_conversation(&mut self, project_id: &str, conv_id: &str) -> Result<(), String> {
        let was_active = self
            .active_by_project
            .get(project_id)
            .map(|id| id == conv_id)
            .unwrap_or(false)
            || (self.active_project.as_deref() == Some(project_id)
                && self.active_conversation.as_deref() == Some(conv_id));

        let p = self.project_mut(project_id).ok_or("项目不存在")?;
        p.conversations.retain(|c| c.id != conv_id);

        if was_active {
            let fallback = p.conversations.last().map(|c| c.id.clone());
            match fallback {
                Some(cid) => {
                    self.active_by_project
                        .insert(project_id.to_string(), cid.clone());
                    if self.active_project.as_deref() == Some(project_id) {
                        self.active_conversation = Some(cid);
                    }
                }
                None => {
                    self.active_by_project.remove(project_id);
                    if self.active_project.as_deref() == Some(project_id) {
                        self.active_conversation = None;
                    }
                }
            }
        }
        Ok(())
    }

    /// 清空本地聊天记录后复位对话元数据，保留对话条目本身。
    pub fn reset_conversation_history(&mut self, project_id: &str, conv_id: &str) {
        if let Some(p) = self.project_mut(project_id) {
            if let Some(c) = p.conversations.iter_mut().find(|c| c.id == conv_id) {
                c.message_count = 0;
                c.last_parent_id = None;
            }
        }
    }

    pub fn set_active(&mut self, project_id: &str, conv_id: &str) -> Result<(), String> {
        let p = self.project(project_id).ok_or("项目不存在")?;
        if !p.conversations.iter().any(|c| c.id == conv_id) {
            return Err("对话不存在".into());
        }
        self.active_project = Some(project_id.to_string());
        self.active_conversation = Some(conv_id.to_string());
        self.active_by_project
            .insert(project_id.to_string(), conv_id.to_string());
        Ok(())
    }

    /// 更新某对话的条数与标题（每轮回复后调用）。
    pub fn touch_conversation(
        &mut self,
        project_id: &str,
        conv_id: &str,
        count: i64,
        title: Option<&str>,
    ) {
        if let Some(p) = self.project_mut(project_id) {
            if let Some(c) = p.conversations.iter_mut().find(|c| c.id == conv_id) {
                c.message_count = count;
                if let Some(t) = title {
                    if !t.trim().is_empty() {
                        c.title = t.trim().to_string();
                    }
                }
            }
        }
    }

    pub fn sub_agents_of(&self, project_id: &str) -> Vec<SubAgent> {
        self.project(project_id)
            .map(|p| p.sub_agents.clone())
            .unwrap_or_default()
    }

    pub fn sub_accounts_of(&self, project_id: &str) -> Vec<String> {
        self.project(project_id)
            .map(|p| p.sub_accounts.clone())
            .unwrap_or_default()
    }

    pub fn set_sub_accounts(&mut self, project_id: &str, accounts: Vec<String>) -> bool {
        match self.project_mut(project_id) {
            Some(p) => { p.sub_accounts = accounts; true }
            None => false,
        }
    }

    pub fn sub_account_limits_of(&self, project_id: &str) -> std::collections::HashMap<String, u32> {
        self.project(project_id)
            .map(|p| p.sub_account_limits.clone())
            .unwrap_or_default()
    }

    pub fn set_sub_account_limits(
        &mut self,
        project_id: &str,
        limits: std::collections::HashMap<String, u32>,
    ) -> bool {
        match self.project_mut(project_id) {
            Some(p) => { p.sub_account_limits = limits; true }
            None => false,
        }
    }

    pub fn sub_account_mode_of(&self, project_id: &str) -> String {
        self.project(project_id)
            .map(|p| if p.sub_account_mode.is_empty() { "permanent".to_string() } else { p.sub_account_mode.clone() })
            .unwrap_or_else(|| "permanent".to_string())
    }

    pub fn set_sub_account_mode(&mut self, project_id: &str, mode: &str) -> bool {
        let mode = match mode {
            "active" => "active",
            _ => "permanent",
        };
        match self.project_mut(project_id) {
            Some(p) => { p.sub_account_mode = mode.to_string(); true }
            None => false,
        }
    }

    /// 删除项目中属于指定账号的全部子 agent，返回其 session_id（供删服务端会话）。
    pub fn remove_sub_agents_by_account(&mut self, project_id: &str, account: &str) -> Vec<String> {
        let Some(p) = self.project_mut(project_id) else { return Vec::new() };
        let mut sids = Vec::new();
        p.sub_agents.retain(|sa| {
            if sa.account == account {
                sids.extend(sa.conversations.iter().map(|c| c.session_id.clone()));
                false
            } else {
                true
            }
        });
        sids
    }

    pub fn find_sub_agent(&self, project_id: &str, name: &str) -> Option<SubAgent> {
        self.project(project_id)?
            .sub_agents
            .iter()
            .find(|s| s.name == name)
            .cloned()
    }

    pub fn add_sub_agent(&mut self, project_id: &str, name: &str, duty: &str, account: &str) -> Option<String> {
        let p = self.project_mut(project_id)?;
        if let Some(existing) = p.sub_agents.iter().find(|s| s.name == name) {
            return Some(existing.id.clone());
        }
        let id = gen_id("sub");
        p.sub_agents.push(SubAgent {
            id: id.clone(),
            name: name.to_string(),
            duty: duty.to_string(),
            account: account.to_string(),
            created_at: now_secs(),
            conversations: Vec::new(),
        });
        Some(id)
    }

    pub fn push_sub_conv(
        &mut self,
        project_id: &str,
        sub_id: &str,
        session_id: &str,
        parent_message_id: Option<i64>,
    ) -> bool {
        let Some(p) = self.project_mut(project_id) else { return false };
        let Some(sa) = p.sub_agents.iter_mut().find(|s| s.id == sub_id) else { return false };
        for c in sa.conversations.iter_mut() {
            c.is_active = false;
        }
        sa.conversations.push(SubAgentConv {
            session_id: session_id.to_string(),
            parent_message_id,
            created_at: now_secs(),
            is_active: true,
        });
        true
    }

    pub fn set_sub_parent(&mut self, project_id: &str, sub_id: &str, parent: Option<i64>) -> bool {
        let Some(p) = self.project_mut(project_id) else { return false };
        let Some(sa) = p.sub_agents.iter_mut().find(|s| s.id == sub_id) else { return false };
        if let Some(c) = sa.conversations.iter_mut().find(|c| c.is_active) {
            c.parent_message_id = parent;
            true
        } else {
            false
        }
    }

    /// 子 agent 首次对话后回写真实 session_id（空占位 → 真实 UUID）。
    pub fn update_sub_conv_session(
        &mut self,
        project_id: &str,
        sub_id: &str,
        old_sid: &str,
        new_sid: &str,
    ) -> bool {
        let Some(p) = self.project_mut(project_id) else { return false };
        let Some(sa) = p.sub_agents.iter_mut().find(|s| s.id == sub_id) else { return false };
        let target = sa.conversations.iter_mut().find(|c| c.is_active && c.session_id == old_sid);
        if let Some(c) = target {
            c.session_id = new_sid.to_string();
            true
        } else {
            false
        }
    }

    pub fn active_sub_conv(&self, project_id: &str, sub_id: &str) -> Option<SubAgentConv> {
        self.project(project_id)?
            .sub_agents
            .iter()
            .find(|s| s.id == sub_id)?
            .conversations
            .iter()
            .find(|c| c.is_active)
            .cloned()
    }

    pub fn remove_sub_agent(&mut self, project_id: &str, sub_id: &str) -> Vec<String> {
        let Some(p) = self.project_mut(project_id) else { return Vec::new() };
        let Some(pos) = p.sub_agents.iter().position(|s| s.id == sub_id) else { return Vec::new() };
        let sa = p.sub_agents.remove(pos);
        sa.conversations.into_iter().map(|c| c.session_id).collect()
    }

    pub fn remove_sub_conv(
        &mut self,
        project_id: &str,
        sub_id: &str,
        session_id: &str,
    ) -> Option<String> {
        let p = self.project_mut(project_id)?;
        let sa = p.sub_agents.iter_mut().find(|s| s.id == sub_id)?;
        let pos = sa.conversations.iter().position(|c| c.session_id == session_id)?;
        let was_active = sa.conversations[pos].is_active;
        sa.conversations.remove(pos);
        if was_active {
            if let Some(last) = sa.conversations.last_mut() {
                last.is_active = true;
            }
        }
        Some(session_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_rename_delete() {
        let mut s = ProjectStore::default();
        let p = s.create_project("测试", "alice");
        assert_eq!(s.projects.len(), 1);
        assert_eq!(s.active_project.as_deref(), Some(p.id.as_str()));

        s.rename_project(&p.id, "改名").unwrap();
        assert_eq!(s.project(&p.id).unwrap().name, "改名");

        s.delete_project(&p.id).unwrap();
        assert!(s.projects.is_empty());
    }

    #[test]
    fn message_file_roundtrip() {
        let cid = "test-conv-roundtrip-xyz";
        let _ = delete_conversation_file(cid);
        append_message(cid, "user", "你好").unwrap();
        append_message(cid, "assistant", "hi <tag>").unwrap();
        let msgs = load_messages(cid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text, "你好");
        assert_eq!(msgs[1].text, "hi <tag>");
        delete_conversation_file(cid).unwrap();
        assert!(load_messages(cid).unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_conv_id() {
        assert!(conversation_file("../evil").is_err());
        assert!(conversation_file("").is_err());
        assert!(conversation_file("a/b").is_err());
    }

    #[test]
    fn add_and_switch_conversation() {
        let mut s = ProjectStore::default();
        let p = s.create_project("p", "alice");
        s.add_conversation(&p.id, "sess-1", "第一条").unwrap();
        assert_eq!(s.active_conversation.as_deref(), Some("sess-1"));
        s.add_conversation(&p.id, "sess-2", "第二条").unwrap();
        assert_eq!(s.project(&p.id).unwrap().conversations.len(), 2);
        assert_eq!(s.active_conversation.as_deref(), Some("sess-2"));

        s.set_active(&p.id, "sess-1").unwrap();
        assert_eq!(s.active_conversation.as_deref(), Some("sess-1"));

        s.delete_conversation(&p.id, "sess-1").unwrap();
        assert_eq!(s.project(&p.id).unwrap().conversations.len(), 1);
    }
}