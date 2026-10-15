//! 子 agent：独立循环（复制自主 agent 循环骨架，仅供子 agent 使用，不改 run_turn）。
//! 对话内容不落本地；本地只在 projects 里存 SubAgent 身份 + 会话段引用。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::runner::{execute_tool, send_delay, AnswerStreamGate, EmitCtx, HANDOFF_INTRO, REPORT_PROMPT};
use crate::state::{SubRunCtx, SubRunStatus};
use deepseek_agent::agent::model::Model;
use deepseek_agent::agent::parser::{self, ParseOutcome};
use deepseek_agent::api::types::{CompletionReq, DeltaKind};
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

const SUB_GAP: Duration = Duration::from_secs(10);

pub struct LoopOutcome {
    pub text: String,
    pub ok: bool,
    pub reason: Option<String>,
}

pub struct SubLoopCtx {
    pub app: AppHandle,
    pub project_id: String,
    pub sub_id: String,
    pub sub_name: String,
    pub account: String,
    pub model: Arc<dyn Model>,
    pub session_id: String,
    pub cancel: CancellationToken,
    pub first_input: String,
    pub prompt_already_sent: bool,
    pub workdir: String,
    pub compact_threshold: i64,
}

fn sub_ec(ctx: &SubLoopCtx) -> EmitCtx {
    EmitCtx {
        app: ctx.app.clone(),
        project_id: ctx.project_id.clone(),
        sub_id: Some(ctx.sub_id.clone()),
        sub_name: Some(ctx.sub_name.clone()),
    }
}

fn build_prompt(ctx: &SubLoopCtx, input: &str) -> String {
    if ctx.prompt_already_sent {
        input.to_string()
    } else {
        format!(
            "{}\n\n{}\n\n{}\n\n{}",
            crate::prompt_gui::system_prompt_with_env(),
            crate::prompt_gui::SUBAGENT_PROMPT,
            crate::prompt_gui::SUBAGENT_TOOLS_SECTION,
            input
        )
    }
}

pub async fn agent_loop(ctx: SubLoopCtx) -> LoopOutcome {
    let ec = sub_ec(&ctx);
    let state = ctx.app.state::<crate::state::AppState>();
    let SubLoopCtx { project_id, sub_id, account, model, cancel, .. } = &ctx;
    let mut session_id = ctx.session_id.clone();
    let input = ctx.first_input.clone();
    let mut next_prompt = build_prompt(&ctx, &input);

    loop {
        {
            let n = model.message_count(&session_id).await.unwrap_or(None).unwrap_or(0);
            if ctx.compact_threshold > 0 && n >= ctx.compact_threshold {
                let parent = {
                    let inner = state.inner.lock().await;
                    inner.projects.active_sub_conv(project_id, sub_id).and_then(|c| c.parent_message_id)
                };
                let mut report_req = CompletionReq::new(session_id.clone(), REPORT_PROMPT.to_string());
                report_req.parent_message_id = parent;
                if !state.throttle_account(account, SUB_GAP, cancel).await {
                    return LoopOutcome { text: String::new(), ok: false, reason: Some("cancelled".into()) };
                }
                let report = {
                    let mut noop = |_: DeltaKind, _: &str| {};
                    match model.complete(&report_req, &mut noop).await {
                        Ok(r) => r.content,
                        Err(e) => format!("[交接报告生成失败: {e}]"),
                    }
                };
                if cancel.is_cancelled() {
                    return LoopOutcome { text: String::new(), ok: false, reason: Some("cancelled".into()) };
                }
                let new_sid = match model.new_session().await {
                    Ok(s) => s,
                    Err(e) => return LoopOutcome { text: String::new(), ok: false, reason: Some(format!("new_session: {e}")) },
                };
                {
                    let mut inner = state.inner.lock().await;
                    inner.projects.push_sub_conv(project_id, sub_id, &new_sid, None);
                    let _ = inner.projects.save();
                }
                let _ = ctx.app.emit("agent://sub-handoff", serde_json::json!({
                    "project_id": project_id, "sub_id": sub_id, "sub_name": ctx.sub_name, "count": n
                }));
                session_id = new_sid;
                next_prompt = format!(
                    "{}\n\n{}\n\n{}\n{}\n===== 交接报告结束 =====\n\n{}",
                    crate::prompt_gui::system_prompt_with_env(),
                    crate::prompt_gui::SUBAGENT_TOOLS_SECTION,
                    HANDOFF_INTRO,
                    report,
                    input
                );
            }
        }

        if cancel.is_cancelled() {
            return LoopOutcome { text: String::new(), ok: false, reason: Some("cancelled".into()) };
        }
        send_delay(cancel).await;
        if cancel.is_cancelled() {
            return LoopOutcome { text: String::new(), ok: false, reason: Some("cancelled".into()) };
        }

        let parent = {
            let inner = state.inner.lock().await;
            inner.projects.active_sub_conv(project_id, sub_id).and_then(|c| c.parent_message_id)
        };

        let mut req = CompletionReq::new(session_id.clone(), next_prompt.clone());
        req.parent_message_id = parent;

        let gate = Arc::new(std::sync::Mutex::new(AnswerStreamGate::new()));
        let pending = Arc::new(std::sync::Mutex::new(String::new()));
        let mut on_delta = {
            let ec = ec.clone();
            let pending = pending.clone();
            let gate = gate.clone();
            move |kind: DeltaKind, text: &str| {
                if kind == DeltaKind::Thinking {
                    ec.delta("thinking", text);
                    return;
                }
                let chunks = gate.lock().unwrap().push(text);
                let mut buf = pending.lock().unwrap();
                for c in chunks { buf.push_str(&c); }
            }
        };

        if !state.throttle_account(account, SUB_GAP, cancel).await {
            return LoopOutcome { text: String::new(), ok: false, reason: Some("cancelled".into()) };
        }
        let reply = tokio::select! {
            r = model.complete(&req, &mut on_delta) => match r {
                Ok(x) => x,
                Err(e) => return LoopOutcome { text: String::new(), ok: false, reason: Some(format!("complete: {e}")) },
            },
            _ = cancel.cancelled() => {
                return LoopOutcome { text: String::new(), ok: false, reason: Some("cancelled".into()) };
            }
        };
        {
            let tail = std::mem::take(&mut *pending.lock().unwrap());
            if !tail.is_empty() { ec.delta("answer", &tail); }
        }

        if let Some(real_sid) = &reply.session_id {
            if !real_sid.is_empty() && real_sid != &session_id {
                {
                    let mut inner = state.inner.lock().await;
                    inner.projects.update_sub_conv_session(project_id, sub_id, &session_id, real_sid);
                    let _ = inner.projects.save();
                }
                session_id = real_sid.clone();
            }
        }

        if reply.message_id.is_some() {
            let mut inner = state.inner.lock().await;
            inner.projects.set_sub_parent(project_id, sub_id, reply.message_id);
            let _ = inner.projects.save();
        }

        match parser::parse(&reply.content) {
            ParseOutcome::Text(text) => {
                // 先 flush gate 中缓冲的尾部（防止以工具前缀开头的正文被吞）。
                {
                    let tail = gate.lock().unwrap().finish_text();
                    if let Some(t) = tail {
                        ec.delta("answer", &t);
                    }
                }
                let cleaned = text.trim().to_string();
                let ok = !cleaned.is_empty();
                return LoopOutcome { text: cleaned, ok, reason: if ok { None } else { Some("empty".into()) } };
            }
            ParseOutcome::Calls(calls) => {
                gate.lock().unwrap().finish_text();
                let mut results = String::new();
                let mut call_id: u64 = 0;
                for call in calls {
                    if cancel.is_cancelled() {
                        return LoopOutcome { text: String::new(), ok: false, reason: Some("cancelled".into()) };
                    }
                    call_id += 1;
                    let command = match call.name.as_str() {
                        "read_file" => call.params.get("file_path").or_else(|| call.params.get("path")).cloned().unwrap_or_default(),
                        "execute_command" => call.params.get("command").or_else(|| call.params.get("cmd")).cloned().unwrap_or_default(),
                        _ => call.params.get("action").or_else(|| call.params.get("command")).cloned().unwrap_or_default(),
                    };
                    ec.tool(serde_json::json!({
                        "phase": "call", "call_id": call_id, "name": call.name, "command": command,
                    }));
                    let output = execute_tool(&ctx.app, state.inner(), model, project_id, &call, cancel).await;
                    let (ok, body) = match output { Ok(o) => (true, o), Err(e) => (false, e) };
                    if !body.is_empty() {
                        ec.tool(serde_json::json!({
                            "phase": "output", "call_id": call_id, "name": call.name, "output": &body,
                        }));
                    }
                    ec.tool(serde_json::json!({
                        "phase": "done", "call_id": call_id, "name": call.name, "error": !ok,
                    }));
                    results.push_str(&format!("<tool_result name=\"{}\">\n{}\n</tool_result>\n", call.name, body));
                }
                next_prompt = results;
                continue;
            }
            ParseOutcome::Malformed { reason, .. } => {
                let _ = ctx.app.emit("agent://sub-format-error", serde_json::json!({
                    "project_id": project_id, "sub_id": sub_id, "reason": reason
                }));
                return LoopOutcome { text: String::new(), ok: false, reason: Some(format!("malformed: {reason}")) };
            }
            ParseOutcome::DsmlMalformed { reason, marker_count, .. } => {
                let _ = ctx.app.emit("agent://sub-format-error", serde_json::json!({
                    "project_id": project_id, "sub_id": sub_id, "reason": reason, "marker_count": marker_count
                }));
                return LoopOutcome { text: String::new(), ok: false, reason: Some(format!("dsml: {reason}")) };
            }
        }
    }
}

pub struct SubAgentSpec {
    pub project_id: String,
    pub name: String,
    pub duty: String,
    pub task: String,
    pub agent_type: String,
}

async fn resolve_session(
    state: &crate::state::AppState,
    model: &Arc<dyn Model>,
    spec: &SubAgentSpec,
    sub_id: &str,
) -> Result<(String, bool), String> {
    let existing = {
        let inner = state.inner.lock().await;
        inner
            .projects
            .sub_agents_of(&spec.project_id)
            .into_iter()
            .find(|s| s.id == sub_id)
    };
    if let Some(sa) = existing {
        if let Some(conv) = sa.conversations.iter().find(|c| c.is_active && !c.session_id.is_empty()) {
            return Ok((conv.session_id.clone(), true));
        }
    }
    let sid = model.new_session().await.map_err(|e| e.to_string())?;
    {
        let mut inner = state.inner.lock().await;
        inner.projects.push_sub_conv(&spec.project_id, sub_id, &sid, None);
        let _ = inner.projects.save();
    }
    Ok((sid, false))
}

pub async fn run_subagent(
    app: AppHandle,
    spec: SubAgentSpec,
) -> Result<LoopOutcome, String> {
    let state = app.state::<crate::state::AppState>();

    // 选账号 + 建立身份合到同一把锁内，避免并发派发超配（都读到同一空闲名额）。
    // 同名子 agent 已存在 → 复用其 account（不重新选账号）。
    let (sub_id, account) = {
        let mut inner = state.inner.lock().await;

        let project_account = inner
            .projects
            .project(&spec.project_id)
            .map(|p| p.account.clone())
            .unwrap_or_default();

        let existing = inner
            .projects
            .sub_agents_of(&spec.project_id)
            .into_iter()
            .find(|s| s.name == spec.name);

        let account = if let Some(sa) = &existing {
            if sa.account.is_empty() {
                project_account.clone()
            } else {
                sa.account.clone()
            }
        } else {
            let mut order = inner.projects.sub_accounts_of(&spec.project_id);
            if !order.contains(&project_account) {
                order.insert(0, project_account.clone());
            }
            let limits = inner.projects.sub_account_limits_of(&spec.project_id);

            let mut used: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
            for sa in inner.projects.sub_agents_of(&spec.project_id) {
                let a = if sa.account.is_empty() {
                    project_account.clone()
                } else {
                    sa.account.clone()
                };
                *used.entry(a).or_insert(0) += 1;
            }

            let others_sum: u32 = order
                .iter()
                .filter(|a| **a != project_account)
                .map(|a| *limits.get(a).unwrap_or(&0))
                .sum();
            let main_limit = 3u32.saturating_sub(others_sum);

            let mut chosen = project_account.clone();
            for acc in &order {
                let limit = if *acc == project_account {
                    main_limit
                } else {
                    *limits.get(acc).unwrap_or(&0)
                };
                if limit > 0 && *used.get(acc).unwrap_or(&0) < limit {
                    chosen = acc.clone();
                    break;
                }
            }
            chosen
        };

        let sid = inner
            .projects
            .add_sub_agent(&spec.project_id, &spec.name, &spec.duty, &account)
            .ok_or("创建子 agent 失败")?;
        let _ = inner.projects.save();
        (sid, account)
    };

    // 同名并发保护：同名子 agent 已处于 Starting/Running 时拒绝再次派发，
    // 否则后续 sub_runs.insert 会覆盖旧运行的 cancel token / 状态 / generation。
    {
        let inner = state.inner.lock().await;
        if let Some(r) = inner.sub_runs.get(&sub_id) {
            if matches!(r.status, SubRunStatus::Starting | SubRunStatus::Running) {
                return Err(format!("子 agent「{}」正在运行中", spec.name));
            }
        }
    }

    // 2. 用唯一 sub_id 建/取子 bridge（不同子 agent → 不同 bridge → 并发）。
    let model: Arc<dyn Model> = state.ensure_sub_bridge(&account, &sub_id).await?;

    // 3. 解析会话（复用 active conv / 新建）。
    //    失败时释放刚建的 bridge/profile，避免泄漏（此时尚未注册到 sub_runs）。
    let (session_id, prompt_already_sent) =
        match resolve_session(state.inner(), &model, &spec, &sub_id).await {
            Ok(v) => v,
            Err(e) => {
                state.release_sub_bridge(&account, &sub_id).await;
                return Err(e);
            }
        };

    let cancel = CancellationToken::new();
    let workdir = {
        let inner = state.inner.lock().await;
        inner.projects.workdir_of(&spec.project_id).unwrap_or_else(|| {
            dirs::home_dir().unwrap_or_default().to_string_lossy().to_string()
        })
    };
    let compact_threshold = {
        let inner = state.inner.lock().await;
        inner.projects.compact_of(&spec.project_id)
    };
    let run_generation = {
        let mut inner = state.inner.lock().await;
        // CAS：拿锁后再校验一次，防止两次同名派发竞态都通过前面的检查。
        if let Some(r) = inner.sub_runs.get(&sub_id) {
            if matches!(r.status, SubRunStatus::Starting | SubRunStatus::Running) {
                return Err(format!("子 agent「{}」正在运行中", spec.name));
            }
        }
        let gen = inner
            .sub_runs
            .values()
            .map(|r| r.run_generation)
            .max()
            .unwrap_or(0)
            .wrapping_add(1);
        inner.sub_runs.insert(sub_id.clone(), SubRunCtx {
            sid: sub_id.clone(),
            name: spec.name.clone(),
            duty: spec.duty.clone(),
            project_id: spec.project_id.clone(),
            account: account.clone(),
            agent_type: spec.agent_type.clone(),
            profile_id: sub_id.clone(),
            status: SubRunStatus::Running,
            cancel: cancel.clone(),
            run_generation: gen,
            started_at: Instant::now(),
            attempts: 0,
            last_text: None,
            last_ok: None,
        });
        gen
    };
    let _ = app.emit("agent://sub-start", serde_json::json!({
        "project_id": spec.project_id, "sub_id": sub_id, "name": spec.name, "duty": spec.duty
    }));

    let ctx = SubLoopCtx {
        app: app.clone(),
        project_id: spec.project_id.clone(),
        sub_id: sub_id.clone(),
        sub_name: spec.name.clone(),
        account: account.clone(),
        model,
        session_id,
        cancel: cancel.clone(),
        first_input: spec.task.clone(),
        prompt_already_sent,
        workdir,
        compact_threshold,
    };
    let outcome = agent_loop(ctx).await;

    // 结果判定：有正文结果即视为成功，即使期间收到 cancel。
    // 仅"无正文且被取消"才算取消；"无正文且未取消"才算空失败。
    let has_result = outcome.ok && !outcome.text.trim().is_empty();
    let cancelled = !has_result
        && (cancel.is_cancelled() || matches!(outcome.reason.as_deref(), Some("cancelled")));

    if !has_result {
        let (status, reason, ok) = if cancelled {
            (SubRunStatus::Cancelled, "cancelled", false)
        } else {
            (SubRunStatus::Failed, "empty", false)
        };
        {
            let mut inner = state.inner.lock().await;
            if let Some(run) = inner.sub_runs.get_mut(&sub_id) {
                if run.run_generation == run_generation {
                    run.status = status;
                    run.last_text = Some(outcome.text.clone());
                    run.last_ok = Some(ok);
                }
            }
        }
        let _ = app.emit("agent://sub-done", serde_json::json!({
            "project_id": spec.project_id, "sub_id": sub_id, "name": spec.name,
            "ok": ok, "reason": reason, "result": ""
        }));
        return Ok(outcome);
    }

    {
        let mut inner = state.inner.lock().await;
        if let Some(run) = inner.sub_runs.get_mut(&sub_id) {
            if run.run_generation == run_generation {
                run.status = SubRunStatus::Succeeded;
                run.last_text = Some(outcome.text.clone());
                run.last_ok = Some(true);
            }
        }
    }
    let _ = app.emit("agent://sub-done", serde_json::json!({
        "project_id": spec.project_id, "sub_id": sub_id, "name": spec.name,
        "ok": true, "reason": outcome.reason, "result": outcome.text
    }));

    {
        let turn_active = {
            let inner = state.inner.lock().await;
            inner.running.get(&spec.project_id).map(|c| c.turn_active).unwrap_or(false)
        };
        if turn_active {
            let mut inner = state.inner.lock().await;
            inner.pending_sub_results.entry(spec.project_id.clone()).or_default()
                .push(crate::state::SubResult {
                    name: spec.name.clone(),
                    text: outcome.text.clone(),
                    ok: outcome.ok,
                });
        } else {
            let ctx_exists = {
                let inner = state.inner.lock().await;
                inner.running.contains_key(&spec.project_id)
            };
            if ctx_exists {
                let start_fn = {
                    let inner = state.inner.lock().await;
                    inner.start_turn.clone()
                };
                if let Some(f) = start_fn {
                    let _ = f(
                        app.clone(),
                        spec.project_id.clone(),
                        format!(
                            "[系统] 子 agent「{}」已完成。\n<sub_agent_result name=\"{}\" ok=\"{}\">\n{}\n</sub_agent_result>\n\n{}",
                            spec.name, spec.name, outcome.ok, outcome.text, crate::runner::SUB_REVIEW_PROMPT
                        ),
                    )
                    .await;
                }
            }
        }
    }

    {
        let inner_arc = state.inner.clone();
        let acc = account.clone();
        let pid_profile = sub_id.clone();
        let sid = sub_id.clone();
        let app_w = app.clone();
        let my_gen = run_generation;
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            // 仅当仍是本次运行实例（generation 匹配）且已终态时才清理，防止同名重启后误删。
            let should_close = {
                let inner = inner_arc.lock().await;
                inner.sub_runs.get(&sid)
                    .map(|r| r.run_generation == my_gen
                        && matches!(r.status,
                            SubRunStatus::Succeeded | SubRunStatus::Failed
                            | SubRunStatus::Cancelled | SubRunStatus::Closed))
                    .unwrap_or(false)
            };
            if should_close {
                let (pid, nm) = {
                    let mut inner = inner_arc.lock().await;
                    // 再次校验 generation：锁内 CAS，若已被新运行替换则不删。
                    let info = inner.sub_runs.get(&sid)
                        .filter(|r| r.run_generation == my_gen)
                        .map(|r| (r.project_id.clone(), r.name.clone()));
                    let info = match info {
                        Some(i) => { inner.sub_runs.remove(&sid); i }
                        None => return,
                    };
                    info
                };
                let st = app_w.state::<crate::state::AppState>();
                st.release_sub_bridge(&acc, &pid_profile).await;
                let _ = app_w.emit("agent://sub-closed", serde_json::json!({
                    "project_id": pid, "sub_id": sid, "name": nm,
                }));
                {
                    let mut inner = inner_arc.lock().await;
                    let has_running_sub = inner.sub_runs.values().any(|r| {
                        r.project_id == pid
                            && matches!(r.status, SubRunStatus::Running | SubRunStatus::Starting)
                    });
                    if let Some(ctx) = inner.running.get_mut(&pid) {
                        if !ctx.turn_active { ctx.busy = has_running_sub; }
                    }
                }
            }
        });
    }

    Ok(outcome)
}