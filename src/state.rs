use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::projects::ProjectStore;
use crate::tools::{EditFile, ExecuteCommand, ReadFile, WriteFile};
use crate::qq::QqHandle;
use deepseek_agent::accounts::AccountManager;
use deepseek_agent::agent::model::Model;
use deepseek_agent::bridge::BridgeModel;
use deepseek_agent::agent::tool::ToolRegistry;
use deepseek_agent::{Config, Runtime};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub const BRIDGE_IDLE_SECS: u64 = 10;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingFile {
    pub id: String,
    pub name: String,
    pub size: i64,
    pub is_image: bool,
    #[serde(default)]
    pub path: String,
}

#[derive(Default)]
pub struct Session {
    pub session_id: Option<String>,
    pub parent_message_id: Option<i64>,
    pub prompt_sent: bool,
    pub thinking: bool,
    pub search: bool,
    pub free_mode: bool,
    pub pending_files: Vec<PendingFile>,
    pub announced_workdir: Option<String>,
    /// 压缩交接后等待真实 session UUID 绑定（网页内核 new_session 返回空串时置位）。
    pub pending_handoff: bool,
    /// 交接报告文本，待真实 session 绑定后随首轮一并带入（防止空 session 被持久化）。
    pub pending_report: Option<String>,
}

pub struct RunCtx {
    pub account: String,
    pub model: Arc<dyn Model>,
    pub runtime: Arc<Runtime>,
    pub session: Session,
    pub cancel: Option<CancellationToken>,
    pub busy: bool,
    pub turn_active: bool,
    /// 当前主 turn 的代数。每次 start_turn 自增；收尾回调只在代数匹配时清理状态。
    pub turn_gen: u64,
    /// 本轮是否被用户请求取消。为真时收尾不得自动唤起审阅 turn。
    pub cancel_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum SubRunStatus {
    Starting,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Closed,
}

pub struct SubRunCtx {
    pub sid: String,
    pub name: String,
    pub duty: String,
    pub project_id: String,
    pub account: String,
    pub agent_type: String,
    pub profile_id: String,
    pub status: SubRunStatus,
    pub cancel: CancellationToken,
    /// 运行实例代数。清理任务和结果回写必须校验，防同名重启覆盖。
    pub run_generation: u64,
    pub started_at: Instant,
    pub attempts: u8,
    pub last_text: Option<String>,
    pub last_ok: Option<bool>,
    /// 最近一次派发的任务文本，供"重试"原样重派。
    pub last_task: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SubResult {
    pub name: String,
    pub text: String,
    pub ok: bool,
}

pub type StartTurnFn = std::sync::Arc<
    dyn Fn(
            tauri::AppHandle,
            String,
            String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub struct Inner {
    pub tools: Arc<ToolRegistry>,
    pub projects: ProjectStore,
    pub running: HashMap<String, RunCtx>,
    pub approvals: HashMap<u64, (String, tokio::sync::oneshot::Sender<bool>)>,
    pub next_approval_id: u64,
    pub bridges: HashMap<String, Arc<BridgeModel>>,
    pub account_locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    pub init_locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    pub bridge_last_used: HashMap<String, Instant>,
    /// 子 bridge（key = `{account}#sub-{sid}`）上次空闲时间。用于 10s TTL 复用。
    pub sub_bridge_last_used: HashMap<String, Instant>,
    pub qq: Option<QqHandle>,
    /// IDE 文件监听（外部改动同步）。setup 阶段注入。
    pub ide_watcher: Option<crate::ide::watch::IdeWatcher>,
    /// 子 agent 运行态。key = sid。
    pub sub_runs: HashMap<String, SubRunCtx>,
    /// 账号级限流：上次请求时间。
    pub account_last_at: HashMap<String, Instant>,
    /// 子 agent 待注入主 agent 的结果。key = project_id。
    pub pending_sub_results: HashMap<String, Vec<SubResult>>,
    /// 唤起主 agent 一轮的间接入口（打破 async 类型递归环）。
    pub start_turn: Option<StartTurnFn>,
}

pub struct AppState {
    pub inner: Arc<Mutex<Inner>>,
    last_request_at: Mutex<Option<Instant>>,
}

impl Default for AppState {
    fn default() -> Self {
        let mut tools = ToolRegistry::new();
        let cwd = std::env::current_dir().unwrap_or_default();
        tools.register(Box::new(ExecuteCommand::new(cwd.clone())));
        tools.register(Box::new(ReadFile::new(cwd.clone())));
        tools.register(Box::new(WriteFile::new(cwd.clone())));
        tools.register(Box::new(EditFile::new(cwd.clone())));
        
        Self {
            inner: Arc::new(Mutex::new(Inner {
                tools: Arc::new(tools),
                projects: ProjectStore::load(),
                running: HashMap::new(),
                approvals: HashMap::new(),
                next_approval_id: 1,
                bridges: HashMap::new(),
                account_locks: HashMap::new(),
                init_locks: HashMap::new(),
                bridge_last_used: HashMap::new(),
                sub_bridge_last_used: HashMap::new(),
                qq: None,
                ide_watcher: None,
                sub_runs: HashMap::new(),
                account_last_at: HashMap::new(),
                pending_sub_results: HashMap::new(),
                start_turn: None,
            })),
            last_request_at: Mutex::new(None),
        }
    }
}

impl Inner {
    pub fn run_ctx_project_account(&self, project_id: &str) -> Result<String, String> {
        self.projects
            .project(project_id)
            .map(|p| p.account.clone())
            .ok_or_else(|| "项目不存在".to_string())
    }
}

fn ligong_profile_dir(account: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".deepseek-agent").join("accounts").join(account).join("browser")
}

fn home_state_file(account: &str) -> Option<PathBuf> {
    let p = dirs::home_dir()?
        .join(".ligong").join("accounts").join(account).join("state.json");
    if p.exists() { Some(p) } else { None }
}

impl AppState {
    pub fn list_accounts() -> Vec<String> {
        AccountManager::load_default()
            .map(|m| m.list().iter().map(|a| a.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn config_for(name: &str) -> Result<Config, String> {
        Config::for_account(name).map_err(|e| e.to_string())
    }

    pub async fn ensure_run_ctx(&self, project_id: &str) -> Result<Arc<dyn Model>, String> {
        {
            let mut inner = self.inner.lock().await;
            // 若 running 有该项目但其 account 的 bridge 已死（被 close_idle 移除），
            // 清掉 running 项，下方走重建分支。
            let stale = inner.running.get(project_id).map(|c| {
                (c.account.clone(), !inner.bridges.contains_key(&c.account))
            });
            if let Some((_, true)) = stale {
                inner.running.remove(project_id);
            }
            let persisted_sid = inner.projects.active_conversation_of(project_id);
            let persisted_parent = persisted_sid
                .as_deref()
                .and_then(|sid| {
                    inner
                        .projects
                        .project(project_id)
                        .and_then(|p| p.conversations.iter().find(|c| c.id == sid))
                        .and_then(|c| c.last_parent_id)
                });
            let persisted_free = inner.projects.free_mode_of(project_id);
            if let Some(ctx) = inner.running.get_mut(project_id) {
                if ctx.session.session_id.is_none() {
                    ctx.session.session_id = persisted_sid;
                }
                if ctx.session.parent_message_id.is_none() {
                    ctx.session.parent_message_id = persisted_parent;
                }
                ctx.session.free_mode = persisted_free;
                let account = ctx.account.clone();
                let model = ctx.model.clone();
                drop(inner);
                self.touch_bridge(&account).await;
                return Ok(model);
            }
        }
        let account = {
            let inner = self.inner.lock().await;
            inner.run_ctx_project_account(project_id)?
        };
        let cfg = Self::config_for(&account)?;
        let rt = Runtime::new_lazy(&cfg).await.map_err(|e| e.to_string())?;
        let runtime = Arc::new(rt);

        let profile_dir = ligong_profile_dir(&account);
        if !profile_dir.exists() {
            return Err(format!(
                "账号 {} 的浏览器 profile 缺失，请先登录生成 {}",
                account,
                profile_dir.display()
            ));
        }

        let model: Arc<dyn Model> = {
            let init_lock = self.init_lock(&account).await;
            let _init_guard = init_lock.lock().await;
            let existing = {
                let inner = self.inner.lock().await;
                inner.bridges.get(&account).cloned()
            };
            match existing {
                Some(bm) => bm,
                None => {
                    let state_file = home_state_file(&account);
                    let bridge = BridgeModel::launch(&profile_dir, state_file.as_deref(), false)
                        .await
                        .map_err(|e| format!("启动浏览器 bridge 失败: {e}"))?;
                    self.inner.lock().await.bridges.insert(account.clone(), bridge.clone());
                    bridge
                }
            }
        };

        let mut inner = self.inner.lock().await;
        let persisted_sid = inner.projects.active_conversation_of(project_id);
        let persisted_parent = persisted_sid
            .as_deref()
            .and_then(|sid| {
                inner
                    .projects
                    .project(project_id)
                    .and_then(|p| p.conversations.iter().find(|c| c.id == sid))
                    .and_then(|c| c.last_parent_id)
            });
        let persisted_free = inner.projects.free_mode_of(project_id);
        let persisted_thinking = inner.projects.thinking_of(project_id);
        let persisted_search = inner.projects.search_of(project_id);
        if let Some(ctx) = inner.running.get_mut(project_id) {
            if ctx.session.session_id.is_none() {
                ctx.session.session_id = persisted_sid;
            }
            if ctx.session.parent_message_id.is_none() {
                ctx.session.parent_message_id = persisted_parent;
            }
            ctx.session.free_mode = persisted_free;
            ctx.session.thinking = persisted_thinking;
            ctx.session.search = persisted_search;
            let model = ctx.model.clone();
            drop(inner);
            self.touch_bridge(&account).await;
            return Ok(model);
        }
        let mut session = Session::default();
        session.session_id = persisted_sid;
        session.parent_message_id = persisted_parent;
        session.free_mode = persisted_free;
        session.thinking = persisted_thinking;
        session.search = persisted_search;
        inner.running.insert(
            project_id.to_string(),
            RunCtx {
                account: account.clone(),
                model: model.clone(),
                runtime,
                session,
                cancel: None,
                busy: false,
                turn_active: false,
                turn_gen: 0,
                cancel_requested: false,
            },
        );
        drop(inner);
        self.touch_bridge(&account).await;
        Ok(model)
    }

    pub async fn is_busy(&self, project_id: &str) -> bool {
        self.inner
            .lock()
            .await
            .running
            .get(project_id)
            .map(|c| c.busy)
            .unwrap_or(false)
    }

    pub async fn any_busy(&self) -> bool {
        self.inner.lock().await.running.values().any(|c| c.busy)
    }

    pub async fn throttle(&self, min_gap: Duration, cancel: &CancellationToken) -> bool {
        let wait = {
            let mut guard = self.last_request_at.lock().await;
            let now = Instant::now();
            let wait = match *guard {
                Some(prev) => {
                    let elapsed = now.duration_since(prev);
                    min_gap.checked_sub(elapsed).unwrap_or(Duration::ZERO)
                }
                None => Duration::ZERO,
            };
            *guard = Some(now + wait);
            wait
        };
        if wait.is_zero() {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => true,
            _ = cancel.cancelled() => false,
        }
    }

    /// 账号级最小请求间隔：同账号串行间隔，不同账号互不影响。
    pub async fn throttle_account(
        &self,
        account: &str,
        min_gap: Duration,
        cancel: &CancellationToken,
    ) -> bool {
        loop {
            let wait = {
                let mut inner = self.inner.lock().await;
                let now = Instant::now();
                let last = inner.account_last_at.get(account).copied();
                match last {
                    Some(t) if now.duration_since(t) < min_gap => {
                        min_gap - now.duration_since(t)
                    }
                    _ => {
                        inner.account_last_at.insert(account.to_string(), now);
                        return true;
                    }
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = cancel.cancelled() => return false,
            }
        }
    }

    pub async fn account_lock(&self, account: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut inner = self.inner.lock().await;
        inner.account_locks
            .entry(account.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub async fn init_lock(&self, account: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut inner = self.inner.lock().await;
        inner.init_locks
            .entry(account.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 为子 agent 创建独立空 profile 目录（不复制 master，避免并发 leveldb 不一致）。
    /// 登录态由 state.json 注入（cookies + localStorage）。
    pub fn clone_profile(account: &str, sid: &str) -> Result<PathBuf, String> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let sub_root = home
            .join(".deepseek-agent")
            .join("accounts")
            .join(account)
            .join("sub")
            .join(sid);
        let dst = sub_root.join("browser");
        std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
        Ok(dst)
    }

    /// 取（或建）某子 agent 的 bridge。bridge_key = "<account>#sub-<sid>"。
    pub async fn ensure_sub_bridge(
        &self,
        account: &str,
        sid: &str,
    ) -> Result<Arc<BridgeModel>, String> {
        let key = format!("{account}#sub-{sid}");
        {
            let mut inner = self.inner.lock().await;
            if let Some(b) = inner.bridges.get(&key) {
                // 复用：刷新空闲时间戳，避免被 idle worker 回收。
                let b = b.clone();
                inner.sub_bridge_last_used.insert(key.clone(), Instant::now());
                return Ok(b);
            }
        }
        let profile = Self::clone_profile(account, sid)?;
        let state_file = home_state_file(account);
        let init_lock = self.init_lock(&key).await;
        let _g = init_lock.lock().await;
        {
            let inner = self.inner.lock().await;
            if let Some(b) = inner.bridges.get(&key) {
                return Ok(b.clone());
            }
        }
        let bridge = BridgeModel::launch(&profile, state_file.as_deref(), true)
            .await
            .map_err(|e| format!("启动子 bridge 失败: {e}"))?;
        // P1-6：子 bridge 同账号登录态失效时明确报错。
        let auth = bridge.auth_status().await;
        if auth == "login" {
            bridge.shutdown().await;
            return Err(format!(
                "账号 {account} 登录态已失效，请重新登录后重试子 agent",
            ));
        }
        {
            let mut inner = self.inner.lock().await;
            inner.bridges.insert(key.clone(), bridge.clone());
            inner.sub_bridge_last_used.insert(key, Instant::now());
        }
        Ok(bridge)
    }

    /// 标记子 bridge 空闲（不再立即销毁）。统一由 idle worker 在 TTL 后回收，
    /// 保留 profile 以支持短时间内同名重派复用（免重启 chromium）。
    pub async fn release_sub_bridge(&self, account: &str, sid: &str) {
        let key = format!("{account}#sub-{sid}");
        let mut inner = self.inner.lock().await;
        inner.sub_bridge_last_used.insert(key, Instant::now());
    }

    /// 立即销毁子 bridge 与其 profile（删除子 agent / 清理账号等场景）。
    pub async fn destroy_sub_bridge(&self, account: &str, sid: &str) {
        let key = format!("{account}#sub-{sid}");
        let bridge = {
            let mut inner = self.inner.lock().await;
            inner.sub_bridge_last_used.remove(&key);
            inner.bridges.remove(&key)
        };
        if let Some(b) = bridge {
            b.shutdown().await;
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let sub_dir = home
            .join(".deepseek-agent")
            .join("accounts")
            .join(account)
            .join("sub")
            .join(sid);
        let _ = std::fs::remove_dir_all(&sub_dir);
    }

    pub async fn ensure_account_model(&self, account: &str) -> Result<Arc<BridgeModel>, String> {
        let init_lock = self.init_lock(account).await;
        let _init_guard = init_lock.lock().await;
        {
            let inner = self.inner.lock().await;
            if let Some(bm) = inner.bridges.get(account) {
                return Ok(bm.clone());
            }
        }
        let profile_dir = ligong_profile_dir(account);
        if !profile_dir.exists() {
            return Err(format!(
                "账号 {} 的浏览器 profile 缺失，请先登录生成 {}",
                account,
                profile_dir.display()
            ));
        }
        let state_file = home_state_file(account);
        let bridge = BridgeModel::launch(&profile_dir, state_file.as_deref(), false)
            .await
            .map_err(|e| format!("启动 bridge 失败: {e}"))?;
        // P1-6：启动后若检测到登录态失效，明确报错并回收 bridge。
        let auth = bridge.auth_status().await;
        if auth == "login" {
            bridge.shutdown().await;
            return Err(format!(
                "账号 {account} 登录态已失效，请重新登录该账号",
            ));
        }
        self.inner.lock().await.bridges.insert(account.to_string(), bridge.clone());
        Ok(bridge)
    }

    pub async fn touch_bridge(&self, account: &str) {
        let mut inner = self.inner.lock().await;
        inner
            .bridge_last_used
            .insert(account.to_string(), Instant::now());
    }

    pub async fn close_idle(inner: &Arc<Mutex<Inner>>, max_idle: Duration) -> usize {
        let mut guard = inner.lock().await;
        let now = Instant::now();
        let stale: Vec<String> = guard
            .bridge_last_used
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= max_idle)
            .map(|(k, _)| k.clone())
            .collect();
        let mut closed = 0usize;
        for account in stale {
            // 只跳过"正在处理中"的 account；对话完成后 busy=false，会被正常关闭。
            let busy = guard.running.values().any(|c| c.account == account && c.busy);
            if busy {
                guard.bridge_last_used.insert(account.clone(), Instant::now());
                continue;
            }
            let bridge = guard.bridges.remove(&account);
            guard.bridge_last_used.remove(&account);
            // running 里已确认无该项目，清理同 account 的残留项（防御，正常为空）。
            let stale_projects: Vec<String> = guard
                .running
                .iter()
                .filter(|(_, c)| c.account == account)
                .map(|(k, _)| k.clone())
                .collect();
            for pid in stale_projects {
                guard.running.remove(&pid);
            }
            drop(guard);
            if let Some(bridge) = bridge {
                bridge.shutdown().await;
            }
            guard = inner.lock().await;
            closed += 1;
        }
        // 子 bridge：超过 TTL 且对应 sid 非运行中 → 回收 bridge 并删 profile。
        let stale_sub: Vec<String> = guard
            .sub_bridge_last_used
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= max_idle)
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale_sub {
            let sid = key.split_once("#sub-").map(|(_, s)| s.to_string());
            let running = sid
                .as_deref()
                .map(|s| {
                    guard.sub_runs.get(s).map(|r| {
                        matches!(
                            r.status,
                            SubRunStatus::Starting | SubRunStatus::Running
                        )
                    }).unwrap_or(false)
                })
                .unwrap_or(false);
            if running {
                // 运行中：刷新时间戳，跳过回收。
                guard.sub_bridge_last_used.insert(key, Instant::now());
                continue;
            }
            let bridge = guard.bridges.remove(&key);
            guard.sub_bridge_last_used.remove(&key);
            let account = key.split_once("#sub-").map(|(a, _)| a.to_string());
            drop(guard);
            if let Some(bridge) = bridge {
                bridge.shutdown().await;
            }
            // 删 profile 目录。
            if let (Some(account), Some(sid)) = (account, sid) {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                let sub_dir = home
                    .join(".deepseek-agent")
                    .join("accounts")
                    .join(&account)
                    .join("sub")
                    .join(&sid);
                let _ = std::fs::remove_dir_all(&sub_dir);
            }
            guard = inner.lock().await;
            closed += 1;
        }
        closed
    }

    pub async fn close_idle_bridges(&self, idle: Duration) -> usize {
        Self::close_idle(&self.inner, idle).await
    }

    pub async fn close_all_bridges(&self) {
        let bridges: Vec<Arc<BridgeModel>> = {
            let mut inner = self.inner.lock().await;
            let list = inner.bridges.values().cloned().collect();
            inner.bridges.clear();
            inner.bridge_last_used.clear();
            inner.sub_bridge_last_used.clear();
            list
        };
        for b in bridges {
            b.shutdown().await;
        }
    }
}