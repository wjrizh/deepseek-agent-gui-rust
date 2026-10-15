// 底部面板：终端 / 对话 两个 tab 的切换、显隐、快捷键、拖拽调高。
// 终端内部逻辑在 term.js；对话逻辑在 agent/index.js。

import { ensureTerminal, fitTerminal } from "./term.js";
import { initAgent, isAgentReady } from "./agent/index.js";

let panel, tabTerm, tabAgent, termPane, agentPane;
let current = "term"; // "term" | "agent"
let getCwd = () => "";
let onBusy = null;
let initialized = false;

function applyTab() {
  const isTerm = current === "term";
  tabTerm?.classList.toggle("active", isTerm);
  tabAgent?.classList.toggle("active", !isTerm);
  termPane?.classList.toggle("hidden", !isTerm);
  agentPane?.classList.toggle("hidden", isTerm);
  if (isTerm) fitTerminal();
}

export function isBottomVisible() {
  return panel && !panel.classList.contains("hidden");
}

export function showBottom(which) {
  if (!panel) return;
  panel.classList.remove("hidden");
  void panel.offsetHeight; // 强制 reflow，确保尺寸就绪
  if (which) current = which;
  if (current === "term") ensureTerminal();
  else if (current === "agent") ensureAgent();
  applyTab();
}

export function hideBottom() {
  if (panel) panel.classList.add("hidden");
}

export function toggleBottom(which) {
  if (isBottomVisible() && current === which) { hideBottom(); return; }
  showBottom(which);
}

export function initBottom({ projectId, cwdProvider }) {
  panel = document.getElementById("bottom-panel");
  if (!panel) return;
  tabTerm = document.getElementById("bottom-tab-term");
  tabAgent = document.getElementById("bottom-tab-agent");
  termPane = document.getElementById("terminal-panel");
  agentPane = document.getElementById("agent-panel");
  getCwd = cwdProvider || (() => "");

  tabTerm?.addEventListener("click", () => showBottom("term"));
  tabAgent?.addEventListener("click", () => showBottom("agent"));
  document.getElementById("bottom-close")?.addEventListener("click", hideBottom);

  onBusy = (busy) => {
    const dot = document.querySelector(".title-dot");
    if (dot) dot.classList.toggle("working", !!busy);
  };

  // 顶部拖拽调整高度。
  const resizer = document.createElement("div");
  resizer.className = "bottom-resizer";
  panel.prepend(resizer);
  let dragging = false;
  resizer.addEventListener("mousedown", (e) => { dragging = true; panel.classList.add("resizing"); e.preventDefault(); });
  window.addEventListener("mousemove", (e) => {
    if (!dragging) return;
    const bottom = panel.getBoundingClientRect().bottom;
    const h = Math.max(120, Math.min(window.innerHeight - 120, bottom - e.clientY));
    panel.style.height = h + "px";
    fitTerminal();
  });
  window.addEventListener("mouseup", () => { if (dragging) { dragging = false; panel.classList.remove("resizing"); } });

  // 初次不显示；由快捷键或用户点击唤起。
  initialized = true;
}

let _projectId = "";
export function setProjectId(id) { _projectId = id; }
function getProjectId() { return _projectId; }

// 确保 Agent 面板已初始化（首次切到对话时）。
export function ensureAgent() {
  if (!isAgentReady()) initAgent({ projectId: getProjectId(), onBusy });
}