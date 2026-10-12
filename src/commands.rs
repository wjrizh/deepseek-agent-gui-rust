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
    let conv_ids: Vec<String> = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .project(&id)
            .map(|p| p.conversations.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default()
    };
    let total = conv_ids.len();
    let model = state.ensure_run_ctx(&id).await.ok();

    for (i, cid) in conv_ids.iter().enumerate() {
        let _ = app.emit(
            "agent://delete-progress",
            serde_json::json!({ "phase": "deleting", "current": i + 1, "total": total }),
        );
        if let Some(m) = &model {
            let _ = m.delete_session(cid).await;
        }
        {
            let mut inner = state.inner.lock().await;
            let _ = inner.projects.delete_conversation(&id, cid);
            let _ = save_projects(&inner);
        }
        if i + 1 < total {
            let ms = 3000 + {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64)
                    .unwrap_or(0);
                nanos % 2001
            };
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
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
    drop(inner);
    deepseek_agent::agent::browser::shutdown_key(&id).await;
    let _ = app.emit("agent://projects-changed", ());
    let _ = app.emit(
        "agent://delete-progress",
        serde_json::json!({ "phase": "done", "current": total, "total": total }),
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
    save_projects(&inner)?;
    drop(inner);
    let _ = app.emit("agent://projects-changed", ());
    Ok(())
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
    let state = app.state::<AppState>();
    {
        let inner = state.inner.lock().await;
        inner
            .projects
            .project(&project_id)
            .ok_or("请先选择一个项目")?;
    }
    let model = state.ensure_run_ctx(&project_id).await?;
    let cancel = {
        let mut inner = state.inner.lock().await;
        let ctx = inner.running.get_mut(&project_id).ok_or("运行上下文丢失")?;
        if ctx.busy {
            return Err("该项目正在处理中".into());
        }
        ctx.busy = true;
        let c = CancellationToken::new();
        ctx.cancel = Some(c.clone());
        c
    };
    let app2 = app.clone();
    let pid = project_id.clone();
    tauri::async_runtime::spawn(async move {
        let state = app2.state::<AppState>();
        let model: std::sync::Arc<dyn Model> = model;
        let result = runner::run_turn(
            app2.clone(),
            state.inner(),
            pid.clone(),
            model,
            text,
            cancel,
        )
        .await;
        {
            let mut inner = state.inner.lock().await;
            if let Some(ctx) = inner.running.get_mut(&pid) {
                ctx.busy = false;
                ctx.cancel = None;
            }
        }
        match result {
            Ok(()) => {
                let _ = app2.emit("agent://done", serde_json::json!({ "project_id": pid }));
            }
            Err(e) => {
                let _ = app2.emit(
                    "agent://error",
                    serde_json::json!({ "project_id": pid, "message": e }),
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
pub async fn interrupt(project_id: String, state: State<'_, AppState>) -> Result<(), String> {
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
    Ok(inner
        .running
        .get(&project_id)
        .map(|c| c.session.thinking)
        .unwrap_or(false))
}

#[tauri::command]
pub async fn set_thinking(
    project_id: String,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (model, thinking, search) = {
        let mut inner = state.inner.lock().await;
        let ctx = match inner.running.get_mut(&project_id) {
            Some(c) => c,
            None => return Ok(()),
        };
        ctx.session.thinking = on;
        (ctx.model.clone(), ctx.session.thinking, ctx.session.search)
    };
    model
        .set_toggles(thinking, search)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn get_search(project_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let inner = state.inner.lock().await;
    Ok(inner
        .running
        .get(&project_id)
        .map(|c| c.session.search)
        .unwrap_or(false))
}

#[tauri::command]
pub async fn set_search(
    project_id: String,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (model, thinking, search) = {
        let mut inner = state.inner.lock().await;
        let ctx = match inner.running.get_mut(&project_id) {
            Some(c) => c,
            None => return Ok(()),
        };
        ctx.session.search = on;
        (ctx.model.clone(), ctx.session.thinking, ctx.session.search)
    };
    model
        .set_toggles(thinking, search)
        .await
        .map_err(|e| e.to_string())?;
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