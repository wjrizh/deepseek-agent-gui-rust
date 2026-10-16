//! Tauri 命令：账号、发消息、打断、审批、会话管理。

use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

use deepseek_agent::agent::model::Model;
use deepseek_agent::auth::TokenRecord;

use crate::projects::{Conversation, Project};
use crate::runner;
use crate::state::{AppState, PendingFile};

fn save_projects(inner: &crate::state::Inner) -> Result<(), String> {
    inner.projects.save()
}

/// P2-3 错误分层：把任意错误串归类，产出结构化字段（保留原始 message 兼容旧前端）。
/// 返回 (category, user_message, retryable)。
pub fn classify_error(msg: &str) -> (&'static str, String, bool) {
    let m = msg.to_lowercase();
    let has = |k: &str| m.contains(k);
    if has("登录") || has("login") || has("auth") || has("未登录") || has("登录态") {
        ("auth", "账号登录态已失效，请重新登录该账号后重试。".to_string(), false)
    } else if has("取消") || has("cancel") {
        ("cancelled", "已取消。".to_string(), true)
    } else if has("超时") || has("timeout") || has("timed out") || has("连接") || has("network")
        || has("econn") || has("socket") || has("dns") || has("bridge") || has("启动")
    {
        ("network", format!("网络或内核连接异常，可稍后重试。（{msg}）"), true)
    } else if has("限流") || has("rate") || has("429") || has("额度") || has("quota") || has("too many") {
        ("rate_limit", "请求过于频繁或额度受限，请稍后重试。".to_string(), true)
    } else if has("格式") || has("format") || has("解析") || has("parse") {
        ("format", "模型输出格式异常，可重试本轮。".to_string(), true)
    } else if has("审批") || has("approval") || has("拒绝") || has("denied") {
        ("approval", "工具调用被拒绝或审批未通过。".to_string(), false)
    } else {
        ("unknown", msg.to_string(), true)
    }
}

/// 构造统一的 `agent://error` 载荷（分层字段 + 原始 message）。
pub fn error_payload(
    project_id: &str,
    message: &str,
    account: Option<&str>,
    sub_id: Option<&str>,
) -> serde_json::Value {
    let (category, user_message, retryable) = classify_error(message);
    serde_json::json!({
        "project_id": project_id,
        "category": category,
        "user_message": user_message,
        "technical_message": message,
        "retryable": retryable,
        "account": account.unwrap_or(""),
        "sub_id": sub_id.unwrap_or(""),
        "message": message,
    })
}

fn delete_gap() -> std::time::Duration {
    let ms = 3000 + {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        nanos % 2001
    };
    std::time::Duration::from_millis(ms)
}

async fn account_guard(
    state: &AppState,
    project_id: &str,
) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    let account = {
        let inner = state.inner.lock().await;
        inner
            .running
            .get(project_id)
            .map(|c| c.account.clone())
            .or_else(|| {
                inner
                    .projects
                    .project(project_id)
                    .map(|p| p.account.clone())
            })
    }?;
    let lock = state.account_lock(&account).await;
    Some(lock.lock_owned().await)
}

#[derive(serde::Serialize)]
pub struct AccountStatus {
    pub accounts: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct ConversationView {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub message_count: i64,
    pub active: bool,
}

#[derive(serde::Serialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub account: String,
    pub created_at: u64,
    pub conversation_count: usize,
    pub active: bool,
    pub working: bool,
}

fn conv_view(c: &Conversation, active: &Option<String>) -> ConversationView {
    ConversationView {
        id: c.id.clone(),
        title: c.title.clone(),
        created_at: c.created_at,
        message_count: c.message_count,
        active: active.as_deref() == Some(c.id.as_str()),
    }
}

fn project_view(p: &Project, active: &Option<String>, working: bool) -> ProjectView {
    ProjectView {
        id: p.id.clone(),
        name: p.name.clone(),
        account: p.account.clone(),
        created_at: p.created_at,
        conversation_count: p.conversations.len(),
        active: active.as_deref() == Some(p.id.as_str()),
        working,
    }
}



#[tauri::command]
pub async fn account_status() -> Result<AccountStatus, String> {
    Ok(AccountStatus {
        accounts: AppState::list_accounts(),
    })
}

#[tauri::command]
pub async fn list_accounts() -> Result<Vec<String>, String> {
    Ok(AppState::list_accounts())
}

#[tauri::command]
pub async fn login(name: String, token: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("账号名不能为空".into());
    }
    deepseek_agent::accounts::AccountManager::validate_name(&name).map_err(|e| e.to_string())?;
    let mut mgr =
        deepseek_agent::accounts::AccountManager::load_default().map_err(|e| e.to_string())?;
    if !mgr.exists(&name) {
        mgr.add(&name).map_err(|e| e.to_string())?;
    }
    let token_file = mgr.token_file(&name);
    if let Some(dir) = token_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let rec = TokenRecord::new(token.trim().to_string(), "gui");
    let json = serde_json::to_string_pretty(&rec).map_err(|e| e.to_string())?;
    std::fs::write(&token_file, json).map_err(|e| e.to_string())?;
    mgr.touch(&name).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- 项目管理 ----------

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectView>, String> {
    let inner = state.inner.lock().await;
    let active = inner.projects.active_project.clone();
    Ok(inner
        .projects
        .projects
        .iter()
        .map(|p| {
            project_view(
                p,
                &active,
                inner.running.get(&p.id).map(|c| c.busy).unwrap_or(false),
            )
        })
        .collect())
}

#[tauri::command]
pub async fn create_project(
    name: String,
    account: String,
    state: State<'_, AppState>,
) -> Result<ProjectView, String> {
    let name = name.trim();
    let account = account.trim();
    if name.is_empty() {
        return Err("项目名不能为空".into());
    }
    if account.is_empty() {
        return Err("请选择账号".into());
    }
    let mut inner = state.inner.lock().await;
    let p = inner.projects.create_project(name, account);
    save_projects(&inner)?;
    let active = inner.projects.active_project.clone();
    Ok(project_view(&p, &active, false))
}

#[tauri::command]
pub async fn rename_project(
    id: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("项目名不能为空".into());
    }
    let mut inner = state.inner.lock().await;
    inner.projects.rename_project(&id, name)?;
    save_projects(&inner)
}

#[tauri::command]
pub async fn delete_project(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (main_ids, sub_by_account): (
        Vec<String>,
        std::collections::HashMap<String, Vec<String>>,
    ) = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .project(&id)
            .map(|p| {
                let main: Vec<String> = p.conversations.iter().map(|c| c.id.clone()).collect();
                let mut by: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();
                for sa in &p.sub_agents {
                    let acc = if sa.account.is_empty() {
                        p.account.clone()
                    } else {
                        sa.account.clone()
                    };
                    for c in &sa.conversations {
                        if !c.session_id.is_empty() {
                            by.entry(acc.clone()).or_default().push(c.session_id.clone());
                        }
                    }
                }
                (main, by)
            })
            .unwrap_or_default()
    };

    // 取消该项目下所有运行中子 agent，并清理待注入结果与主 turn 取消钩子。
    {
        let mut inner = state.inner.lock().await;
        for r in inner.sub_runs.values() {
            if r.project_id == id
                && matches!(
                    r.status,
                    crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                )
            {
                r.cancel.cancel();
            }
        }
        if let Some(ctx) = inner.running.get(&id) {
            if let Some(c) = &ctx.cancel {
                c.cancel();
            }
        }
        inner.pending_sub_results.remove(&id);
    }

    let total = main_ids.len() + sub_by_account.values().map(|v| v.len()).sum::<usize>();
    let mut done = 0usize;
    let mut failed: Vec<String> = Vec::new();

    let main_model = state.ensure_run_ctx(&id).await.ok();

    for cid in &main_ids {
        done += 1;
        let _ = app.emit(
            "agent://delete-progress",
            serde_json::json!({ "phase": "deleting", "current": done, "total": total }),
        );
        if let Some(m) = &main_model {
            if let Err(e) = m.delete_session(cid).await {
                failed.push(format!("{cid}: {e}"));
            }
        }
        {
            let mut inner = state.inner.lock().await;
            let _ = inner.projects.delete_conversation(&id, cid);
            let _ = save_projects(&inner);
        }
        if done < total {
            tokio::time::sleep(delete_gap()).await;
        }
    }

    for (acc, sids) in &sub_by_account {
        let m = match state.ensure_account_model(acc).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("删除子对话：账号 {acc} bridge 不可用: {e}");
                continue;
            }
        };
        for sid in sids {
            done += 1;
            let _ = app.emit(
                "agent://delete-progress",
                serde_json::json!({ "phase": "deleting", "current": done, "total": total }),
            );
            if let Err(e) = m.delete_session(sid).await {
                failed.push(format!("{sid}: {e}"));
            }
            if done < total {
                tokio::time::sleep(delete_gap()).await;
            }
        }
    }

    let _ = app.emit(
        "agent://delete-progress",
        serde_json::json!({ "phase": "finishing", "current": total, "total": total }),
    );

    let mut inner = state.inner.lock().await;
    inner.projects.delete_project(&id)?;
    save_projects(&inner)?;
    inner.running.remove(&id);
    // 清理该项目残留的子 agent 运行态与待注入结果。
    // 仅立即释放"非运行中"的 bridge/profile；仍在运行的任务已收到 cancel，
    // 由其收尾的 3s 清理任务负责释放，避免任务还在用 bridge 时被切断（删除屏障）。
    let stale_sub: Vec<(String, String)> = inner
        .sub_runs
        .values()
        .filter(|r| {
            r.project_id == id
                && !matches!(
                    r.status,
                    crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                )
        })
        .map(|r| (r.account.clone(), r.profile_id.clone()))
        .collect();
    inner.sub_runs.retain(|_, r| {
        !(r.project_id == id
            && !matches!(
                r.status,
                crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
            ))
    });
    inner.pending_sub_results.remove(&id);
    drop(inner);
    for (acc, prof) in stale_sub {
        state.destroy_sub_bridge(&acc, &prof).await;
    }
    deepseek_agent::agent::browser::shutdown_key(&id).await;
    let _ = app.emit("agent://projects-changed", ());
    let _ = app.emit(
        "agent://delete-progress",
        serde_json::json!({
            "phase": "done",
            "current": total,
            "total": total,
            "failed": failed,
        }),
    );
    Ok(())
}

#[tauri::command]
pub async fn open_project(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    inner.projects.project(&id).ok_or("项目不存在")?;

    let target = inner.projects.active_conversation_of(&id).or_else(|| {
        inner
            .projects
            .project(&id)
            .and_then(|p| p.conversations.last().map(|c| c.id.clone()))
    });

    inner
        .projects
        .set_active_conversation_of(&id, target.as_deref());

    if let Some(ctx) = inner.running.get_mut(&id) {
        let switched = ctx.session.session_id != target;
        ctx.session.session_id = target.clone();
        if switched {
            ctx.session.parent_message_id = None;
            ctx.session.prompt_sent = false;
        }
    }
    let warmup = inner.projects.warmup_of(&id);
    save_projects(&inner)?;
    drop(inner);
    let _ = app.emit("agent://projects-changed", ());
    // P1-2：可选预热（默认关）。开启后打开项目即后台预启动主 bridge，减少首字延迟。
    if warmup {
        let app2 = app.clone();
        let pid = id.clone();
        tauri::async_runtime::spawn(async move {
            let st = app2.state::<AppState>();
            let _ = st.ensure_run_ctx(&pid).await;
        });
    }
    Ok(())
}

#[tauri::command]
pub async fn list_sub_agents(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<crate::projects::SubAgent>, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.sub_agents_of(&project_id))
}

/// 运行中子 agent 状态快照，供主 UI / IDE 初始化状态条（与 sub-* 事件同一状态集）。
#[tauri::command]
pub async fn list_sub_runs(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let inner = state.inner.lock().await;
    Ok(collect_sub_runs(&inner, &project_id))
}

fn collect_sub_runs(inner: &crate::state::Inner, project_id: &str) -> Vec<serde_json::Value> {
    use crate::state::SubRunStatus;
    let mut out = Vec::new();
    for r in inner.sub_runs.values() {
        if r.project_id != project_id {
            continue;
        }
        let status = match r.status {
            SubRunStatus::Starting => "starting",
            SubRunStatus::Running => "running",
            SubRunStatus::Succeeded => "succeeded",
            SubRunStatus::Failed => "failed",
            SubRunStatus::Cancelled => "cancelled",
            SubRunStatus::Closed => "closed",
        };
        out.push(serde_json::json!({
            "project_id": r.project_id,
            "sub_id": r.sid,
            "name": r.name,
            "agent_type": r.agent_type,
            "account": r.account,
            "status": status,
            "run_generation": r.run_generation,
            "elapsed_ms": r.started_at.elapsed().as_millis() as u64,
            "last_ok": r.last_ok,
            "last_text": r.last_text,
        }));
    }
    out
}

/// IDE Agent 面板初始化：主 turn 运行态 + 子 agent 快照，一次拿全。
#[tauri::command]
pub async fn get_project_runtime(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let inner = state.inner.lock().await;
    let (busy, turn_active, turn_gen) = inner
        .running
        .get(&project_id)
        .map(|c| (c.busy, c.turn_active, c.turn_gen))
        .unwrap_or((false, false, 0));
    let pending = inner
        .pending_sub_results
        .get(&project_id)
        .map(|v| v.len())
        .unwrap_or(0);
    let sub_runs = collect_sub_runs(&inner, &project_id);
    Ok(serde_json::json!({
        "project_id": project_id,
        "busy": busy,
        "turn_active": turn_active,
        "turn_gen": turn_gen,
        "pending_sub_results": pending,
        "sub_runs": sub_runs,
    }))
}

#[tauri::command]
pub async fn get_sub_accounts(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.sub_accounts_of(&project_id))
}

#[tauri::command]
pub async fn set_sub_accounts(
    project_id: String,
    accounts: Vec<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut inner = state.inner.lock().await;
        inner.projects.set_sub_accounts(&project_id, accounts);
        let _ = inner.projects.save();
    }
    let _ = app.emit("agent://projects-changed", ());
    Ok(())
}

#[tauri::command]
pub async fn get_sub_account_limits(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, u32>, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.sub_account_limits_of(&project_id))
}

#[tauri::command]
pub async fn set_sub_account_limits(
    project_id: String,
    limits: std::collections::HashMap<String, u32>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut inner = state.inner.lock().await;
        inner.projects.set_sub_account_limits(&project_id, limits);
        let _ = inner.projects.save();
    }
    let _ = app.emit("agent://projects-changed", ());
    Ok(())
}

#[tauri::command]
pub async fn get_sub_account_mode(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.sub_account_mode_of(&project_id))
}

#[tauri::command]
pub async fn set_sub_account_mode(
    project_id: String,
    mode: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut inner = state.inner.lock().await;
        inner.projects.set_sub_account_mode(&project_id, &mode);
        let _ = inner.projects.save();
    }
    let _ = app.emit("agent://projects-changed", ());
    Ok(())
}

#[tauri::command]
pub async fn list_sub_candidate_accounts() -> Result<Vec<String>, String> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut out = Vec::new();
    for name in AppState::list_accounts() {
        let profile = home
            .join(".deepseek-agent")
            .join("accounts")
            .join(&name)
            .join("browser");
        let state_json = home
            .join(".ligong")
            .join("accounts")
            .join(&name)
            .join("state.json");
        if profile.is_dir() && state_json.is_file() {
            out.push(name);
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn remove_sub_account(
    project_id: String,
    account: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // 先取消该项目下该账号的所有运行中子 agent，避免删除后后台任务继续运行。
    {
        let inner = state.inner.lock().await;
        for r in inner.sub_runs.values() {
            if r.project_id == project_id
                && r.account == account
                && matches!(
                    r.status,
                    crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                )
            {
                r.cancel.cancel();
            }
        }
    }
    let sids = {
        let mut inner = state.inner.lock().await;
        let mut pool = inner.projects.sub_accounts_of(&project_id);
        pool.retain(|a| a != &account);
        inner.projects.set_sub_accounts(&project_id, pool);
        let sids = inner.projects.remove_sub_agents_by_account(&project_id, &account);
        let _ = inner.projects.save();
        sids
    };
    if let Ok(model) = state.ensure_account_model(&account).await {
        for sid in &sids {
            if !sid.is_empty() {
                let _ = model.delete_session(sid).await;
            }
        }
    }
    // 仅立即释放"非运行中"的子 bridge/profile；仍在运行的任务已收到 cancel，
    // 由其收尾的 3s 清理任务负责释放，避免任务还在用 bridge 时被切断（删除屏障）。
    let keys: Vec<(String, String)> = {
        let inner = state.inner.lock().await;
        inner
            .sub_runs
            .values()
            .filter(|r| {
                r.project_id == project_id
                    && r.account == account
                    && !matches!(
                        r.status,
                        crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                    )
            })
            .map(|r| (r.account.clone(), r.profile_id.clone()))
            .collect()
    };
    for (acc, pid_profile) in keys {
        state.destroy_sub_bridge(&acc, &pid_profile).await;
    }
    {
        let mut inner = state.inner.lock().await;
        inner.sub_runs.retain(|_, r| {
            !(r.project_id == project_id
                && r.account == account
                && !matches!(
                    r.status,
                    crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                ))
        });
    }
    let _ = app.emit(
        "agent://sub-agents-changed",
        serde_json::json!({ "project_id": project_id }),
    );
    Ok(())
}

#[tauri::command]
pub async fn delete_sub_agent(
    project_id: String,
    sub_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let acc = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .sub_agents_of(&project_id)
            .into_iter()
            .find(|s| s.id == sub_id)
            .map(|s| s.account)
            .unwrap_or_default()
    };
    // 先取消该子 agent 的运行实例。
    {
        let inner = state.inner.lock().await;
        if let Some(r) = inner.sub_runs.get(&sub_id) {
            if matches!(
                r.status,
                crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
            ) {
                r.cancel.cancel();
            }
        }
    }
    let sids = {
        let mut inner = state.inner.lock().await;
        let sids = inner.projects.remove_sub_agent(&project_id, &sub_id);
        let _ = inner.projects.save();
        sids
    };
    if !acc.is_empty() {
        if let Ok(model) = state.ensure_account_model(&acc).await {
            for sid in &sids {
                let _ = model.delete_session(sid).await;
            }
        }
    } else if let Ok(model) = state.ensure_run_ctx(&project_id).await {
        for sid in &sids {
            let _ = model.delete_session(sid).await;
        }
    }
    let _ = app.emit("agent://sub-agents-changed", serde_json::json!({ "project_id": project_id }));
    Ok(())
}

#[tauri::command]
pub async fn delete_sub_agent_conv(
    project_id: String,
    sub_id: String,
    session_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let acc = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .sub_agents_of(&project_id)
            .into_iter()
            .find(|s| s.id == sub_id)
            .map(|s| s.account)
            .unwrap_or_default()
    };
    let removed = {
        let mut inner = state.inner.lock().await;
        let r = inner.projects.remove_sub_conv(&project_id, &sub_id, &session_id);
        let _ = inner.projects.save();
        r
    };
    if let Some(sid) = removed {
        if !acc.is_empty() {
            if let Ok(model) = state.ensure_account_model(&acc).await {
                let _ = model.delete_session(&sid).await;
            }
        } else if let Ok(model) = state.ensure_run_ctx(&project_id).await {
            let _ = model.delete_session(&sid).await;
        }
    }
    let _ = app.emit("agent://sub-agents-changed", serde_json::json!({ "project_id": project_id }));
    Ok(())
}

/// 停止子 agent 的当前运行实例（保留其定义与会话，可稍后重试/继续）。
#[tauri::command]
pub async fn stop_sub_agent(
    project_id: String,
    sub_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let cancelled = {
        let inner = state.inner.lock().await;
        match inner.sub_runs.get(&sub_id) {
            Some(r) if matches!(
                r.status,
                crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
            ) => {
                r.cancel.cancel();
                true
            }
            _ => false,
        }
    };
    if cancelled {
        let _ = app.emit("agent://sub-done", serde_json::json!({
            "project_id": project_id,
            "sub_id": sub_id,
            "ok": false,
            "reason": "cancelled",
        }));
    }
    Ok(())
}

fn sub_agent_of(
    inner: &crate::state::Inner,
    project_id: &str,
    sub_id: &str,
) -> Result<crate::projects::SubAgent, String> {
    inner
        .projects
        .sub_agents_of(project_id)
        .into_iter()
        .find(|s| s.id == sub_id)
        .ok_or_else(|| "子 agent 不存在".to_string())
}

async fn dispatch_sub_agent(
    app: AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    name: String,
    task: String,
    agent_type: String,
) -> Result<(), String> {
    {
        let inner = state.inner.lock().await;
        let limit = inner.projects.max_sub_of(&project_id) as usize;
        let running = inner
            .sub_runs
            .values()
            .filter(|r| {
                r.project_id == project_id
                    && matches!(
                        r.status,
                        crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                    )
            })
            .count();
        if running >= limit {
            return Err(format!("当前已有 {limit} 个子 agent 在运行（并发上限）"));
        }
    }
    let spec = crate::subagent::SubAgentSpec {
        project_id,
        name,
        task,
        agent_type,
    };
    tauri::async_runtime::spawn(async move {
        let _ = crate::subagent::run_subagent(app, spec).await;
    });
    Ok(())
}

/// 重试：用原任务文本、复用现有会话重新派发。
#[tauri::command]
pub async fn retry_sub_agent(
    project_id: String,
    sub_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (name, agent_type, last_task) = {
        let inner = state.inner.lock().await;
        let sa = sub_agent_of(&inner, &project_id, &sub_id)?;
        let last_task = inner
            .sub_runs
            .get(&sub_id)
            .map(|r| r.last_task.clone())
            .filter(|t| !t.trim().is_empty())
            .ok_or("该子 agent 无可重试的任务记录")?;
        (
            sa.name,
            inner
                .sub_runs
                .get(&sub_id)
                .map(|r| r.agent_type.clone())
                .unwrap_or_else(|| "general".to_string()),
            last_task,
        )
    };
    dispatch_sub_agent(app, state, project_id, name, last_task, agent_type).await
}

/// 继续：在现有会话上用新任务续跑。
#[tauri::command]
pub async fn continue_sub_agent(
    project_id: String,
    sub_id: String,
    task: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if task.trim().is_empty() {
        return Err("继续任务的内容不能为空".into());
    }
    let (name, agent_type) = {
        let inner = state.inner.lock().await;
        let sa = sub_agent_of(&inner, &project_id, &sub_id)?;
        (
            sa.name,
            inner
                .sub_runs
                .get(&sub_id)
                .map(|r| r.agent_type.clone())
                .unwrap_or_else(|| "general".to_string()),
        )
    };
    dispatch_sub_agent(app, state, project_id, name, task, agent_type).await
}

/// 新会话重试：清空当前 active 会话后，用原任务重新派发。
#[tauri::command]
pub async fn retry_sub_agent_new_conv(
    project_id: String,
    sub_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (name, agent_type, last_task) = {
        let mut inner = state.inner.lock().await;
        let sa = sub_agent_of(&inner, &project_id, &sub_id)?;
        let last_task = inner
            .sub_runs
            .get(&sub_id)
            .map(|r| r.last_task.clone())
            .filter(|t| !t.trim().is_empty())
            .ok_or("该子 agent 无可重试的任务记录")?;
        // 清空 active 标记，使 resolve_session 新建会话。
        if let Some(p) = inner.projects.project_mut(&project_id) {
            if let Some(s) = p.sub_agents.iter_mut().find(|s| s.id == sub_id) {
                for c in s.conversations.iter_mut() {
                    c.is_active = false;
                }
            }
        }
        let _ = inner.projects.save();
        (
            sa.name,
            inner
                .sub_runs
                .get(&sub_id)
                .map(|r| r.agent_type.clone())
                .unwrap_or_else(|| "general".to_string()),
            last_task,
        )
    };
    dispatch_sub_agent(app, state, project_id, name, last_task, agent_type).await
}

#[tauri::command]
pub async fn list_conversations(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ConversationView>, String> {
    let inner = state.inner.lock().await;
    let p = inner.projects.project(&project_id).ok_or("项目不存在")?;
    let active = inner.projects.active_conversation_of(&project_id);
    Ok(p.conversations
        .iter()
        .map(|c| conv_view(c, &active))
        .collect())
}

#[tauri::command]
pub async fn new_conversation(
    project_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ConversationView, String> {
    let model = state.ensure_run_ctx(&project_id).await?;
    let _guard = account_guard(&state, &project_id).await;
    let sid = model.new_session().await.map_err(|e| e.to_string())?;

    let mut inner = state.inner.lock().await;
    let title = inner.projects.next_conversation_title(&project_id);
    let conv = inner.projects.add_conversation(&project_id, &sid, &title)?;
    save_projects(&inner)?;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session = Default::default();
        ctx.session.session_id = Some(sid.clone());
    }
    let active = inner.projects.active_conversation.clone();
    let view = conv_view(&conv, &active);
    let _ = app.emit("agent://projects-changed", ());
    Ok(view)
}

#[tauri::command]
pub async fn switch_conversation(
    project_id: String,
    conversation_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    inner.projects.set_active(&project_id, &conversation_id)?;
    save_projects(&inner)?;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session.session_id = Some(conversation_id);
        ctx.session.parent_message_id = None;
        ctx.session.prompt_sent = false;
    }
    let _ = app.emit("agent://projects-changed", ());
    Ok(())
}

#[tauri::command]
pub async fn delete_conversation(
    project_id: String,
    conversation_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let model = state.ensure_run_ctx(&project_id).await.ok();
    let _guard = account_guard(&state, &project_id).await;
    if let Some(model) = model {
        let _ = model.delete_session(&conversation_id).await;
    }
    let mut inner = state.inner.lock().await;
    inner
        .projects
        .delete_conversation(&project_id, &conversation_id)?;
    save_projects(&inner)?;
    let fallback_active = inner.projects.active_conversation_of(&project_id);
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        if ctx.session.session_id.as_deref() == Some(conversation_id.as_str()) {
            ctx.session.session_id = fallback_active;
            ctx.session.parent_message_id = None;
            ctx.session.prompt_sent = false;
        }
    }
    let _ = crate::projects::delete_conversation_file(&conversation_id);
    let _ = app.emit("agent://projects-changed", ());
    Ok(())
}

#[derive(serde::Serialize)]
pub struct StoredMessageView {
    pub role: String,
    pub text: String,
    pub ts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<crate::projects::ToolRecord>,
}

#[tauri::command]
pub async fn load_conversation_messages(
    conversation_id: String,
) -> Result<Vec<StoredMessageView>, String> {
    let msgs = crate::projects::load_messages(&conversation_id)?;
    Ok(msgs
        .into_iter()
        .map(|m| StoredMessageView {
            role: m.role,
            text: m.text,
            ts: m.ts,
            tool: m.tool,
        })
        .collect())
}

#[tauri::command]
pub async fn load_project_messages(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<StoredMessageView>, String> {
    let ids: Vec<String> = {
        let inner = state.inner.lock().await;
        let p = inner.projects.project(&project_id).ok_or("项目不存在")?;
        p.conversations.iter().map(|c| c.id.clone()).collect()
    };
    let mut all = Vec::new();
    for cid in ids {
        if let Ok(msgs) = crate::projects::load_messages(&cid) {
            all.extend(msgs);
        }
    }
    all.sort_by_key(|m| m.ts);
    Ok(all
        .into_iter()
        .map(|m| StoredMessageView {
            role: m.role,
            text: m.text,
            ts: m.ts,
            tool: m.tool,
        })
        .collect())
}

#[tauri::command]
pub async fn send_message(project_id: String, text: String, app: AppHandle) -> Result<(), String> {
    start_turn(app, project_id, text).await
}

pub async fn start_turn(
    app: AppHandle,
    project_id: String,
    text: String,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    {
        let inner = state.inner.lock().await;
        inner
            .projects
            .project(&project_id)
            .ok_or("请先选择一个项目")?;
    }
    let model = state.ensure_run_ctx(&project_id).await?;
    let (cancel, my_gen) = {
        let mut inner = state.inner.lock().await;
        let ctx = inner.running.get_mut(&project_id).ok_or("运行上下文丢失")?;
        if ctx.turn_active {
            return Err("该项目正在处理中".into());
        }
        ctx.busy = true;
        ctx.turn_active = true;
        ctx.turn_gen = ctx.turn_gen.wrapping_add(1);
        ctx.cancel_requested = false;
        let c = CancellationToken::new();
        ctx.cancel = Some(c.clone());
        (c, ctx.turn_gen)
    };
    let app2 = app.clone();
    let pid = project_id.clone();
    tauri::async_runtime::spawn(async move {
        let state = app2.state::<AppState>();
        let model: std::sync::Arc<dyn Model> = model;
        let fut: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> =
            Box::pin(runner::run_turn(
                app2.clone(),
                state.inner(),
                pid.clone(),
                model,
                text,
                cancel,
            ));
        let result = fut.await;
        // 只有当代主 turn 才有权收尾：若已被新 turn 取代（generation 不匹配），
        // 不得清理新 turn 的状态、也不得消费其 pending。
        let (pending, still_busy, is_current) = {
            let mut inner = state.inner.lock().await;
            let has_running_sub = inner.sub_runs.values().any(|r| {
                r.project_id == pid
                    && matches!(
                        r.status,
                        crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                    )
            });
            let mut is_current = false;
            let mut was_cancelled = false;
            if let Some(ctx) = inner.running.get_mut(&pid) {
                if ctx.turn_gen == my_gen {
                    is_current = true;
                    was_cancelled = ctx.cancel_requested;
                    ctx.cancel = None;
                    ctx.turn_active = false;
                    ctx.busy = has_running_sub;
                    ctx.cancel_requested = false;
                }
            }
            // 被用户取消的 turn：丢弃本轮 pending，不再自动唤起审阅 turn。
            let pending = if is_current && !was_cancelled {
                inner.pending_sub_results.remove(&pid).unwrap_or_default()
            } else {
                if is_current {
                    inner.pending_sub_results.remove(&pid);
                }
                Vec::new()
            };
            (pending, has_running_sub, is_current)
        };
        if !is_current {
            // 已被新 turn 接管：本次只结束，不再改状态、不再注入。
            return;
        }
        let _ = app2.emit(
            "agent://project-busy",
            serde_json::json!({ "id": pid, "busy": still_busy }),
        );
        if !pending.is_empty() {
            let mut block = String::new();
            for r in pending {
                block.push_str(&crate::subagent::format_sub_result(&r.name, r.ok, &r.text));
            }
            block.push_str(&format!("\n{}\n", runner::SUB_REVIEW_PROMPT));
            let f = {
                let inner = state.inner.lock().await;
                inner.start_turn.clone()
            };
            if let Some(f) = f {
                let _ = f(app2.clone(), pid.clone(), block).await;
            }
        }
        match result {
            Ok(()) => {
                let _ = app2.emit("agent://done", serde_json::json!({ "project_id": pid }));
            }
            Err(e) => {
                let account = {
                    let inner = state.inner.lock().await;
                    inner.projects.project(&pid).map(|p| p.account.clone()).unwrap_or_default()
                };
                let _ = app2.emit(
                    "agent://error",
                    error_payload(&pid, &e, Some(&account), None),
                );
            }
        }
        let inner_arc = state.inner.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(
                crate::state::BRIDGE_IDLE_SECS + 5,
            ))
            .await;
            let _ = crate::state::AppState::close_idle(
                &inner_arc,
                std::time::Duration::from_secs(crate::state::BRIDGE_IDLE_SECS),
            )
            .await;
        });
    });
    Ok(())
}

#[tauri::command]
pub async fn interrupt(project_id: String, state: State<'_, AppState>, _app: AppHandle) -> Result<(), String> {
    let (cancel, model) = {
        let inner = state.inner.lock().await;
        match inner.running.get(&project_id) {
            Some(ctx) => (ctx.cancel.clone(), Some(ctx.model.clone())),
            None => (None, None),
        }
    };
    if let Some(c) = cancel {
        c.cancel();
    }
    if let Some(m) = model {
        m.stop().await;
    }
    {
        let inner = state.inner.lock().await;
        for r in inner.sub_runs.values() {
            if r.project_id == project_id
                && matches!(
                    r.status,
                    crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting
                )
            {
                r.cancel.cancel();
            }
        }
    }
    // 标记用户已请求取消：收尾时据此丢弃 pending、不再自动唤起审阅 turn。
    // 仍不在此处修改 turn_active / cancel，由 run_turn 收尾回调（校验 generation）统一清理。
    {
        let mut inner = state.inner.lock().await;
        if let Some(ctx) = inner.running.get_mut(&project_id) {
            if ctx.turn_active {
                ctx.cancel_requested = true;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn approve_command(
    id: u64,
    allow: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    if let Some((_, tx)) = inner.approvals.remove(&id) {
        let _ = tx.send(allow);
    }
    Ok(())
}

#[tauri::command]
pub async fn busy_projects(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let inner = state.inner.lock().await;
    Ok(inner
        .running
        .iter()
        .filter(|(_, c)| c.busy)
        .map(|(k, _)| k.clone())
        .collect())
}

#[tauri::command]
pub fn window_minimize(window: tauri::WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_close(window: tauri::WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_start_drag(window: tauri::WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_mode(project_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let inner = state.inner.lock().await;
    if let Some(ctx) = inner.running.get(&project_id) {
        return Ok(ctx.session.free_mode);
    }
    Ok(inner.projects.free_mode_of(&project_id))
}

#[tauri::command]
pub async fn set_mode(
    project_id: String,
    free: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session.free_mode = free;
    }
    inner.projects.set_free_mode_of(&project_id, free);
    save_projects(&inner)?;
    Ok(())
}

#[tauri::command]
pub async fn get_thinking(project_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let inner = state.inner.lock().await;
    if let Some(ctx) = inner.running.get(&project_id) {
        return Ok(ctx.session.thinking);
    }
    Ok(inner.projects.thinking_of(&project_id))
}

#[tauri::command]
pub async fn set_thinking(
    project_id: String,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let maybe = {
        let mut inner = state.inner.lock().await;
        inner.projects.set_thinking_of(&project_id, on);
        save_projects(&inner)?;
        match inner.running.get_mut(&project_id) {
            Some(c) => {
                c.session.thinking = on;
                Some((c.model.clone(), c.session.thinking, c.session.search))
            }
            None => None,
        }
    };
    if let Some((model, thinking, search)) = maybe {
        model
            .set_toggles(thinking, search)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_search(project_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let inner = state.inner.lock().await;
    if let Some(ctx) = inner.running.get(&project_id) {
        return Ok(ctx.session.search);
    }
    Ok(inner.projects.search_of(&project_id))
}

#[tauri::command]
pub async fn set_search(
    project_id: String,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let maybe = {
        let mut inner = state.inner.lock().await;
        inner.projects.set_search_of(&project_id, on);
        save_projects(&inner)?;
        match inner.running.get_mut(&project_id) {
            Some(c) => {
                c.session.search = on;
                Some((c.model.clone(), c.session.thinking, c.session.search))
            }
            None => None,
        }
    };
    if let Some((model, thinking, search)) = maybe {
        model
            .set_toggles(thinking, search)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn upload_files(
    project_id: String,
    paths: Vec<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<PendingFile>, String> {
    if paths.len() > 50 {
        return Err(format!("一次最多上传 50 个文件，当前 {} 个", paths.len()));
    }
    {
        let inner = state.inner.lock().await;
        if inner
            .running
            .get(&project_id)
            .map(|c| c.busy)
            .unwrap_or(false)
        {
            return Err("正在生成回复，请稍后再上传".into());
        }
    }
    let mut out = Vec::new();
    for p in &paths {
        let path = std::path::Path::new(p);
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > 100 * 1024 * 1024 {
            return Err(format!(
                "文件 {} 超过 100MB 上限",
                path.file_name().and_then(|s| s.to_str()).unwrap_or(p)
            ));
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        let is_image = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"].contains(&ext.as_str());
        out.push(PendingFile {
            id: String::new(),
            name,
            size: size as i64,
            is_image,
            path: p.clone(),
        });
    }

    let (project_id, session_id) = {
        let inner = state.inner.lock().await;
        let pid = inner
            .projects
            .project(&project_id)
            .ok_or_else(|| "未选择项目".to_string())?
            .id
            .clone();
        let sid = inner
            .running
            .get(&pid)
            .and_then(|c| c.session.session_id.clone())
            .or_else(|| inner.projects.active_conversation_of(&pid));
        (pid, sid)
    };

    let model = state.ensure_run_ctx(&project_id).await?;
    let _guard = account_guard(&state, &project_id).await;
    let upload_paths: Vec<String> = out.iter().map(|f| f.path.clone()).collect();
    model
        .upload_files(session_id.as_deref(), &upload_paths)
        .await
        .map_err(|e| format!("上传失败：{e}"))?;

    {
        let mut inner = state.inner.lock().await;
        if let Some(ctx) = inner.running.get_mut(&project_id) {
            ctx.session.pending_files.extend(out.iter().cloned());
        }
    }
    let _ = app.emit("agent://files-changed", &project_id);
    Ok(out)
}

#[tauri::command]
pub async fn set_pending_files(
    project_id: String,
    files: Vec<PendingFile>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session.pending_files = files;
    }
    Ok(())
}

#[tauri::command]
pub async fn clear_pending_files(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session.pending_files.clear();
    }
    Ok(())
}
#[tauri::command]
pub async fn get_workdir(project_id: String, state: State<'_, AppState>) -> Result<String, String> {
    let inner = state.inner.lock().await;
    if let Some(spec) = inner.projects.remote_of(&project_id) {
        return Ok(spec.root);
    }
    Ok(inner.projects.workdir_of(&project_id).unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }))
}

fn strip_extended_prefix(p: std::path::PathBuf) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return std::path::PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return std::path::PathBuf::from(rest.to_string());
        }
    }
    p
}

#[tauri::command]
pub async fn set_workdir(
    app: AppHandle,
    project_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_dir() {
        return Err(format!("不是有效目录：{path}"));
    }
    let canon = p.canonicalize().map_err(|e| e.to_string())?;
    let canon = strip_extended_prefix(canon);
    let dir = canon.to_string_lossy().to_string();
    let mut inner = state.inner.lock().await;
    inner.projects.set_workdir_of(&project_id, &dir);
    save_projects(&inner)?;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session.announced_workdir = None;
    }
    drop(inner);
    // 广播给主窗口，使顶栏工作目录输入框实时跟随（IDE 里“上一层”也走这里）。
    let _ = app.emit(
        "agent://workdir-changed",
        serde_json::json!({ "project_id": project_id, "path": dir }),
    );
    Ok(dir)
}

#[tauri::command]
pub async fn get_restrict(project_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.restrict_of(&project_id))
}

#[tauri::command]
pub async fn set_restrict(
    project_id: String,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    inner.projects.set_restrict_of(&project_id, on);
    save_projects(&inner)?;
    Ok(())
}

#[tauri::command]
pub async fn get_compact_threshold(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.compact_of(&project_id))
}

#[tauri::command]
pub async fn set_compact_threshold(
    project_id: String,
    value: i64,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    if value <= 0 {
        return Err("压缩阈值必须大于 0".into());
    }
    let mut inner = state.inner.lock().await;
    inner.projects.set_compact_of(&project_id, value);
    save_projects(&inner)?;
    Ok(value)
}

#[tauri::command]
pub async fn get_max_sub(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<u32, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.max_sub_of(&project_id))
}

#[tauri::command]
pub async fn set_max_sub(
    project_id: String,
    value: u32,
    state: State<'_, AppState>,
) -> Result<u32, String> {
    if value == 0 {
        return Err("子 agent 并发上限必须大于 0".into());
    }
    let mut inner = state.inner.lock().await;
    inner.projects.set_max_sub_of(&project_id, value);
    save_projects(&inner)?;
    Ok(value)
}

#[tauri::command]
pub async fn get_warmup(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let inner = state.inner.lock().await;
    Ok(inner.projects.warmup_of(&project_id))
}

#[tauri::command]
pub async fn set_warmup(
    project_id: String,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    inner.projects.set_warmup_of(&project_id, on);
    save_projects(&inner)?;
    Ok(())
}

#[tauri::command]
pub async fn remove_file(
    project_id: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let model = state.ensure_run_ctx(&project_id).await.ok();
    let _guard = account_guard(&state, &project_id).await;
    if let Some(model) = model {
        let session_id = {
            let inner = state.inner.lock().await;
            inner
                .running
                .get(&project_id)
                .and_then(|c| c.session.session_id.clone())
                .or_else(|| inner.projects.active_conversation_of(&project_id))
        };
        let _ = model.remove_file(session_id.as_deref(), &name).await;
    }
    let mut inner = state.inner.lock().await;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session.pending_files.retain(|f| f.name != name);
    }
    Ok(())
}

#[tauri::command]
pub async fn list_web_files(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let model = state.ensure_run_ctx(&project_id).await.ok();
    let _guard = account_guard(&state, &project_id).await;
    match model {
        Some(model) => {
            let session_id = {
                let inner = state.inner.lock().await;
                inner
                    .running
                    .get(&project_id)
                    .and_then(|c| c.session.session_id.clone())
                    .or_else(|| inner.projects.active_conversation_of(&project_id))
            };
            model
                .list_files(session_id.as_deref())
                .await
                .map_err(|e| e.to_string())
        }
        None => Ok(Vec::new()),
    }
}
#[tauri::command]
pub async fn login_browser(name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("账号名不能为空".into());
    }
    deepseek_agent::accounts::AccountManager::validate_name(&name).map_err(|e| e.to_string())?;
    let mut mgr =
        deepseek_agent::accounts::AccountManager::load_default().map_err(|e| e.to_string())?;
    if !mgr.exists(&name) {
        mgr.add(&name).map_err(|e| e.to_string())?;
    }
    mgr.touch(&name).map_err(|e| e.to_string())?;
    let home = dirs::home_dir().ok_or("无 HOME")?;
    let profile = home
        .join(".deepseek-agent")
        .join("accounts")
        .join(&name)
        .join("browser");
    std::fs::create_dir_all(&profile).map_err(|e| e.to_string())?;
    let state_file = home
        .join(".ligong")
        .join("accounts")
        .join(&name)
        .join("state.json");
    if let Some(dir) = state_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let script = deepseek_agent::bridge::login_script_path();
    let node = deepseek_agent::paths::node_bin();
    let mut cmd = tokio::process::Command::new(node);
    cmd.arg(script)
        .arg(&name)
        .env("LIGONG_PROFILE_DIR", &profile)
        .env("LIGONG_STATE_FILE", &state_file)
        .env("LIGONG_PLAYWRIGHT", deepseek_agent::paths::playwright_dir());
    if let Some(ch) = deepseek_agent::paths::chromium_exec() {
        cmd.env("LIGONG_CHROMIUM", ch);
    }
    let status = cmd
        .status()
        .await
        .map_err(|e| format!("启动登录浏览器失败: {e}"))?;
    if !status.success() {
        return Err("登录流程未完成".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_account(name: String, state: State<'_, AppState>) -> Result<(), String> {
    {
        let inner = state.inner.lock().await;
        if inner.running.values().any(|c| c.account == name && c.busy) {
            return Err("该账号有项目正在运行".into());
        }
    }
    let removed_bridge = {
        let mut inner = state.inner.lock().await;
        inner.bridges.remove(&name)
    };
    if let Some(bm) = removed_bridge {
        bm.stop().await;
    }
    let mut mgr =
        deepseek_agent::accounts::AccountManager::load_default().map_err(|e| e.to_string())?;
    mgr.remove(&name).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn clear_local_history(
    project_id: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let plan: Vec<(String, String)> = {
        let inner = state.inner.lock().await;
        match &project_id {
            Some(pid) => inner
                .projects
                .project(pid)
                .map(|p| {
                    p.conversations
                        .iter()
                        .map(|c| (pid.clone(), c.id.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            None => inner
                .projects
                .projects
                .iter()
                .flat_map(|p| p.conversations.iter().map(|c| (p.id.clone(), c.id.clone())))
                .collect(),
        }
    };

    let mut removed = 0usize;
    for (_pid, cid) in &plan {
        if crate::projects::delete_conversation_file(cid).is_ok() {
            removed += 1;
        }
    }

    {
        let mut inner = state.inner.lock().await;
        for (pid, cid) in &plan {
            inner.projects.reset_conversation_history(pid, cid);
        }
        match &project_id {
            Some(pid) => {
                if let Some(ctx) = inner.running.get_mut(pid) {
                    ctx.session.parent_message_id = None;
                }
            }
            None => {
                for ctx in inner.running.values_mut() {
                    ctx.session.parent_message_id = None;
                }
            }
        }
        let _ = save_projects(&inner);
    }

    let _ = app.emit("agent://projects-changed", ());
    Ok(removed)
}

#[tauri::command]
pub async fn list_account_sessions(
    name: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let bm = state.ensure_account_model(&name).await?;
    let sessions = bm.bridge_list_sessions().await.map_err(|e| e.to_string())?;
    Ok(sessions
        .into_iter()
        .map(|(id, title)| serde_json::json!({ "id": id, "title": title }))
        .collect())
}

#[tauri::command]
pub async fn delete_account_sessions(
    name: String,
    ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let bm = state.ensure_account_model(&name).await?;
    for id in &ids {
        let _ = bm.bridge_delete_session(id).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn detach_conversation(
    project_id: String,
    conversation_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    inner
        .projects
        .detach_conversation(&project_id, &conversation_id)?;
    save_projects(&inner)?;
    let fallback_active = inner.projects.active_conversation_of(&project_id);
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        if ctx.session.session_id.as_deref() == Some(conversation_id.as_str()) {
            ctx.session.session_id = fallback_active;
            ctx.session.parent_message_id = None;
            ctx.session.prompt_sent = false;
        }
    }
    let _ = app.emit("agent://projects-changed", ());
    Ok(())
}

#[tauri::command]
pub async fn list_project_account_sessions(
    project_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let account = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .project(&project_id)
            .map(|p| p.account.clone())
            .unwrap_or_default()
    };
    if account.is_empty() {
        return Err("项目未绑定账号".into());
    }
    let bm = state.ensure_account_model(&account).await?;
    let sessions = bm.bridge_list_sessions().await.map_err(|e| e.to_string())?;
    Ok(sessions
        .into_iter()
        .map(|(id, title)| serde_json::json!({ "id": id, "title": title }))
        .collect())
}

#[tauri::command]
pub async fn attach_account_session(
    project_id: String,
    session_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ConversationView, String> {
    let mut inner = state.inner.lock().await;

    let owner: Option<(String, String)> = inner
        .projects
        .projects
        .iter()
        .find(|p| p.conversations.iter().any(|c| c.id == session_id))
        .map(|p| (p.id.clone(), p.name.clone()));
    if let Some((owner_pid, owner_name)) = owner {
        if owner_pid == project_id {
            return Err("该会话已在当前项目中。".into());
        }
        return Err(format!(
            "该会话已属于项目「{}」，请先在原项目中移出它再加入。",
            owner_name
        ));
    }

    let title = inner.projects.next_conversation_title(&project_id);
    let conv = inner
        .projects
        .add_conversation(&project_id, &session_id, &title)?;
    save_projects(&inner)?;
    if let Some(ctx) = inner.running.get_mut(&project_id) {
        ctx.session = Default::default();
        ctx.session.session_id = Some(session_id.clone());
    }
    let active = inner.projects.active_conversation.clone();
    let view = conv_view(&conv, &active);
    let _ = app.emit("agent://projects-changed", ());
    Ok(view)
}
#[derive(serde::Serialize)]
pub struct QqStatus {
    pub running: bool,
    pub ws_url: String,
    pub self_id: i64,
}

/// 查询 QQ 接入状态。
#[tauri::command]
pub async fn qq_status(state: State<'_, AppState>) -> Result<QqStatus, String> {
    let inner = state.inner.lock().await;
    match &inner.qq {
        Some(h) => Ok(QqStatus {
            running: h.running,
            ws_url: h.ws_url.clone(),
            self_id: h.self_id,
        }),
        None => {
            let cfg = crate::qq::QqConfig::load();
            Ok(QqStatus {
                running: false,
                ws_url: cfg.ws_url,
                self_id: 0,
            })
        }
    }
}

/// 启停 QQ 接入。启用时自动确保 NapCat 就绪并拉起终端。
#[tauri::command]
pub async fn qq_toggle(app: AppHandle, enable: bool) -> Result<bool, String> {
    let state = app.state::<AppState>();
    let cfg = crate::qq::QqConfig::load();
    if enable {
        {
            let mut inner = state.inner.lock().await;
            if inner.qq.as_ref().map(|h| h.running).unwrap_or(false) {
                return Ok(true);
            }
            let cancel = CancellationToken::new();
            inner.qq = Some(crate::qq::QqHandle {
                running: true,
                ws_url: cfg.ws_url.clone(),
                http_url: cfg.http_url.clone(),
                self_id: 0,
                cancel: Some(cancel.clone()),
                sender: None,
            });
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                crate::qq::ws::run_loop(app2, cancel).await;
            });
        }
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = crate::qq::napcat::ensure_and_launch().await {
                eprintln!("[qq] NapCat 启动失败: {e}");
                let _ = app2.emit("qq://error", serde_json::json!({ "message": e }));
            }
        });
        Ok(true)
    } else {
        let mut inner = state.inner.lock().await;
        if let Some(h) = inner.qq.take() {
            if let Some(c) = h.cancel {
                c.cancel();
            }
        }
        Ok(false)
    }
}