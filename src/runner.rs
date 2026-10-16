//! GUI 侧 Agent 驱动循环：复用 deepseek-agent 的 Model/parser/Tool，
//! 通过 Tauri 事件把流式增量推给前端，不依赖终端 UI。
//!
//! 多项目架构：所有运行期状态挂在 `state.inner.running[project_id]`，
//! 事件均带 `project_id` 供前端按项目路由。不同项目可并发运行。

use std::sync::Arc;
use std::time::Duration;

use crate::prompt_gui::{DSML_ERROR_PROMPT, FORMAT_ERROR_PROMPT, TOOLS_SECTION_GUI, system_prompt_with_remote};
use crate::state::{AppState, BRIDGE_IDLE_SECS};
use deepseek_agent::agent::model::Model;
use deepseek_agent::agent::parser::{self, ParseOutcome};
use deepseek_agent::agent::tool::Tool;
use deepseek_agent::api::types::{CompletionReq, DeltaKind};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerStreamState {
    Normal,
    MaybeTool,
    ToolBuffer,
}

pub(crate) struct AnswerStreamGate {
    state: AnswerStreamState,
    buf: String,
    sent: usize,
}

impl AnswerStreamGate {
    pub(crate) fn new() -> Self {
        Self {
            state: AnswerStreamState::Normal,
            buf: String::new(),
            sent: 0,
        }
    }

    pub(crate) fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        match self.state {
            AnswerStreamState::Normal => self.handle_normal(),
            AnswerStreamState::MaybeTool => self.handle_maybe_tool(),
            AnswerStreamState::ToolBuffer => Vec::new(),
        }
    }

    pub(crate) fn finish_text(&mut self) -> Option<String> {
        if self.sent >= self.buf.len() {
            self.state = AnswerStreamState::Normal;
            return None;
        }
        let out = self.buf[self.sent..].to_string();
        self.sent = self.buf.len();
        self.state = AnswerStreamState::Normal;
        Some(out)
    }

    #[cfg(test)]
    fn suppress_tool_buffer(&mut self) {
        self.sent = self.buf.len();
        self.state = AnswerStreamState::Normal;
    }

    fn handle_normal(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(pos) = find_tool_marker(&self.buf[self.sent..]) {
            let absolute = self.sent + pos;
            if absolute > self.sent {
                out.push(self.buf[self.sent..absolute].to_string());
                self.sent = absolute;
            }
            self.state = AnswerStreamState::ToolBuffer;
            return out;
        }
        if ends_with_tool_prefix(&self.buf[self.sent..]) {
            self.state = AnswerStreamState::MaybeTool;
            return out;
        }
        if self.sent < self.buf.len() {
            out.push(self.buf[self.sent..].to_string());
            self.sent = self.buf.len();
        }
        out
    }

    fn handle_maybe_tool(&mut self) -> Vec<String> {
        let pending = &self.buf[self.sent..];
        if let Some(pos) = find_tool_marker(pending) {
            let absolute = self.sent + pos;
            let mut out = Vec::new();
            if absolute > self.sent {
                out.push(self.buf[self.sent..absolute].to_string());
                self.sent = absolute;
            }
            self.state = AnswerStreamState::ToolBuffer;
            return out;
        }
        if ends_with_tool_prefix(pending) {
            return Vec::new();
        }
        self.state = AnswerStreamState::Normal;
        Vec::new()
    }
}

const TOOL_MARKERS: &[&str] = &[
    "<tool_calls",
    "<tool call",
    "<tool_call",
    "<function_call",
    "<invoke",
    "<calls",
];

fn is_marker_noise(c: char) -> bool {
    matches!(c, '|' | '｜' | '\u{2581}' | '_' | '-' | ' ' | '\t' | '\n' | '\r')
}

fn compact_for_match(text: &str) -> (String, Vec<usize>) {
    let mut bytes: Vec<u8> = Vec::with_capacity(text.len());
    let mut map: Vec<usize> = Vec::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        if is_marker_noise(c) {
            continue;
        }
        let lc = c.to_ascii_lowercase();
        let mut buf = [0u8; 4];
        let enc = lc.encode_utf8(&mut buf).as_bytes();
        for &b in enc {
            bytes.push(b);
            map.push(i);
        }
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut out_map: Vec<usize> = Vec::with_capacity(map.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"dsml") {
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        out_map.push(map[i]);
        i += 1;
    }
    let s = String::from_utf8(out).unwrap_or_default();
    (s, out_map)
}

pub(crate) fn find_tool_marker(text: &str) -> Option<usize> {
    let (compact, map) = compact_for_match(text);
    TOOL_MARKERS
        .iter()
        .filter_map(|marker| {
            let (mcompact, _) = compact_for_match(marker);
            compact.find(&mcompact).map(|pos| map[pos])
        })
        .min()
}

pub(crate) fn ends_with_tool_prefix(text: &str) -> bool {
    let (compact, _) = compact_for_match(text);
    TOOL_MARKERS.iter().any(|marker| {
        let (mcompact, _) = compact_for_match(marker);
        let max = compact.len().min(mcompact.len().saturating_sub(1));
        (1..=max).any(|n| compact.ends_with(&mcompact[..n]))
    })
}



pub(crate) const REPORT_PROMPT: &str = "请对本次对话做一份工作交接报告，用中文输出，包含以下部分：\n1. 任务目标\n2. 已完成的工作与产出\n3. 积累的经验/结论\n4. 尚未解决的问题\n5. 接下来要解决的问题\n只输出报告本身，不要客套。";

pub(crate) const HANDOFF_INTRO: &str = "以下是从上一会话交接过来的工作进度报告。请先确认收到这份报告，然后我们继续。\n\n===== 交接报告开始 =====\n";

pub(crate) const SUB_REVIEW_PROMPT: &str = "以下是子 agent 的完成结果。请审阅：若合格，简要说明并继续；若异常（幻觉/不完整/格式错/中断），用同名 sub_agent 重试（最多 3 次）；3 次仍失败则给出可执行的解决方案。";

const SEND_DELAY_BASE: Duration = Duration::from_secs(7);
const SEND_DELAY_JITTER_MS: u64 = 4000;
const GLOBAL_REQUEST_GAP: Duration = Duration::from_secs(10);

pub(crate) async fn send_delay(cancel: &CancellationToken) {
    let jitter = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        nanos % (SEND_DELAY_JITTER_MS + 1)
    };
    let total = SEND_DELAY_BASE + Duration::from_millis(jitter);
    tokio::select! {
        _ = tokio::time::sleep(total) => {}
        _ = cancel.cancelled() => {}
    }
}

struct BusyGuard {
    app: AppHandle,
    pid: String,
    cancel: CancellationToken,
    project_name: String,
}
impl BusyGuard {
    fn new(app: &AppHandle, pid: String, cancel: CancellationToken, project_name: String) -> Self {
        let _ = app.emit(
            "agent://project-busy",
            serde_json::json!({ "id": pid, "busy": true }),
        );
        Self { app: app.clone(), pid, cancel, project_name }
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        let _ = self.app.emit(
            "agent://project-busy",
            serde_json::json!({ "id": self.pid, "busy": false }),
        );
        if self.cancel.is_cancelled() {
            return;
        }
        let focused = self.app
            .get_webview_window("main")
            .and_then(|w| w.is_focused().ok())
            .unwrap_or(false);
        if focused {
            return;
        }
        crate::notify::notify_work_done(&self.app, &self.project_name);
    }
}

struct DoneGuard {
    app: AppHandle,
    pid: String,
    cancel: CancellationToken,
    armed: bool,
}
impl DoneGuard {
    fn new(app: &AppHandle, pid: String, cancel: CancellationToken) -> Self {
        Self { app: app.clone(), pid, cancel, armed: true }
    }
}
impl Drop for DoneGuard {
    fn drop(&mut self) {
        if self.armed && !self.cancel.is_cancelled() {
            let _ = self.app.emit(
                "agent://done",
                serde_json::json!({ "project_id": self.pid }),
            );
        }
    }
}

struct IdleGuard {
    inner: Arc<Mutex<crate::state::Inner>>,
}

impl Drop for IdleGuard {
    fn drop(&mut self) {
        let inner = self.inner.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(BRIDGE_IDLE_SECS + 5)).await;
            AppState::close_idle(&inner, Duration::from_secs(BRIDGE_IDLE_SECS)).await;
        });
    }
}



#[cfg(test)]
mod tests {
    use super::AnswerStreamGate;

    #[test]
    fn normal_text_streams_immediately() {
        let mut g = AnswerStreamGate::new();
        assert_eq!(g.push("你好"), vec!["你好".to_string()]);
    }

    #[test]
    fn tool_call_is_never_emitted() {
        let mut g = AnswerStreamGate::new();
        let out = g.push("<tool_calls>\n<invoke name=\"browser\">");
        assert!(out.is_empty(), "工具标签不应流出: {out:?}");
        g.suppress_tool_buffer();
    }

    #[test]
    fn text_before_tool_call_is_emitted() {
        let mut g = AnswerStreamGate::new();
        let out = g.push("我先查看\n<tool_calls>");
        assert_eq!(out, vec!["我先查看\n".to_string()]);
    }

    #[test]
    fn tool_marker_split_across_deltas_is_buffered() {
        let mut g = AnswerStreamGate::new();
        let mut all = Vec::new();
        all.extend(g.push("我先查看"));
        all.extend(g.push("\n<to"));
        all.extend(g.push("ol_"));
        all.extend(g.push("calls>"));
        all.extend(g.push("\n<invoke name=\"browser\">"));
        let joined = all.join("");
        assert!(joined.contains("我先查看"), "正文应流出: {joined:?}");
        assert!(!joined.contains("<to"), "标签前缀不应流出: {joined:?}");
        assert!(!joined.contains("invoke"), "invoke 不应流出: {joined:?}");
    }

    #[test]
    fn false_positive_tool_prefix_is_flushed() {
        let mut g = AnswerStreamGate::new();
        let _ = g.push("解释 <to");
        let _ = g.push("day 这个词");
        let flushed = g.finish_text();
        assert_eq!(flushed.as_deref(), Some("解释 <today 这个词"));
    }

    #[test]
    fn corrupted_tool_call_is_buffered() {
        let mut g = AnswerStreamGate::new();
        let out = g.push("<tool_calls>\n<tool call name=\"invoke\" name=\"browser\">");
        assert!(out.is_empty(), "污染标签也不应流出: {out:?}");
        g.suppress_tool_buffer();
    }

    #[test]
    fn invoke_marker_split_across_deltas_is_buffered() {
        let mut g = AnswerStreamGate::new();
        let mut all = Vec::new();
        all.extend(g.push("看"));
        all.extend(g.push("<in"));
        all.extend(g.push("voke name=\"browser\">"));
        let joined = all.join("");
        assert!(joined.contains("看"));
        assert!(!joined.contains("<in"));
    }
}

async fn session_id_of(state: &AppState, project_id: &str) -> Option<String> {
    let inner = state.inner.lock().await;
    if let Some(id) = inner
        .running
        .get(project_id)
        .and_then(|c| c.session.session_id.clone())
    {
        return Some(id);
    }
    inner.projects.active_conversation_of(project_id)
}

#[derive(Clone)]
pub struct EmitCtx {
    pub app: AppHandle,
    pub project_id: String,
    pub sub_id: Option<String>,
    pub sub_name: Option<String>,
}

impl EmitCtx {
    pub fn main(app: &AppHandle, project_id: &str) -> Self {
        Self {
            app: app.clone(),
            project_id: project_id.to_string(),
            sub_id: None,
            sub_name: None,
        }
    }

    pub fn delta(&self, kind: &str, text: &str) {
        let mut v = serde_json::json!({
            "project_id": self.project_id,
            "kind": kind,
            "text": text,
        });
        if let Some(sid) = &self.sub_id {
            v["sub_id"] = serde_json::json!(sid);
            v["sub_name"] = serde_json::json!(self.sub_name.clone().unwrap_or_default());
        }
        let _ = self.app.emit("agent://delta", v);
    }

    pub fn tool(&self, payload: serde_json::Value) {
        let mut v = payload;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("project_id".into(), serde_json::json!(self.project_id));
            if let Some(sid) = &self.sub_id {
                obj.insert("sub_id".into(), serde_json::json!(sid));
                obj.insert("sub_name".into(), serde_json::json!(self.sub_name.clone().unwrap_or_default()));
            }
        }
        let _ = self.app.emit("agent://tool", v);
    }

    pub fn retry(&self, reason: &str) {
        let _ = self.app.emit(
            "agent://retry",
            serde_json::json!({ "project_id": self.project_id, "reason": reason }),
        );
    }
}

pub async fn run_turn(
    app: AppHandle,
    state: &AppState,
    project_id: String,
    model: Arc<dyn Model>,
    input: String,
    cancel: CancellationToken,
) -> Result<(), String> {
    let resolved_sid = session_id_of(state, &project_id).await;
    let mut session_id = match resolved_sid {
        Some(id) => id,
        None => {
            let allow_create = {
                let inner = state.inner.lock().await;
                inner
                    .projects
                    .project(&project_id)
                    .map(|p| p.conversations.is_empty())
                    .unwrap_or(false)
            };
            if !allow_create {
                return Err("当前项目已有对话，但没有可恢复的 session_id".into());
            }
            let id = model.new_session().await.map_err(|e| e.to_string())?;
            let mut inner = state.inner.lock().await;
            let ctx = inner.running.get_mut(&project_id).ok_or("运行上下文丢失")?;
            ctx.session.session_id = Some(id.clone());
            id
        }
    };
    let project_name = {
        let inner = state.inner.lock().await;
        inner.projects.project(&project_id).map(|p| p.name.clone()).unwrap_or_else(|| project_id.clone())
    };
    let _busy_guard = BusyGuard::new(&app, project_id.clone(), cancel.clone(), project_name);
    let _done_guard = DoneGuard::new(&app, project_id.clone(), cancel.clone());
    let _idle_guard = IdleGuard { inner: state.inner.clone() };
    
    let account = {
        let inner = state.inner.lock().await;
        inner.running.get(&project_id).map(|c| c.account.clone())
    }.unwrap_or_default();
    {
        let mut inner = state.inner.lock().await;
        let registered = inner
            .projects
            .project(&project_id)
            .map(|p| p.conversations.iter().any(|c| c.id == session_id))
            .unwrap_or(false);
        let mut registered_now = false;
        // 网页内核 new_session() 返回空串：此时不建本地会话（空 id 会变成"对话1"），
        // 待首轮拿到真实 UUID 后再补建（见下方 session_id.is_empty() 分支）。
        if !registered && !session_id.is_empty() {
            let title = inner.projects.next_conversation_title(&project_id);
            let _ = inner.projects.add_conversation(&project_id, &session_id, &title);
            let _ = inner.projects.save();
            registered_now = true;
        }
        drop(inner);
        if !session_id.is_empty() {
            let _ = crate::projects::append_message(&session_id, "user", &input);
        }
        if registered_now {
            let _ = app.emit("agent://projects-changed", ());
        }
    }

    let prompt_already_sent = {
        let inner = state.inner.lock().await;
        inner
            .running
            .get(&project_id)
            .map(|c| c.session.prompt_sent)
            .unwrap_or(true)
    };
    // 内存 flag 丢失（如应用重启后进入老对话）时，用服务端历史消息数判断：
    // 已有历史 => 系统提示词早已在该 session 中，本轮不再附带，省 token。
    let prompt_already_sent = if prompt_already_sent {
        true
    } else {
        model
            .message_count(&session_id)
            .await
            .unwrap_or(None)
            .map(|n| n > 0)
            .unwrap_or(false)
    };

    let remote_spec = {
        let inner = state.inner.lock().await;
        inner.projects.remote_of(&project_id)
    };

    // 远程项目的工作目录取远端 root；本地项目取 workdir_of。
    let workdir_now = if let Some(spec) = &remote_spec {
        spec.root.clone()
    } else {
        let inner = state.inner.lock().await;
        inner
            .projects
            .workdir_of(&project_id)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().to_string_lossy().to_string())
    };

    // 异步探测远端 OS（probe_remote 是同步的，is_win 需提前算好传入）。
    let is_win_remote = match &remote_spec {
        Some(spec) => crate::ide::backend::is_windows_remote(spec).await,
        None => false,
    };

    let mut next_prompt = {
        let mut inner = state.inner.lock().await;
        let ctx = inner.running.get_mut(&project_id).ok_or("上下文丢失")?;
        ctx.session.prompt_sent = true;
        let cwd_notice = if ctx.session.announced_workdir.as_deref() == Some(workdir_now.as_str()) {
            String::new()
        } else {
            ctx.session.announced_workdir = Some(workdir_now.clone());
            format!("[系统] 当前工作目录已切换为：{workdir_now}\n（execute_command 在该目录执行，后续以此为准。）\n\n")
        };
        let input_full = format!("{cwd_notice}{input}");
        if prompt_already_sent {
            input_full
        } else {
            format!(
                "{}\n\n{TOOLS_SECTION_GUI}\n\n{input_full}",
                system_prompt_with_remote(remote_spec.as_ref(), is_win_remote)
            )
        }
    };

    let ec = EmitCtx::main(&app, &project_id);

    let mut format_retries: u32 = 0;
    // 模型主动请求压缩：置位后在本轮顶部走压缩分支。
    let mut force_compact = false;
    // 上次注入 `[运行状态]` 时的消息数分桶（每 10 条注入一次）。
    let mut last_status_bucket: i64 = -1;
    // 上次注入的子 agent 状态文本（内容变化时立即注入）。
    let mut last_sub_status = String::new();
    // 子 agent 状态注入的轮数计数：全部空闲时每 10 轮注入一次，有运行中则每轮注入。
    let mut sub_status_rounds: u32 = 0;
    loop {
        {
            let n = model
                .message_count(&session_id)
                .await
                .unwrap_or(None)
                .unwrap_or(0);
            let compact_threshold = state
                .inner
                .lock()
                .await
                .projects
                .compact_of(&project_id);
            if n >= compact_threshold || force_compact {
                force_compact = false;
                let parent = {
                    state.inner.lock().await
                        .running.get(&project_id)
                        .and_then(|c| c.session.parent_message_id)
                };
                let mut report_req = CompletionReq::new(session_id.clone(), REPORT_PROMPT.to_string());
                report_req.parent_message_id = parent;
                if !state.throttle_account(&account, GLOBAL_REQUEST_GAP, &cancel).await {
                    return Ok(());
                }
                let report = {
                    let mut noop = |_: DeltaKind, _: &str| {};
                    match model.complete(&report_req, &mut noop).await {
                        Ok(r) => r.content,
                        Err(e) => format!("[交接报告生成失败: {e}]"),
                    }
                };
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let new_sid = model.new_session().await.map_err(|e| e.to_string())?;
                if new_sid.is_empty() {
                    // 网页内核无预建会话接口：空 session 不持久化，进入"待绑定"。
                    // 用空 session 发首轮让网页创建真实会话，拿到真实 UUID 后再补建本地会话。
                    let mut inner = state.inner.lock().await;
                    let ctx = inner.running.get_mut(&project_id).ok_or("上下文丢失")?;
                    ctx.session.session_id = None;
                    ctx.session.parent_message_id = None;
                    ctx.session.prompt_sent = false;
                    ctx.session.pending_handoff = true;
                    ctx.session.pending_report = Some(report.clone());
                    session_id = String::new();
                } else {
                    let mut inner = state.inner.lock().await;
                    let title = inner.projects.next_conversation_title(&project_id);
                    let _ = inner.projects.add_conversation(&project_id, &new_sid, &title);
                    let _ = inner.projects.save();
                    let ctx = inner.running.get_mut(&project_id).ok_or("上下文丢失")?;
                    ctx.session.session_id = Some(new_sid.clone());
                    ctx.session.parent_message_id = None;
                    ctx.session.prompt_sent = true;
                    ctx.session.pending_handoff = false;
                    ctx.session.pending_report = None;
                    session_id = new_sid;
                }
                let _ = app.emit("agent://projects-changed", ());
                let _ = app.emit("agent://handoff", serde_json::json!({ "project_id": project_id, "count": n }));
                next_prompt = format!("{}\n\n{TOOLS_SECTION_GUI}\n\n{HANDOFF_INTRO}{report}\n===== 交接报告结束 =====\n\n{input}", system_prompt_with_remote(remote_spec.as_ref(), is_win_remote));
            }
        }

        if cancel.is_cancelled() {
            return Ok(());
        }

        send_delay(&cancel).await;
        if cancel.is_cancelled() {
            return Ok(());
        }

        let (parent, thinking, search) = {
            let mut inner = state.inner.lock().await;
            let ctx = inner.running.get_mut(&project_id).ok_or("上下文丢失")?;
            (
                ctx.session.parent_message_id,
                ctx.session.thinking,
                ctx.session.search,
            )
        };

        // 子 agent 状态注入策略：内容变化立即注入；否则有运行中每轮注入，
        // 全部空闲时每 10 轮注入一次（避免无意义刷屏）。
        {
            let (sub_status, running) = {
                let inner = state.inner.lock().await;
                let subs = inner.projects.sub_agents_of(&project_id);
                if subs.is_empty() {
                    (String::new(), 0usize)
                } else {
                    let mut parts = Vec::new();
                    let mut running = 0usize;
                    for sa in &subs {
                        let st = match inner.sub_runs.get(&sa.id).map(|r| &r.status) {
                            Some(crate::state::SubRunStatus::Running)
                            | Some(crate::state::SubRunStatus::Starting) => { running += 1; "运行中" }
                            Some(crate::state::SubRunStatus::Succeeded) => "完成",
                            Some(crate::state::SubRunStatus::Failed) => "失败",
                            Some(crate::state::SubRunStatus::Cancelled) => "已取消",
                            Some(crate::state::SubRunStatus::Closed) => "已关闭",
                            None => "空闲",
                        };
                        parts.push(format!("{}（{}）", sa.name, st));
                    }
                    (
                        format!(
                            "\n[系统] 本项目子agent（同名再次派发=续跑其对话；有空闲的优先复用，不要为一句话小事新建）：共 {}（运行 {}）—— {}\n",
                            subs.len(),
                            running,
                            parts.join("、")
                        ),
                        running,
                    )
                }
            };
            sub_status_rounds = sub_status_rounds.wrapping_add(1);
            if !sub_status.is_empty() {
                let changed = sub_status != last_sub_status;
                let due = running > 0 || sub_status_rounds >= 10;
                if changed || due {
                    next_prompt.push_str(&sub_status);
                    last_sub_status = sub_status;
                    sub_status_rounds = 0;
                }
            }
        }

        let mut req = CompletionReq::new(session_id.clone(), next_prompt.clone());
        req.parent_message_id = parent;
        if thinking {
            req.model_type = "expert".into();
            req.thinking_enabled = true;
        }
        req.search_enabled = search;

        let app2 = app.clone();
        let pid = project_id.clone();
        let mut thinking_buf = String::new();
        let mut thinking_sent = 0usize;
        let answer_gate = std::sync::Arc::new(std::sync::Mutex::new(AnswerStreamGate::new()));
        let gate_for_delta = answer_gate.clone();

        let pending = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let pending_for_ticker = pending.clone();
        let pending_for_closure = pending.clone();
        let app_for_ticker = app2.clone();
        let pid_for_ticker = pid.clone();
        let flush_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                ticker.tick().await;
                let chunk = {
                    let mut g = pending_for_ticker.lock().unwrap();
                    if g.is_empty() { continue; }
                    std::mem::take(&mut *g)
                };
                let _ = app_for_ticker.emit(
                    "agent://delta",
                    serde_json::json!({ "project_id": pid_for_ticker, "kind": "answer", "text": chunk }),
                );
            }
        });

        let mut on_delta = move |kind: DeltaKind, text: &str| {
            if kind == DeltaKind::Thinking {
                thinking_buf.push_str(text);
                if thinking_buf.len() > thinking_sent {
                    let chunk = thinking_buf[thinking_sent..].to_string();
                    thinking_sent = thinking_buf.len();
                    let _ = app2.emit(
                        "agent://delta",
                        serde_json::json!({ "project_id": pid, "kind": "thinking", "text": chunk }),
                    );
                }
                return;
            }

            let chunks = gate_for_delta.lock().unwrap().push(text);
            let mut buf = pending_for_closure.lock().unwrap();
            for chunk in chunks {
                buf.push_str(&chunk);
            }
        };

        if !state.throttle_account(&account, GLOBAL_REQUEST_GAP, &cancel).await {
            return Ok(());
        }
        let mut reply = tokio::select! {
            r = model.complete(&req, &mut on_delta) => r.map_err(|e| e.to_string())?,
            _ = cancel.cancelled() => {
                flush_handle.abort();
                let _ = flush_handle.await;
                return Ok(());
            }
        };
        flush_handle.abort();
        let _ = flush_handle.await;
        {
            let tail = std::mem::take(&mut *pending.lock().unwrap());
            if !tail.is_empty() {
                let _ = app.emit(
                    "agent://delta",
                    serde_json::json!({ "project_id": project_id, "kind": "answer", "text": tail }),
                );
            }
        }
        if reply.content.trim().is_empty() {
            ec.retry("empty");
            let mut noop = |_: DeltaKind, _: &str| {};
            if !state.throttle_account(&account, GLOBAL_REQUEST_GAP, &cancel).await {
                return Ok(());
            }
            reply = tokio::select! {
                r = model.complete(&req, &mut noop) => r.map_err(|e| e.to_string())?,
                _ = cancel.cancelled() => return Ok(()),
            };
        }

        if let Some(real_sid) = &reply.session_id {
            if !real_sid.is_empty() && real_sid != &session_id {
                let mut inner = state.inner.lock().await;
                if session_id.is_empty() {
                    // 压缩交接后首次拿到真实 UUID：此时才补建本地会话（避免持久化空 session）。
                    let title = inner.projects.next_conversation_title(&project_id);
                    let _ = inner.projects.add_conversation(&project_id, real_sid, &title);
                    if let Some(ctx) = inner.running.get_mut(&project_id) {
                        ctx.session.session_id = Some(real_sid.clone());
                        ctx.session.pending_handoff = false;
                        ctx.session.pending_report = None;
                    }
                    inner.projects.set_active_conversation_of(&project_id, Some(real_sid.as_str()));
                    let _ = inner.projects.save();
                    session_id = real_sid.clone();
                    let _ = app.emit("agent://projects-changed", ());
                } else {
                    let _ = crate::projects::rename_conversation_file(&session_id, real_sid);
                    if let Some(p) = inner.projects.project_mut(&project_id) {
                        if let Some(c) = p.conversations.iter_mut().find(|c| c.id == session_id) {
                            c.id = real_sid.clone();
                        }
                    }
                    if let Some(ctx) = inner.running.get_mut(&project_id) {
                        if ctx.session.session_id.as_deref() == Some(session_id.as_str()) {
                            ctx.session.session_id = Some(real_sid.clone());
                        }
                    }
                    // 同步 active_by_project，确保重启后 open_project 能取到真实 UUID
                    inner.projects.set_active_conversation_of(&project_id, Some(real_sid.as_str()));
                    let _ = inner.projects.save();
                    session_id = real_sid.clone();
                    let _ = app.emit("agent://projects-changed", ());
                }
            }
        }
        if reply.message_id.is_some() {
            let mut inner = state.inner.lock().await;
            if let Some(ctx) = inner.running.get_mut(&project_id) {
                ctx.session.parent_message_id = reply.message_id;
            }
            if let Some(p) = inner.projects.project_mut(&project_id) {
                if let Some(c) = p.conversations.iter_mut().find(|c| c.id == session_id) {
                    c.last_parent_id = reply.message_id;
                }
            }
        }

        {
            refresh_message_count(&app, state, &model, &project_id, &session_id).await;
        }

        let __po = parser::parse(&reply.content);
        match __po {
            ParseOutcome::Text(text) => {
                // 先把 gate 中可能缓冲的尾部 flush（避免以工具前缀开头的正文被吞）。
                {
                    let tail = answer_gate.lock().unwrap().finish_text();
                    if let Some(t) = tail {
                        let _ = app.emit(
                            "agent://delta",
                            serde_json::json!({ "project_id": project_id, "kind": "answer", "text": t }),
                        );
                    }
                }
                let cleaned = text.trim().to_string();
                if !cleaned.is_empty() {
                    let _ = crate::projects::append_message(&session_id, "assistant", &cleaned);
                }
                refresh_message_count(&app, state, &model, &project_id, &session_id).await;
                // 模型输出纯文本（无工具调用）= 本轮回答结束。不再有完成信号/重试/限流判定。
                return Ok(());
            }
            ParseOutcome::Calls(calls) => {
                // 出现合法工具调用 = 格式恢复正常，重置"连续格式错误"计数。
                format_retries = 0;
                // 分离"主动请求压缩"：仅当其独占本轮且无普通工具时才生效。
                let (compact_calls, tool_calls): (Vec<_>, Vec<_>) =
                    calls.into_iter().partition(|c| c.name == "request_context_compaction");
                // 主动压缩：独占本轮时触发；防滥用——本会话至少 10 条消息，否则忽略。
                let want_compact = !compact_calls.is_empty() && tool_calls.is_empty();
                let calls = tool_calls;
                // 含工具调用的回复：只反馈工具状态，不打印伴随的正文。
                // 丢弃 gate 中缓冲的文本（工具标记之前的说明文字）。
                answer_gate.lock().unwrap().finish_text();
                let mut results = String::new();
                let mut call_id: u64 = 0;
                for call in calls {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    call_id += 1;
                    // 工具卡标题 / 历史摘要：文件工具用 file_path，命令工具用 command。
                    let command = match call.name.as_str() {
                        "read_file" | "write_file" | "edit_file" => call
                            .params
                            .get("file_path")
                            .or_else(|| call.params.get("path"))
                            .cloned()
                            .unwrap_or_default(),
                        _ => call
                            .params
                            .get("command")
                            .or_else(|| call.params.get("cmd"))
                            .cloned()
                            .unwrap_or_default(),
                    };
                    ec.tool(serde_json::json!({
                        "phase": "call",
                        "call_id": call_id,
                        "name": call.name,
                        "command": command,
                    }));
                    let started = std::time::Instant::now();
                    let output = dispatch_tool(&app, state, &model, &project_id, &call, &cancel).await;
                    let ms = started.elapsed().as_millis() as u64;
                    let (ok, body) = match output {
                        Ok(out) => (true, out),
                        Err(err) => (false, err),
                    };
                    if call.name != "execute_command" && !body.is_empty() {
                        ec.tool(serde_json::json!({
                            "phase": "output",
                            "call_id": call_id,
                            "name": call.name,
                            "output": &body,
                        }));
                    }
                    ec.tool(serde_json::json!({
                        "phase": "done",
                        "call_id": call_id,
                        "name": call.name,
                        "ms": ms,
                        "error": !ok,
                    }));
                    let _ = crate::projects::append_tool(&session_id, &call.name, &command, &body, ms, !ok);
                    results.push_str(&format!("<tool_result name=\"{}\">\n{}\n</tool_result>\n", call.name, body));
                }
                let drained = {
                    let mut inner = state.inner.lock().await;
                    inner.pending_sub_results.remove(&project_id).unwrap_or_default()
                };
                let has_pending = !drained.is_empty();
                for r in drained {
                    results.push_str(&crate::subagent::format_sub_result(&r.name, r.ok, &r.text));
                }
                if has_pending {
                    results.push_str(&format!("\n{}\n", SUB_REVIEW_PROMPT));
                }
                if want_compact {
                    // 防滥用：本会话消息数过少（<10）时忽略请求，避免频繁重建会话。
                    let n = model.message_count(&session_id).await
                        .unwrap_or(None).unwrap_or(0);
                    if n < 10 {
                        let _ = app.emit("agent://delta", serde_json::json!({
                            "project_id": project_id, "kind": "answer",
                            "text": "\n[系统] 忽略本次上下文压缩请求：当前会话消息过少，无需压缩。"
                        }));
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
                        results.push_str(&status_block(state, &project_id, &model, &session_id).await);
                    }
                }
                next_prompt = results;
                continue;
            }
            ParseOutcome::Malformed { raw, reason } => {
                let _ = raw;
                let _ = app.emit(
                    "agent://format-error",
                    serde_json::json!({ "project_id": project_id, "reason": reason }),
                );
                format_retries += 1;
                if format_retries > 3 {
                    let account = {
                        let inner = state.inner.lock().await;
                        inner.projects.project(&project_id).map(|p| p.account.clone()).unwrap_or_default()
                    };
                    let _ = app.emit("agent://error", crate::commands::error_payload(
                        &project_id, "模型连续多次输出格式错误，已停止本轮", Some(&account), None,
                    ));
                    return Ok(());
                }
                next_prompt = format!("{FORMAT_ERROR_PROMPT}\n\n（解析失败原因：{reason}）");
                continue;
            }
            ParseOutcome::DsmlMalformed { raw, reason, marker_count } => {
                let _ = raw;
                let _ = app.emit(
                    "agent://format-error",
                    serde_json::json!({
                        "project_id": project_id,
                        "reason": reason,
                        "marker_count": marker_count,
                    }),
                );
                format_retries += 1;
                if format_retries > 3 {
                    let account = {
                        let inner = state.inner.lock().await;
                        inner.projects.project(&project_id).map(|p| p.account.clone()).unwrap_or_default()
                    };
                    let _ = app.emit("agent://error", crate::commands::error_payload(
                        &project_id, "模型连续多次输出格式错误，已停止本轮", Some(&account), None,
                    ));
                    return Ok(());
                }
                next_prompt = format!(
                    "{DSML_ERROR_PROMPT}\n\n（检测到 {marker_count} 个工具标记，解析失败原因：{reason}）"
                );
                continue;
            }
        }
    }
}

/// 上下文状态块：当前消息数 + 压缩阈值，供模型自我判断是否需精简/压缩。
pub(crate) async fn status_block(
    state: &AppState,
    project_id: &str,
    model: &Arc<dyn Model>,
    session_id: &str,
) -> String {
    let threshold = state.inner.lock().await.projects.compact_of(project_id);
    let used = model
        .message_count(session_id)
        .await
        .unwrap_or(None)
        .unwrap_or(0);
    format!(
        "[运行状态] 当前上下文消息数 {used}（压缩阈值 {threshold}）。\
        若接近阈值请精简输出，必要时调用 request_context_compaction。\n"
    )
}

pub(crate) async fn refresh_message_count(
    app: &AppHandle,
    state: &AppState,
    model: &Arc<dyn Model>,
    project_id: &str,
    session_id: &str,
) {
    let count = model
        .message_count(session_id)
        .await
        .unwrap_or(None)
        .unwrap_or(0);
    let mut inner = state.inner.lock().await;
    inner.projects.touch_conversation(project_id, session_id, count, None);
    let _ = inner.projects.save();
    drop(inner);
    let _ = app.emit("agent://projects-changed", ());
}

pub(crate) fn resolve_tool_path(
    params: &std::collections::HashMap<String, String>,
    cwd: &std::path::Path,
) -> Result<std::path::PathBuf, ()> {
    let raw = params
        .get("file_path")
        .or_else(|| params.get("path"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or(())?;
    let p = std::path::Path::new(raw);
    Ok(if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) })
}

/// 主 agent 工具分派：含子 agent 三工具；子 agent 循环只走 `execute_tool`（不含此三工具），
/// 以避免 `run_subagent → agent_loop → execute_tool → run_sub_agent_tool` 的异步递归环。
pub(crate) async fn dispatch_tool(
    app: &AppHandle,
    state: &AppState,
    model: &Arc<dyn Model>,
    project_id: &str,
    call: &deepseek_agent::agent::parser::ToolCall,
    cancel: &CancellationToken,
) -> Result<String, String> {
    if call.name.as_str() == "sub_agent" {
        return run_sub_agent_tool(app, state, project_id, call, cancel).await;
    }
    execute_tool(app, state, model, project_id, call, cancel).await
}

pub(crate) async fn execute_tool(
    app: &AppHandle,
    state: &AppState,
    model: &Arc<dyn Model>,
    project_id: &str,
    call: &deepseek_agent::agent::parser::ToolCall,
    cancel: &CancellationToken,
) -> Result<String, String> {
    if call.name == "upload_file" {
        return upload_file_tool(app, state, model, project_id, call, cancel).await;
    }
    if call.name == "browser" {
        let browser = deepseek_agent::agent::browser::Browser::with_key(project_id);
        let app2 = app.clone();
        let pid = project_id.to_string();
        let mut on_output = move |line: &str| {
            if let Some(rest) = line.strip_prefix('\u{1}') {
                if let Some(img) = rest.strip_prefix("IMAGE\u{1}") {
                    let _ = app2.emit(
                        "agent://tool",
                        serde_json::json!({
                            "project_id": pid,
                            "phase": "image",
                            "name": "browser",
                            "image": img,
                        }),
                    );
                }
            }
        };
        return browser
            .call(&call.params, false, &mut on_output)
            .await
            .map_err(|e| e.to_string());
    }

    let tools = { state.inner.lock().await.tools.clone() };
    let tool = tools.get(&call.name).ok_or_else(|| format!("未知工具：{}", call.name))?;

    let needs_approval = matches!(
        call.name.as_str(),
        "execute_command" | "write_file" | "edit_file"
    );
    if needs_approval {
        let free_now = {
            let inner = state.inner.lock().await;
            inner.running.get(project_id).map(|c| c.session.free_mode).unwrap_or(false)
        };
        if !free_now {
            let (id, rx) = {
                let mut inner = state.inner.lock().await;
                let id = inner.next_approval_id;
                inner.next_approval_id += 1;
                let (tx, rx) = tokio::sync::oneshot::channel();
                inner.approvals.insert(id, (project_id.to_string(), tx));
                (id, rx)
            };
            let _ = app.emit(
                "agent://approval",
                serde_json::json!({ "id": id, "project_id": project_id }),
            );
            let allow = tokio::select! {
                r = rx => r.unwrap_or(false),
                _ = cancel.cancelled() => {
                    state.inner.lock().await.approvals.remove(&id);
                    return Err("已取消".into());
                }
            };
            if !allow {
                return Ok("用户拒绝执行该命令。".into());
            }
        }
    }

    let app2 = app.clone();
    let pid = project_id.to_string();
    let tool_name = call.name.clone();
    let mut on_output = move |line: &str| {
        if let Some(rest) = line.strip_prefix('\u{1}') {
            if let Some(img) = rest.strip_prefix("IMAGE\u{1}") {
                let _ = app2.emit(
                    "agent://tool",
                    serde_json::json!({
                        "project_id": pid,
                        "phase": "image",
                        "name": tool_name,
                        "image": img,
                    }),
                );
                return;
            }
        }
        let _ = app2.emit(
            "agent://tool",
            serde_json::json!({
                "project_id": pid,
                "phase": "output",
                "name": tool_name,
                "output": format!("{line}\n")
            }),
        );
    };
    if matches!(
        call.name.as_str(),
        "execute_command" | "read_file" | "write_file" | "edit_file"
    ) {
        let (workdir, restrict, remote_spec) = {
            let inner = state.inner.lock().await;
            (
                inner.projects.workdir_of(project_id),
                inner.projects.restrict_of(project_id),
                inner.projects.remote_of(project_id),
            )
        };
        let cwd = workdir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
        return match call.name.as_str() {
            "execute_command" => {
                if let Some(spec) = &remote_spec {
                    crate::tools::remote::exec_remote(spec, &call.params, &mut on_output)
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    let runner =
                        crate::tools::ExecuteCommand::new(cwd.clone()).with_cancel(cancel.clone());
                    runner
                        .call(&call.params, false, &mut on_output)
                        .await
                        .map_err(|e| e.to_string())
                }
            }
            "read_file" => crate::tools::ReadFile::new(cwd)
                .with_restrict(restrict)
                .call(&call.params, false, &mut on_output)
                .await
                .map_err(|e| e.to_string()),
            "write_file" | "edit_file" => {
                let target = resolve_tool_path(&call.params, &cwd);
                let old = target.as_ref().ok()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default();
                if let Ok(p) = &target {
                    let mut inner = state.inner.lock().await;
                    if let Some(w) = inner.ide_watcher.as_mut() {
                        w.suppress_path(p);
                    }
                }
                let is_write = call.name == "write_file";
                let tool_out = if is_write {
                    crate::tools::WriteFile::new(cwd.clone()).with_restrict(restrict)
                        .call(&call.params, false, &mut on_output).await
                } else {
                    crate::tools::EditFile::new(cwd.clone()).with_restrict(restrict)
                        .call(&call.params, false, &mut on_output).await
                };
                let out = tool_out.map_err(|e| e.to_string());
                if out.is_ok() {
                    if let Ok(p) = &target {
                        let new = std::fs::read_to_string(p).unwrap_or_default();
                        let rel = p.strip_prefix(&cwd)
                            .map(|r| r.to_string_lossy().replace('\\', "/"))
                            .unwrap_or_default();
                        if !rel.is_empty() {
                            let win_label = format!("ide-{project_id}");
                            if let Some(win) = app.get_webview_window(&win_label) {
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64).unwrap_or(0);
                                let _ = win.emit("ide://agent-diff",
                                    crate::ide::watch::AgentDiffPayload {
                                        project_id: project_id.to_string(),
                                        rel, old_content: old, new_content: new,
                                        tool_call_id: call.raw.chars().take(40).collect(),
                                        timestamp: ts,
                                    });
                            }
                        }
                    }
                }
                out
            }
            _ => unreachable!(),
        };
    }
    tool.call(&call.params, false, &mut on_output)
        .await
        .map_err(|e| e.to_string())
}

async fn run_sub_agent_tool(
    app: &AppHandle,
    state: &AppState,
    project_id: &str,
    call: &deepseek_agent::agent::parser::ToolCall,
    _cancel: &CancellationToken,
) -> Result<String, String> {
    match call.name.as_str() {
        "sub_agent" => {
            let name = call.params.get("name").map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or("sub_agent: 缺少 name（子 agent 名字/职责）")?;
            let task = call.params.get("task").cloned().unwrap_or_default();
            if task.trim().is_empty() {
                return Err("sub_agent: 缺少 task".into());
            }
            let agent_type = call.params.get("agent_type").cloned().unwrap_or_else(|| "general".into());

            {
                let inner = state.inner.lock().await;
                let limit = inner.projects.max_sub_of(&project_id) as usize;
                let running = inner.sub_runs.values()
                    .filter(|r| r.project_id == project_id
                        && matches!(r.status, crate::state::SubRunStatus::Running | crate::state::SubRunStatus::Starting))
                    .count();
                if running >= limit {
                    return Ok(format!("[子agent] 当前已有 {limit} 个子 agent 在运行（并发上限），请稍后再派。").into());
                }
            }

            let spec = crate::subagent::SubAgentSpec {
                project_id: project_id.to_string(),
                name: name.clone(),
                task,
                agent_type: agent_type.clone(),
            };
            let disp_name = name.clone();
            let disp_type = agent_type.clone();
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::subagent::run_subagent(app2, spec).await;
            });
            Ok(format!(
                "[子agent] 已派发子 agent「{}」（职责：{}），它在后台独立运行。完成后结果会自动送达；期间你可以继续其它任务或结束本轮。",
                disp_name, disp_type
            ))
        }
        _ => Err(format!("未知子 agent 工具：{}", call.name)),
    }
}

/// `upload_file` 工具：模仿 GUI 手动上传链路（commands::upload_files）。
async fn upload_file_tool(
    app: &AppHandle,
    state: &AppState,
    model: &Arc<dyn Model>,
    project_id: &str,
    call: &deepseek_agent::agent::parser::ToolCall,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let raw = call
        .params
        .get("paths")
        .cloned()
        .unwrap_or_default();
    let paths: Vec<String> = raw
        .split(['\n', ','])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if paths.is_empty() {
        return Err("upload_file: <paths> 为空".into());
    }
    if paths.len() > 50 {
        return Err(format!("upload_file: 一次最多上传 50 个文件，当前 {}", paths.len()));
    }

    let mut lines = Vec::new();
    let mut valid: Vec<String> = Vec::new();
    for p in &paths {
        let path = std::path::Path::new(p);
        if !path.exists() {
            lines.push(format!("[!] 文件不存在: {p}"));
            continue;
        }
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > 100 * 1024 * 1024 {
            lines.push(format!("[!] 文件超过 100MB 上限: {p}"));
            continue;
        }
        valid.push(p.clone());
    }
    if valid.is_empty() {
        let joined = lines.join("\n");
        let _ = app.emit(
            "agent://tool",
            serde_json::json!({
                "project_id": project_id,
                "phase": "output",
                "name": "upload_file",
                "output": &joined,
            }),
        );
        return Ok(joined);
    }

    let (sid, pid) = {
        let inner = state.inner.lock().await;
        let pid = inner
            .projects
            .project(project_id)
            .ok_or_else(|| "未选择项目".to_string())?
            .id
            .clone();
        let sid = inner
            .running
            .get(&pid)
            .and_then(|c| c.session.session_id.clone())
            .or_else(|| inner.projects.active_conversation_of(&pid));
        (sid, pid)
    };

    let _ = state.ensure_run_ctx(&pid).await;

    let upload_fut = model.upload_files(sid.as_deref(), &valid);
    let n = tokio::select! {
        r = upload_fut => r.map_err(|e| format!("上传失败：{e}"))?,
        _ = cancel.cancelled() => return Err("已取消".into()),
    };
    for p in &valid {
        lines.push(format!("[✓] 已上传: {p}"));
    }
    lines.push(format!("共上传 {n} 个文件。"));

    {
        let mut inner = state.inner.lock().await;
        if let Some(ctx) = inner.running.get_mut(&pid) {
            for p in &valid {
                let name = std::path::Path::new(p)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string();
                let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as i64;
                let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
                let is_image =
                    ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"].contains(&ext.as_str());
                ctx.session.pending_files.push(crate::state::PendingFile {
                    id: String::new(),
                    name,
                    size,
                    is_image,
                    path: p.clone(),
                });
            }
        }
    }
    let _ = app.emit("agent://files-changed", &pid);
    let joined = lines.join("\n");
    let _ = app.emit(
        "agent://tool",
        serde_json::json!({
            "project_id": project_id,
            "phase": "output",
            "name": "upload_file",
            "output": &joined,
        }),
    );
    Ok(joined)
}