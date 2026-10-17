// IDE 底部终端：多 tab、xterm 实例管理、PTY 通信。
// 面板显隐由 bottom.js 管理；本模块只管终端实例。

import { termOpen, termWrite, termResize, termClose } from "./api.js";
import { Terminal } from "../vendor/xterm/xterm.mjs";
import { FitAddon } from "../vendor/xterm/addon-fit.mjs";

let tabsEl, hostsEl;
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
  if (s) { try { s.fit.fit(); } catch {} s.term.focus(); }
}

// 首次创建终端实例（供 bottom.js 在显示终端 tab 时调用）。
export function ensureTerminal() {
  if (!sessions.size) newTerminal();
  else { const id = activeId() || sessions.keys().next().value; setActive(id); }
}

// 重排所有终端（面板尺寸变化时）。
export function fitTerminal() {
  for (const s of sessions.values()) { try { s.fit.fit(); } catch {} }
}

export function setTerminalCwdProvider(fn) { getCwd = fn || (() => ""); }

export function newTerminal(opts) {
  opts = opts || {};
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
  label.textContent = opts.label || ("终端 " + seq);
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
  scheduleTermOverflow();

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
    }, opts.prog || null, opts.args || null).catch((e) => {
      term.write("\r\n终端启动失败：" + e + "\r\n");
    });
  });

  // 窗口尺寸变化时重排。
  const onWinResize = () => {
    for (const s of sessions.values()) { try { s.fit && s.fit.fit(); } catch {} }
    scheduleTermOverflow();
  };
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
  scheduleTermOverflow();
  if (sessions.size) {
    setActive(activeId() || sessions.keys().next().value);
  } else {
    hideTerminal();
  }
}

// 打开一个远程终端（ssh/tailscale ssh）。prog 与 args 透传给 PTY。
export function newRemoteTerminal(prog, args, label) {
  // 去掉 Tailscale 的 .tailxxxx.ts.net 后缀、前缀 "SSH "，只保留主机名首段。
  const clean = (label || "")
    .replace(/\.tail[0-9a-f]+\.ts\.net$/i, "")
    .replace(/^SSH\s+/, "");
  const host = clean.split(".")[0];
  return newTerminal({ prog, args, label: host ? "SSH " + host : "SSH" });
}

export function initTerminal({ projectId, cwdProvider, onRemoteNew }) {
  tabsEl = document.getElementById("term-tabs");
  hostsEl = document.getElementById("term-hosts");
  getCwd = cwdProvider || (() => "");
  if (!tabsEl || !hostsEl) return;

  document.getElementById("term-new")?.addEventListener("click", () => newTerminal());

  const remoteBtn = document.getElementById("term-remote");
  if (remoteBtn) {
    if (onRemoteNew) {
      remoteBtn.style.display = "";
      remoteBtn.addEventListener("click", () => onRemoteNew());
    } else {
      remoteBtn.style.display = "none";
    }
  }
}
let overflowObserver = null;

// 面板可能尚未显示（宽度 0）：延迟重试一次，确保测量到真实宽度。
function scheduleTermOverflow() {
  requestAnimationFrame(() => {
    if (!tabsEl || !tabsEl.clientWidth) {
      setTimeout(() => requestAnimationFrame(() => updateTermOverflow()), 120);
      return;
    }
    updateTermOverflow();
  });
}

export function updateTermOverflow() {
  if (!tabsEl) return;
  const head = tabsEl.parentElement;
  if (!head || head.clientWidth === 0) return;   // 隐藏时不处理
  const tabs = [...tabsEl.querySelectorAll(".term-tab")];
  tabsEl.classList.remove("overflowing");
  tabsEl.querySelector(".term-overflow-btn")?.remove();
  if (tabs.length < 2) return;
  let total = 0;
  for (const t of tabs) { t.style.display = ""; total += t.offsetWidth + 2; }
  const actions = head.querySelector(".term-actions");
  const avail = head.clientWidth - (actions?.offsetWidth || 0) - 16;
  if (avail <= 0 || total <= avail) return;
  tabsEl.classList.add("overflowing");
  const more = document.createElement("button");
  more.className = "term-overflow-btn";
  more.textContent = "＋" + tabs.length;
  more.title = "全部终端";
  more.addEventListener("click", (e) => { e.stopPropagation(); showTermOverflowPop(more); });
  tabsEl.appendChild(more);
}

// 对外暴露：面板显示/尺寸变化时调用（带 ResizeObserver，避免只靠 rAF 漏触发）。
export function fitTermOverflow() {
  if (!overflowObserver && tabsEl && tabsEl.parentElement) {
    try {
      overflowObserver = new ResizeObserver(() => scheduleTermOverflow());
      overflowObserver.observe(tabsEl.parentElement);
    } catch {}
  }
  scheduleTermOverflow();
}

function showTermOverflowPop(anchor) {
  document.querySelector(".term-overflow-pop")?.remove();
  const pop = document.createElement("div");
  pop.className = "term-overflow-pop";
  for (const [id, s] of sessions) {
    const name = s.tabEl?.querySelector("span")?.textContent || id;
    const item = document.createElement("div");
    item.className = "pop-item";
    item.textContent = name;
    item.addEventListener("click", () => { pop.remove(); setActive(id); });
    pop.appendChild(item);
  }
  document.body.appendChild(pop);
  const r = anchor.getBoundingClientRect();
  pop.style.position = "fixed";
  pop.style.left = r.left + "px";
  pop.style.top = (r.bottom + 4) + "px";
  setTimeout(() => {
    const close = (ev) => {
      if (!pop.contains(ev.target)) {
        pop.remove();
        document.removeEventListener("mousedown", close);
      }
    };
    document.addEventListener("mousedown", close);
  }, 0);
}