// IDE 底部终端：多 tab、xterm 实例管理、PTY 通信。
// 仅依赖 api.js 的 term* 封装；不参与 Agent 主流程。

import { termOpen, termWrite, termResize, termClose } from "./api.js";
import { Terminal } from "../vendor/xterm/xterm.mjs";
import { FitAddon } from "../vendor/xterm/addon-fit.mjs";

let panel, tabsEl, hostsEl;
let getCwd = () => "";
let seq = 0;
const sessions = new Map(); // id -> { term, fit, host, tabEl, disposers }

function activeId() {
  for (const [id, s] of sessions) if (s.host.classList.contains("active")) return id;
  return null;
}

function setActive(id) {
  for (const [sid, s] of sessions) {
    const on = sid === id;
    s.host.classList.toggle("active", on);
    s.tabEl.classList.toggle("active", on);
  }
  const s = sessions.get(id);
  if (s) { s.fit.fit(); s.term.focus(); }
}

export function isTerminalVisible() {
  return panel && !panel.classList.contains("hidden");
}

export function showTerminal() {
  if (!panel) return;
  panel.classList.remove("hidden");
  void panel.offsetHeight; // 强制 reflow，确保尺寸就绪
  if (!sessions.size) newTerminal();
  else { const id = activeId() || sessions.keys().next().value; setActive(id); }
}

export function hideTerminal() {
  if (panel) panel.classList.add("hidden");
}

export function toggleTerminal() {
  if (isTerminalVisible()) hideTerminal();
  else showTerminal();
}

export function newTerminal() {
  const id = "term-" + (++seq);
  const host = document.createElement("div");
  host.className = "term-host";
  // 先显示再 open，否则容器尺寸为 0 会渲染空白。
  host.classList.add("active");
  hostsEl.append(host);

  if (!Terminal) {
    host.textContent = "终端加载失败：xterm 模块未就绪";
    return id;
  }
  const term = new Terminal({
    fontFamily: "'JetBrainsMono Nerd Font', 'JetBrains Mono', Menlo, Consolas, monospace",
    fontSize: 13,
    cursorBlink: true,
    theme: { background: "#1e1e1e", foreground: "#d4d4d4" },
    scrollback: 5000,
  });
  let fit = null;
  try {
    if (FitAddon) {
      fit = new FitAddon();
      term.loadAddon(fit);
    }
  } catch {}

  try {
    term.open(host);
  } catch (e) {
    host.textContent = "终端初始化失败：" + e;
    return id;
  }

  const tabEl = document.createElement("div");
  tabEl.className = "term-tab";
  const label = document.createElement("span");
  label.textContent = "终端 " + seq;
  const close = document.createElement("span");
  close.className = "term-close";
  close.textContent = "✕";
  close.title = "关闭终端";
  close.addEventListener("click", (e) => { e.stopPropagation(); closeTerminal(id); });
  tabEl.append(label, close);
  tabEl.addEventListener("click", () => setActive(id));
  tabsEl.append(tabEl);

  const session = { term, fit, host, tabEl, disposers: [] };
  sessions.set(id, session);
  setActive(id);

  // 键盘输入 -> PTY。
  term.onData((data) => { termWrite(id, data).catch(() => {}); });
  // 尺寸变化 -> PTY。
  term.onResize(({ cols, rows }) => { termResize(id, cols, rows).catch(() => {}); });

  // 显示后再 fit 一次，拿准确尺寸。
  requestAnimationFrame(() => {
    try { if (fit) fit.fit(); } catch {}
    termOpen(id, getCwd(), null, term.cols || 80, term.rows || 24, (ev) => {
      if (!ev) return;
      if (ev.kind === "data") term.write(ev.data);
      else if (ev.kind === "exit") term.write("\r\n[进程已退出]\r\n");
    }).catch((e) => {
      term.write("\r\n终端启动失败：" + e + "\r\n");
    });
  });

  // 窗口尺寸变化时重排。
  const onWinResize = () => { for (const s of sessions.values()) { try { s.fit && s.fit.fit(); } catch {} } };
  window.addEventListener("resize", onWinResize);
  session.disposers.push(() => window.removeEventListener("resize", onWinResize));

  return id;
}

export async function closeTerminal(id) {
  const s = sessions.get(id);
  if (!s) return;
  for (const d of s.disposers) { try { d(); } catch {} }
  try { s.term.dispose(); } catch {}
  s.host.remove();
  s.tabEl.remove();
  sessions.delete(id);
  try { await termClose(id); } catch {}
  if (sessions.size) {
    setActive(activeId() || sessions.keys().next().value);
  } else {
    hideTerminal();
  }
}

export function initTerminal({ projectId, cwdProvider }) {
  panel = document.getElementById("terminal-panel");
  tabsEl = document.getElementById("term-tabs");
  hostsEl = document.getElementById("term-hosts");
  if (!panel) return;
  getCwd = cwdProvider || (() => "");

  document.getElementById("term-new")?.addEventListener("click", () => { showTerminal(); newTerminal(); });
  document.getElementById("term-hide")?.addEventListener("click", hideTerminal);

  // 顶部拖拽调整高度。
  const resizer = document.createElement("div");
  resizer.className = "term-resizer";
  panel.prepend(resizer);
  let dragging = false;
  resizer.addEventListener("mousedown", (e) => {
    dragging = true;
    panel.classList.add("resizing");
    e.preventDefault();
  });
  window.addEventListener("mousemove", (e) => {
    if (!dragging) return;
    const bottom = panel.getBoundingClientRect().bottom;
    const h = Math.max(80, Math.min(window.innerHeight - 120, bottom - e.clientY));
    panel.style.height = h + "px";
    for (const s of sessions.values()) s.fit.fit();
  });
  window.addEventListener("mouseup", () => {
    if (!dragging) return;
    dragging = false;
    panel.classList.remove("resizing");
  });
}