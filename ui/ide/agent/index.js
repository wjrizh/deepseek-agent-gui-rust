// Agent 面板入口：装配 DOM、绑定输入/发送/停止、拖拽、事件订阅。
// 与终端面板并列于底部区域，Ctrl+J 切换。

import { createStore } from "./store.js";
import { render, updateControls } from "./render.js";
import { installAgentEvents } from "./events.js";

const invoke = window.__TAURI__.core.invoke;

let store = null;
let rootEl = null;
let inputEl = null;
let sendBtn = null;
let stopBtn = null;
let uninstallEvents = null;
let onBusyCb = null;

function ensureBuilt() {
  if (rootEl) return;
  rootEl = document.getElementById("agent-body");
  inputEl = document.getElementById("agent-input");
  sendBtn = document.getElementById("agent-send");
  stopBtn = document.getElementById("agent-stop");
}

function renderStatusBar() {
  const bar = document.getElementById("agent-status");
  if (!bar || !store) return;
  const c = store.subCountText();
  const subText = c.total ? "子agent " + c.running + "/" + c.total : "";
  bar.textContent = subText;
}

const subHandlers = {
  stop(s) {
    invoke("stop_sub_agent", { projectId: store.projectId, subId: s.id })
      .catch((e) => store.error(String(e)));
  },
  retry(s) {
    invoke("retry_sub_agent", { projectId: store.projectId, subId: s.id })
      .catch((e) => store.error(String(e)));
  },
  cont(s) {
    const task = window.prompt("继续该子 agent 的新任务内容：");
    if (!task || !task.trim()) return;
    invoke("continue_sub_agent", { projectId: store.projectId, subId: s.id, task })
      .catch((e) => store.error(String(e)));
  },
  retryNew(s) {
    invoke("retry_sub_agent_new_conv", { projectId: store.projectId, subId: s.id })
      .catch((e) => store.error(String(e)));
  },
};

function rerender() {
  if (!rootEl || !store) return;
  render(rootEl, store, subHandlers);
  updateControls(store, { sendBtn, stopBtn, input: inputEl });
  renderStatusBar();
}

function doSend() {
  if (!store || store.running) return;
  const text = inputEl.value.trim();
  if (!text) return;
  store.addUser(text, []);
  store.setRunning(true);
  inputEl.value = "";
  invoke("send_message", { projectId: store.projectId, text })
    .catch((e) => { store.error(String(e)); });
}

export function initAgent({ projectId, onBusy }) {
  ensureBuilt();
  if (!rootEl) return;
  // 项目切换：卸载旧监听 + 重建 store（避免旧项目事件污染）。
  if (store && store.projectId !== projectId) {
    if (uninstallEvents) { uninstallEvents(); uninstallEvents = null; }
    store = null;
  }
  if (store) return; // 同项目已初始化，避免重复绑定
  store = createStore(projectId);
  store.subscribe(rerender);
  uninstallEvents = installAgentEvents(store, onBusy);
  onBusyCb = onBusy;

  sendBtn?.addEventListener("click", doSend);
  stopBtn?.addEventListener("click", () => {
    invoke("interrupt", { projectId: store.projectId }).catch(() => {});
  });
  inputEl?.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); doSend(); }
  });
}

export function disposeAgent() {
  if (uninstallEvents) { uninstallEvents(); uninstallEvents = null; }
  store = null; rootEl = null; inputEl = null; sendBtn = null; stopBtn = null;
}

export function isAgentReady() { return !!store; }