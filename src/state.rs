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

pub const BRIDGE_IDLE_SECS: u64 = 30;

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
}

pub struct RunCtx {
    pub account: String,
    pub model: Arc<dyn Model>,
    pub runtime: Arc<Runtime>,
    pub session: Session,
    pub cancel: Option<CancellationToken>,
    pub busy: bool,
}

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
    pub qq: Option<QqHandle>,
    /// IDE 文件监听（外部改动同步）。setup 阶段注入。
    pub ide_watcher: Option<crate::ide::watch::IdeWatcher>,
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
                qq: None,
                ide_watcher: None,
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
                    let bridge = BridgeModel::launch(&profile_dir, state_file.as_deref())
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
        if let Some(ctx) = inner.running.get_mut(project_id) {
            if ctx.session.session_id.is_none() {
                ctx.session.session_id = persisted_sid;
            }
            if ctx.session.parent_message_id.is_none() {
                ctx.session.parent_message_id = persisted_parent;
            }
            ctx.session.free_mode = persisted_free;
            let model = ctx.model.clone();
            drop(inner);
            self.touch_bridge(&account).await;
            return Ok(model);
        }
        let mut session = Session::default();
        session.session_id = persisted_sid;
        session.parent_message_id = persisted_parent;
        session.free_mode = persisted_free;
        inner.running.insert(
            project_id.to_string(),
            RunCtx {
                account: account.clone(),
                model: model.clone(),
                runtime,
                session,
                cancel: None,
                busy: false,
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
        let bridge = BridgeModel::launch(&profile_dir, state_file.as_deref())
            .await
            .map_err(|e| format!("启动 bridge 失败: {e}"))?;
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
        closed
    }

    pub async fn close_idle_bridges(&self, idle: Duration) -> usize {
        Self::close_idle(&self.inner, idle).await
    }

    pub async fn close_all_bridges(&self) {
        let mut inner = self.inner.lock().await;
        inner.bridges.clear();
        inner.bridge_last_used.clear();
    }
}