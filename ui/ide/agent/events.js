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

  on("agent://error", (p) => {
    if (p.project_id !== pid) return;
    store.error(p.message);
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

  on("agent://sub-start", (p) => { if (p.project_id === pid) store.subStart(p.sub_id, p.name, p.duty); });
  on("agent://sub-done", (p) => { if (p.project_id === pid) store.subDone(p.sub_id, p.name, p.ok, p.reason, p.result); });
  on("agent://sub-closed", (p) => { if (p.project_id === pid) store.subClose(p.sub_id, p.name); });

  return () => { for (const un of unlisteners) { try { un(); } catch {} } };
}