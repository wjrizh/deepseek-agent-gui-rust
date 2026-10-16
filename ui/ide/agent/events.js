// 订阅后端 agent://* 全局事件，按 project_id 过滤后写入 store。
// 与主窗口（app.js）监听同名事件，实现跨窗口同步。

export function installAgentEvents(store, onBusy) {
  const event = window.__TAURI__ && window.__TAURI__.event;
  if (!event) return () => {};
  const pid = store.projectId;
  const unlisteners = [];

  const on = (name, fn) => {
    event.listen(name, (e) => fn(e.payload || {})).then((un) => unlisteners.push(un));
  };

  on("agent://delta", (p) => {
    if (p.project_id !== pid) return;
    if (p.kind === "thinking") return;
    if (p.sub_id) store.subDelta(p.sub_id, p.sub_name, p.text || "");
    else store.appendAssistant(p.text || "");
  });

  on("agent://tool", (p) => {
    if (p.project_id !== pid) return;
    if (p.sub_id) {
      store.subTool(p.sub_id, p.sub_name, p.phase, p.call_id, p.command, p.output, p.ms, p.error);
      return;
    }
    if (p.phase === "call") store.beginTool(p.call_id, p.name, p.command);
    else if (p.phase === "output") store.toolOutput(p.call_id, p.name, p.output);
    else if (p.phase === "done") store.endTool(p.call_id, p.ms, p.error);
  });

  on("agent://done", (p) => {
    if (p.project_id !== pid) return;
    store.finish();
  });

  on("agent://ask", (p) => {
    if (p.project_id !== pid) return;
    store.appendAssistant(p.question || "");
    store.setRunning(false);
  });

  on("agent://error", (p) => {
    if (p.project_id !== pid) return;
    store.error(p.user_message || p.message);
  });

  on("agent://approval", (p) => {
    if (p.project_id !== pid) return;
    store.addNotice("（有工具等待审批，请在主窗口处理）");
  });

  on("agent://project-busy", (p) => {
    // 该事件用 id 字段标识项目。
    if (p.id !== pid) return;
    store.setRunning(!!p.busy);
    if (onBusy) onBusy(!!p.busy);
  });

  on("agent://rate-limit", (p) => {
    if (p.project_id !== pid) return;
    store.setRateLimit(!!p.suspended, p.next_retry_at);
    if (p.suspended) store.addNotice("⚠ 检测到限流，任务已挂起，将在倒计时结束后自动重试。点停止可中断等待并接管。");
    else store.addNotice("限流等待已结束，继续执行。");
  });

  on("agent://sub-start", (p) => { if (p.project_id === pid) store.subStart(p.sub_id, p.name, p.duty, p.run_generation); });
  on("agent://sub-done", (p) => { if (p.project_id === pid) store.subDone(p.sub_id, p.name, p.ok, p.reason, p.result, p.run_generation); });
  on("agent://sub-closed", (p) => { if (p.project_id === pid) store.subClose(p.sub_id, p.name, p.run_generation); });
  on("agent://sub-format-error", (p) => { if (p.project_id === pid) store.subDone(p.sub_id, p.sub_name || p.name, false, "format", "", p.run_generation); });
  on("agent://sub-handoff", (p) => { if (p.project_id === pid) store.subStart(p.sub_id, p.name || p.sub_name, p.duty || "", p.run_generation); });

  // 从后端运行态快照初始化：主 turn 状态 + 子 agent 列表（与 sub-* 事件同一状态集）。
  const core = window.__TAURI__ && window.__TAURI__.core;
  if (core && core.invoke) {
    core.invoke("get_project_runtime", { projectId: pid }).then((rt) => {
      if (!rt) return;
      if (rt.turn_active && rt.busy) store.setRunning(true);
      if (rt.rate_limited_suspended) store.setRateLimit(true, rt.next_retry_at);
      store.subSync(rt.sub_runs || []);
    }).catch(() => {});
  }

  return () => { for (const un of unlisteners) { try { un(); } catch {} } };
}