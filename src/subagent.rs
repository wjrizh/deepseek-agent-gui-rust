//! 子 agent：独立循环（复制自主 agent 循环骨架，仅供子 agent 使用，不改 run_turn）。
//! 对话内容不落本地；本地只在 projects 里存 SubAgent 身份 + 会话段引用。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::runner::{execute_tool, send_delay, AnswerStreamGate, EmitCtx, HANDOFF_INTRO, REPORT_PROMPT};
use crate::prompt_gui::{DSML_ERROR_PROMPT, FORMAT_ERROR_PROMPT};
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
    pub sub_duty: String,
    pub account: String,
    pub model: Arc<dyn Model>,
    pub session_id: String,
    pub cancel: CancellationToken,
    pub first_input: String,
    pub prompt_already_sent: bool,
    pub workdir: String,
    pub compact_threshold: i64,
    pub run_generation: u64,
}

/// P1-9 第一步：安全拼接子 agent 结果块（转义 + 限长）。
/// name 属性转义 引号/尖括号/与号；正文转义 结束标签，并限长。
pub fn format_sub_result(name: &str, ok: bool, text: &str) -> String {
    fn esc_attr(x: &str) -> String {
        x.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    const MAX: usize = 8000;
    let total = text.chars().count();
    let close_tag = "</sub_agent_result>";
    let mut body = text.replace(close_tag, "&lt;/sub_agent_result&gt;");
    if total > MAX {
        let head: String = body.chars().take(MAX).collect();
        body = format!("{head}\n...[已截断]");
    }
    format!(
        "\n<sub_agent_result name=\"{}\" ok=\"{}\" truncated=\"{}\">\n{}\n</sub_agent_result>\n",
        esc_attr(name),
        ok,
        total > MAX,
        body
    )
}

/// 截断原始回复作为诊断摘要（限 2000 字符）。
fn excerpt(raw: &str) -> String {
    let t = raw.trim();
    if t.chars().count() <= 2000 {
        t.to_string()
    } else {
        let head: String = t.chars().take(2000).collect();
        format!("{head}\n...[已截断]")
    }
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
    // 格式错误重试上限（与工具重试独立）：避免无限消耗额度/风控/占用 bridge。
    const SUB_FORMAT_MAX_ATTEMPTS: u32 = 3;
    let mut format_attempts: u32 = 0;
    // 模型主动请求压缩：置位后在本轮顶部走压缩分支。
    let mut force_compact = false;
    // 上次注入 `[运行状态]` 时的消息数分桶（每 10 条注入一次）。
    let mut last_status_bucket: i64 = -1;

    loop {
        {
            let n = model.message_count(&session_id).await.unwrap_or(None).unwrap_or(0);
            if force_compact || (ctx.compact_threshold > 0 && n >= ctx.compact_threshold) {
                force_compact = false;
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
                if !new_sid.is_empty() {
                    let mut inner = state.inner.lock().await;
                    inner.projects.push_sub_conv(project_id, sub_id, &new_sid, None);
                    let _ = inner.projects.save();
                }
                // 空 session（网页内核）不持久化；首轮回复拿到真实 UUID 后再补建子会话。
                let _ = ctx.app.emit("agent://sub-handoff", serde_json::json!({
                    "project_id": project_id, "sub_id": sub_id, "sub_name": ctx.sub_name,
                    "name": ctx.sub_name, "duty": ctx.sub_duty, "count": n,
                    "run_generation": ctx.run_generation
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
                    if session_id.is_empty() {
                        // 压缩交接后首次拿到真实 UUID：补建子会话（避免持久化空 session）。
                        inner.projects.push_sub_conv(project_id, sub_id, real_sid, None);
                    } else {
                        inner.projects.update_sub_conv_session(project_id, sub_id, &session_id, real_sid);
                    }
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
                // 子 agent 无"完成信号"约定：只要没有工具调用即视为完成，
                // 直接返回文本给主 agent（strip_done_marker 仅作兼容清理）。
                let (body, _) = crate::runner::strip_done_marker(&text);
                let cleaned = body.trim().to_string();
                let ok = !cleaned.is_empty();
                return LoopOutcome { text: cleaned, ok, reason: if ok { None } else { Some("empty".into()) } };
            }
            ParseOutcome::Calls(calls) => {
                // 出现合法工具调用 = 格式恢复正常，重置"连续格式错误"计数。
                format_attempts = 0;
                let (done_calls, rest): (Vec<_>, Vec<_>) =
                    calls.into_iter().partition(|c| c.name == "task_complete");
                let (compact_calls, tool_calls): (Vec<_>, Vec<_>) =
                    rest.into_iter().partition(|c| c.name == "request_context_compaction");
                let want_complete = !done_calls.is_empty() && tool_calls.is_empty()
                    && compact_calls.is_empty()
                    && done_calls.iter().any(|c| !c.params.get("summary").map(|x| x.trim().is_empty()).unwrap_or(true));
                let want_compact = !compact_calls.is_empty() && tool_calls.is_empty() && done_calls.is_empty();
                let calls = tool_calls;
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
                if want_complete {
                    let summary = done_calls.iter()
                        .filter_map(|c| c.params.get("summary"))
                        .map(|s| s.trim().to_string())
                        .find(|s| !s.is_empty())
                        .unwrap_or_default();
                    return LoopOutcome { text: summary, ok: true, reason: None };
                }
                if want_compact {
                    // 防滥用：本会话消息数过少（<10）时忽略请求。
                    let n = model.message_count(&session_id).await.unwrap_or(None).unwrap_or(0);
                    if n < 10 {
                        results.push_str("[系统] 忽略本次上下文压缩请求：当前会话消息过少，无需压缩。\n");
                        next_prompt = results;
                        continue;
                    }
                    force_compact = true;
                    continue;
                }
                {
                    let used = model.message_count(&session_id).await.unwrap_or(None).unwrap_or(0);
                    let bucket = used / 10;
                    if bucket != last_status_bucket {
                        last_status_bucket = bucket;
                        results.push('\n');
                        results.push_str(&crate::runner::status_block(&state, project_id, model, &session_id).await);
                    }
                }
                next_prompt = results;
                continue;
            }
            ParseOutcome::Malformed { raw, reason } => {
                let _ = ctx.app.emit("agent://sub-format-error", serde_json::json!({
                    "project_id": project_id, "sub_id": sub_id, "reason": reason,
                    "name": ctx.sub_name, "duty": ctx.sub_duty,
                    "attempt": format_attempts + 1, "run_generation": ctx.run_generation
                }));
                format_attempts += 1;
                if format_attempts > SUB_FORMAT_MAX_ATTEMPTS {
                    return LoopOutcome {
                        text: excerpt(&raw),
                        ok: false,
                        reason: Some("format".into()),
                    };
                }
                next_prompt = format!("{FORMAT_ERROR_PROMPT}\n\n（解析失败原因：{reason}）");
                continue;
            }
            ParseOutcome::DsmlMalformed { raw, reason, marker_count } => {
                let _ = ctx.app.emit("agent://sub-format-error", serde_json::json!({
                    "project_id": project_id, "sub_id": sub_id, "reason": reason,
                    "name": ctx.sub_name, "duty": ctx.sub_duty,
                    "marker_count": marker_count, "attempt": format_attempts + 1,
                    "run_generation": ctx.run_generation
                }));
                format_attempts += 1;
                if format_attempts > SUB_FORMAT_MAX_ATTEMPTS {
                    return LoopOutcome {
                        text: excerpt(&raw),
                        ok: false,
                        reason: Some("format".into()),
                    };
                }
                next_prompt = format!(
                    "{DSML_ERROR_PROMPT}\n\n（检测到 {marker_count} 个工具标记，解析失败原因：{reason}）"
                );
                continue;
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
    // 网页内核 new_session() 返回空串：不建空本地会话，待首轮拿到真实 UUID 后补建。
    if !sid.is_empty() {
        let mut inner = state.inner.lock().await;
        inner.projects.push_sub_conv(&spec.project_id, sub_id, &sid, None);
        let _ = inner.projects.save();
    }
    Ok((sid, false))
}

/// 把子 agent 结果回注主 agent：主 turn 活跃时入 pending（随收尾注入），
/// 否则直接唤起一轮审阅 turn。限流挂起时 text 前缀带标记，供主 agent 决策重派。
async fn inject_sub_result(
    app: AppHandle,
    state: &crate::state::AppState,
    project_id: String,
    name: String,
    ok: bool,
    text: &str,
) {
    let turn_active = {
        let inner = state.inner.lock().await;
        inner.running.get(&project_id).map(|c| c.turn_active).unwrap_or(false)
    };
    if turn_active {
        let mut inner = state.inner.lock().await;
        inner.pending_sub_results.entry(project_id.clone()).or_default()
            .push(crate::state::SubResult { name, text: text.to_string(), ok });
        return;
    }
    let ctx_exists = {
        let inner = state.inner.lock().await;
        inner.running.contains_key(&project_id)
    };
    if !ctx_exists {
        return;
    }
    let start_fn = {
        let inner = state.inner.lock().await;
        inner.start_turn.clone()
    };
    if let Some(f) = start_fn {
        let block = format!(
            "[系统] 子 agent「{}」已结束。{}{}",
            name,
            format_sub_result(&name, ok, text),
            crate::runner::SUB_REVIEW_PROMPT
        );
        let _ = f(app, project_id, block).await;
    }
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
            if inner.projects.sub_account_mode_of(&spec.project_id) == "active" {
                // active 模式：仅运行中的子 agent 占用名额（历史绑定不占）。
                for r in inner.sub_runs.values() {
                    if r.project_id == spec.project_id
                        && matches!(r.status, SubRunStatus::Starting | SubRunStatus::Running)
                    {
                        let a = if r.account.is_empty() {
                            project_account.clone()
                        } else {
                            r.account.clone()
                        };
                        *used.entry(a).or_insert(0) += 1;
                    }
                }
            } else {
                // permanent 模式：该账号下所有子 agent（含历史绑定）长期占用。
                for sa in inner.projects.sub_agents_of(&spec.project_id) {
                    let a = if sa.account.is_empty() {
                        project_account.clone()
                    } else {
                        sa.account.clone()
                    };
                    *used.entry(a).or_insert(0) += 1;
                }
            }

            let others_sum: u32 = order
                .iter()
                .filter(|a| **a != project_account)
                .map(|a| *limits.get(a).unwrap_or(&0))
                .sum();
            let total_limit = inner.projects.max_sub_of(&spec.project_id);
            let main_limit = total_limit.saturating_sub(others_sum);

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
    // 例外：限流挂起的子 agent 允许重派（重派以新 generation 接管）。
    {
        let inner = state.inner.lock().await;
        if let Some(r) = inner.sub_runs.get(&sub_id) {
            if matches!(r.status, SubRunStatus::Starting | SubRunStatus::Running)
                && !r.rate_limited_suspended
            {
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
                state.destroy_sub_bridge(&account, &sub_id).await;
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
            missing_completion_count: 0,
            rate_limited_suspended: false,
            last_task: spec.task.clone(),
        });
        gen
    };
    let _ = app.emit("agent://sub-start", serde_json::json!({
        "project_id": spec.project_id, "sub_id": sub_id, "name": spec.name, "duty": spec.duty,
        "run_generation": run_generation
    }));

    let ctx = SubLoopCtx {
        app: app.clone(),
        project_id: spec.project_id.clone(),
        sub_id: sub_id.clone(),
        sub_name: spec.name.clone(),
        sub_duty: spec.duty.clone(),
        account: account.clone(),
        model,
        session_id,
        cancel: cancel.clone(),
        first_input: spec.task.clone(),
        prompt_already_sent,
        workdir,
        compact_threshold,
        run_generation,
    };
    let outcome = agent_loop(ctx).await;

    // 结果判定：有正文结果即视为成功，即使期间收到 cancel。
    // 仅"无正文且被取消"才算取消；"无正文且未取消"才算空失败。
    let has_result = outcome.ok && !outcome.text.trim().is_empty();
    let cancelled = !has_result
        && (cancel.is_cancelled() || matches!(outcome.reason.as_deref(), Some("cancelled")));

    // 限流挂起：保持 Running（不判失败），保持主 agent busy，
    // 仅回注主 agent 审阅（带限流标记），由其决定是否重派同名子 agent。
    if matches!(outcome.reason.as_deref(), Some("rate_suspended")) {
        // 保持 Running（不判失败）：bridge/卡片保留、主 agent busy 保持，
        // 由主 agent 审阅后决定是否重派同名子 agent。
        {
            let mut inner = state.inner.lock().await;
            if let Some(run) = inner.sub_runs.get_mut(&sub_id) {
                if run.run_generation == run_generation {
                    run.last_text = Some(outcome.text.clone());
                    run.last_ok = Some(false);
                }
            }
        }
        let _ = app.emit("agent://sub-done", serde_json::json!({
            "project_id": spec.project_id, "sub_id": sub_id, "name": spec.name,
            "ok": false, "reason": "rate_suspended",
            "run_generation": run_generation, "result": outcome.text.clone()
        }));
        let text = format!("[限流挂起] 子 agent 连续多次未收到完成信号，疑似被限流，未完成。\n{}", outcome.text);
        inject_sub_result(app.clone(), &state, spec.project_id.clone(), spec.name.clone(), false, &text).await;
        return Ok(outcome);
    }

    if !has_result {
        // 优先识别"格式错误超限"：此时 outcome.text 为最后一次原始回复摘要。
        let is_format_failed = matches!(outcome.reason.as_deref(), Some("format"));
        let (status, reason, ok) = if is_format_failed {
            (SubRunStatus::Failed, "format", false)
        } else if cancelled {
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
            "ok": ok, "reason": reason,
            "run_generation": run_generation,
            // 格式失败时把最后一次原始回复摘要带出，便于诊断/重试。
            "result": if is_format_failed { outcome.text.clone() } else { String::new() }
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
        "ok": true, "reason": outcome.reason, "result": outcome.text,
        "run_generation": run_generation
    }));

    inject_sub_result(app.clone(), &state, spec.project_id.clone(), spec.name.clone(), outcome.ok, &outcome.text).await;

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
                let (pid, nm, nd) = {
                    let mut inner = inner_arc.lock().await;
                    // 再次校验 generation：锁内 CAS，若已被新运行替换则不删。
                    let info = inner.sub_runs.get(&sid)
                        .filter(|r| r.run_generation == my_gen)
                        .map(|r| (r.project_id.clone(), r.name.clone(), r.duty.clone()));
                    let info = match info {
                        Some(i) => { inner.sub_runs.remove(&sid); i }
                        None => return,
                    };
                    info
                };
                let st = app_w.state::<crate::state::AppState>();
                st.release_sub_bridge(&acc, &pid_profile).await;
                // 调度一次 idle worker：子 bridge 在 TTL 后被统一回收（支持短时重派复用）。
                {
                    let inner_for_idle = inner_arc.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(crate::state::BRIDGE_IDLE_SECS + 5)).await;
                        let _ = crate::state::AppState::close_idle(
                            &inner_for_idle,
                            Duration::from_secs(crate::state::BRIDGE_IDLE_SECS),
                        ).await;
                    });
                }
                let _ = app_w.emit("agent://sub-closed", serde_json::json!({
                    "project_id": pid, "sub_id": sid, "name": nm, "duty": nd,
                    "run_generation": my_gen,
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