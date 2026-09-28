//! NapCat 正向 WebSocket 连接与消息处理。

use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::qq::{client, config::QqConfig, router};
use crate::state::AppState;
use deepseek_agent::agent::model::Model;

#[derive(Clone)]
pub struct QqHandle {
    pub running: bool,
    pub ws_url: String,
    pub http_url: String,
    pub self_id: i64,
    pub cancel: Option<CancellationToken>,
    pub sender: Option<client::WsSender>,
}

fn read_self_id(ev: &Value) -> Option<i64> {
    ev.get("self_id").and_then(|v| v.as_i64())
}

pub async fn run_loop(app: AppHandle, cancel: CancellationToken) {
    let cfg = QqConfig::load();
    let ws_url = cfg.ws_url.clone();
    let stream = loop {
        match connect_async(&ws_url).await {
            Ok((s, _)) => break s,
            Err(e) => {
                let _ = app.emit("qq://error", serde_json::json!({ "message": e.to_string() }));
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }
            }
        }
    };
    let _ = app.emit("qq://connected", ());
    let (write, mut read) = stream.split();
    let sender: client::WsSender = Arc::new(Mutex::new(write));
    let mut self_id = 0i64;
    let cfg = Arc::new(Mutex::new(QqConfig::load()));

    {
        let state = app.state::<AppState>();
        let mut inner = state.inner.lock().await;
        if let Some(h) = inner.qq.as_mut() {
            h.sender = Some(sender.clone());
        }
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(_)) => { break; }
                    None => { break; }
                };
                if let Message::Text(txt) = msg {
                    let ev: Value = match serde_json::from_str(&txt) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if self_id == 0 {
                        if let Some(id) = read_self_id(&ev) {
                            self_id = id;
                            let state = app.state::<AppState>();
                            let mut inner = state.inner.lock().await;
                            if let Some(h) = inner.qq.as_mut() {
                                h.self_id = id;
                            }
                        }
                    }
                    let Some(incoming) = router::parse_event(&ev, self_id) else { continue; };
                    let app2 = app.clone();
                    let cfg2 = cfg.clone();
                    let sender2 = sender.clone();
                    // 每条消息一个独立任务 token：连接级 token 只用于断开 WS，
                    // 不能被 /stop 取消，否则后续消息都会因 token 已取消而立刻中止。
                    let task_cancel = CancellationToken::new();
                    let cancel2 = cancel.clone();
                    let task_cancel2 = task_cancel.clone();
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = cancel2.cancelled() => {}
                            _ = handle_incoming(app2, cfg2, incoming, task_cancel2, sender2) => {}
                        }
                    });
                }
            }
        }
    }
    let _ = app.emit("qq://disconnected", ());
}

async fn handle_incoming(
    app: AppHandle,
    cfg: Arc<Mutex<QqConfig>>,
    incoming: router::Incoming,
    cancel: CancellationToken,
    sender: client::WsSender,
) {
    let reply_text = match handle_command(&app, &cfg, &incoming).await {
        Some(t) => t,
        None => {
            match run_agent_for(&app, &cfg, &incoming, cancel).await {
                Ok(t) => t,
                Err(e) => format!("处理失败：{e}"),
            }
        }
    };
    if reply_text.trim().is_empty() {
        return;
    }
    for chunk in client::split_message(&reply_text) {
        let _ = match incoming.group_id {
            Some(gid) => client::send_group(&sender, gid, &chunk).await,
            None => client::send_private(&sender, incoming.user_id, &chunk).await,
        };
    }
}

async fn handle_command(
    app: &AppHandle,
    cfg: &Arc<Mutex<QqConfig>>,
    inc: &router::Incoming,
) -> Option<String> {
    let text = inc.text.trim();
    if !text.starts_with(char::from(47)) {
        return None;
    }
    let mut parts = text.splitn(2, char::from(32));
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match cmd {
        "/help" => Some(help_text()),
        "/projects" => Some(list_projects(app).await),
        "/use" => Some(use_project(app, cfg, inc, arg).await),
        "/new" => Some(new_project(app, cfg, inc, arg).await),
        "/account" => Some(account_cmd(cfg, arg).await),
        "/stop" | "/cancel" => Some(stop_project(app, cfg, inc).await),
        _ => Some(format!("未知命令 {cmd}，输入 /help 查看帮助")),
    }
}

fn help_text() -> String {
    let mut s = String::new();
    s.push_str("可用命令：\n");
    s.push_str("/help 显示帮助\n");
    s.push_str("/projects 列出所有项目\n");
    s.push_str("/use <序号或名称> 绑定到项目（任务用该项目所属账号）\n");
    s.push_str("/new <名称> 新建项目并绑定\n");
    s.push_str("/account 查看默认账号 / /account <名字> 设置新建项目默认账号\n");
    s.push_str("/stop 停止当前绑定项目正在执行的任务\n");
    s.push_str("直接发消息（群聊需@我）会转给已绑定项目。");
    s
}

async fn stop_project(app: &AppHandle, cfg: &Arc<Mutex<QqConfig>>, inc: &router::Incoming) -> String {
    let pid = {
        let c = cfg.lock().await;
        c.project_of(&inc.session_key).cloned()
    };
    let Some(pid) = pid else {
        return "当前会话未绑定项目，无需停止。".to_string();
    };
    let state = app.state::<AppState>();
    let (cancel, model, name) = {
        let inner = state.inner.lock().await;
        let name = inner
            .projects
            .project(&pid)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| pid.clone());
        match inner.running.get(&pid) {
            Some(ctx) => (ctx.cancel.clone(), Some(ctx.model.clone()), name),
            None => (None, None, name),
        }
    };
    match cancel {
        Some(c) => {
            c.cancel();
            if let Some(m) = model {
                m.stop().await;
            }
            // 等 run_turn 退出并清理 busy，避免用户紧接着发消息撞上“项目正忙”。
            for _ in 0..50 {
                let busy = {
                    let inner = state.inner.lock().await;
                    inner.running.get(&pid).map(|c| c.busy).unwrap_or(false)
                };
                if !busy {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            format!("已停止项目「{name}」的任务。")
        }
        None => format!("项目「{name}」当前没有正在执行的任务。"),
    }
}

async fn account_cmd(cfg: &Arc<Mutex<QqConfig>>, arg: &str) -> String {
    let accounts = AppState::list_accounts();
    if accounts.is_empty() {
        return "暂无账号，请先在 GUI 里登录。".to_string();
    }
    let cur = cfg.lock().await.account.clone();
    if arg.is_empty() {
        let mut s = String::from("可用账号：\n");
        for a in &accounts {
            let mark = if *a == cur { " (当前默认)" } else { "" };
            s.push_str(&format!("- {a}{mark}\n"));
        }
        s.push_str("用 /account <名字> 设置新建项目的默认账号。\n");
        s.push_str("注意：已绑定项目按项目自身账号执行，切项目即切账号。");
        return s;
    }
    if !accounts.iter().any(|a| a == arg) {
        return format!("账号不存在：{arg}");
    }
    let mut c = cfg.lock().await;
    c.set_account(arg);
    let _ = c.save();
    format!("新建项目的默认账号已设为：{arg}")
}

async fn list_projects(app: &AppHandle) -> String {
    let state = app.state::<AppState>();
    let inner = state.inner.lock().await;
    if inner.projects.projects.is_empty() {
        return "暂无项目，用 /new <名称> 新建。".to_string();
    }
    let mut s = String::from("项目列表：\n");
    for (i, p) in inner.projects.projects.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", i + 1, p.name));
    }
    s
}

async fn use_project(
    app: &AppHandle,
    cfg: &Arc<Mutex<QqConfig>>,
    inc: &router::Incoming,
    arg: &str,
) -> String {
    if arg.is_empty() {
        return "用法：/use <序号或名称>".to_string();
    }
    let state = app.state::<AppState>();
    let project_id = {
        let inner = state.inner.lock().await;
        if let Ok(n) = arg.parse::<usize>() {
            inner.projects.projects.get(n.wrapping_sub(1)).map(|p| (p.id.clone(), p.name.clone()))
        } else {
            inner.projects.projects.iter().find(|p| p.name == arg).map(|p| (p.id.clone(), p.name.clone()))
        }
    };
    let Some((pid, name)) = project_id else {
        return format!("未找到项目：{arg}");
    };
    let mut c = cfg.lock().await;
    c.bind(&inc.session_key, &pid);
    let _ = c.save();
    format!("已绑定到项目：{name}")
}

async fn new_project(
    app: &AppHandle,
    cfg: &Arc<Mutex<QqConfig>>,
    inc: &router::Incoming,
    arg: &str,
) -> String {
    if arg.is_empty() {
        return "用法：/new <名称>".to_string();
    }
    let state = app.state::<AppState>();
    let (pid, name) = {
        let account = cfg.lock().await.account.clone();
        let mut inner = state.inner.lock().await;
        let p = inner.projects.create_project(arg, &account);
        let _ = inner.projects.save();
        (p.id, p.name)
    };
    let mut c = cfg.lock().await;
    c.bind(&inc.session_key, &pid);
    let _ = c.save();
    let _ = app.emit("agent://projects-changed", ());
    format!("已新建并绑定项目：{name}")
}

async fn run_agent_for(
    app: &AppHandle,
    cfg: &Arc<Mutex<QqConfig>>,
    inc: &router::Incoming,
    cancel: CancellationToken,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    let project_id = ensure_bound_project(app, cfg, inc).await?;
    let account = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .project(&project_id)
            .map(|p| p.account.clone())
            .ok_or_else(|| "绑定项目不存在".to_string())?
    };

    {
        let inner = state.inner.lock().await;
        let acc_busy = inner
            .running
            .values()
            .any(|c| c.account == account && c.busy);
        if acc_busy {
            return Err(format!("账号 {account} 正忙，请稍后再试"));
        }
    }

    let model = state.ensure_run_ctx(&project_id).await?;

    let conv_id = {
        let mut inner = state.inner.lock().await;
        let ctx = inner.running.get_mut(&project_id).ok_or("运行上下文丢失")?;
        if ctx.busy {
            return Err("该项目正在处理中，请稍候".into());
        }
        ctx.busy = true;
        ctx.cancel = Some(cancel.clone());
        ctx.session.session_id.clone()
    };

    let app2 = app.clone();
    let pid = project_id.clone();
    let input = inc.text.clone();
    let c2 = cancel.clone();
    let result = tauri::async_runtime::spawn(async move {
        let state = app2.state::<AppState>();
        let model: Arc<dyn Model> = model;
        let r = crate::runner::run_turn(app2.clone(), state.inner(), pid.clone(), model, input, c2).await;
        {
            let mut inner = state.inner.lock().await;
            if let Some(ctx) = inner.running.get_mut(&pid) {
                ctx.busy = false;
                ctx.cancel = None;
            }
        }
        r
    })
    .await
    .map_err(|e| format!("任务执行失败: {e}"))?;

    result?;

    let reply = {
        let inner = state.inner.lock().await;
        let cid = inner
            .projects
            .active_conversation_of(&project_id)
            .or(conv_id);
        cid.and_then(|cid| crate::projects::load_messages(&cid).ok())
            .and_then(|msgs| {
                msgs.into_iter()
                    .rev()
                    .find(|m| m.role == "assistant" && !m.text.trim().is_empty())
                    .map(|m| m.text)
            })
    };
    Ok(reply.unwrap_or_else(|| "（无回复）".to_string()))
}

async fn ensure_bound_project(
    app: &AppHandle,
    cfg: &Arc<Mutex<QqConfig>>,
    inc: &router::Incoming,
) -> Result<String, String> {
    {
        let c = cfg.lock().await;
        if let Some(pid) = c.project_of(&inc.session_key) {
            let state = app.state::<AppState>();
            let exists = { state.inner.lock().await.projects.project(pid).is_some() };
            if exists {
                return Ok(pid.clone());
            }
        }
    }
    let name = auto_project_name(app, inc).await;
    let state = app.state::<AppState>();
    let (pid, _name) = {
        let account = cfg.lock().await.account.clone();
        let mut inner = state.inner.lock().await;
        let p = inner.projects.create_project(&name, &account);
        let _ = inner.projects.save();
        (p.id, p.name)
    };
    let mut c = cfg.lock().await;
    c.bind(&inc.session_key, &pid);
    let _ = c.save();
    let _ = app.emit("agent://projects-changed", ());
    Ok(pid)
}

async fn auto_project_name(_app: &AppHandle, inc: &router::Incoming) -> String {
    match inc.group_id {
        Some(gid) => format!("QQ群_{gid}"),
        None => format!("QQ_{}", inc.user_id),
    }
}