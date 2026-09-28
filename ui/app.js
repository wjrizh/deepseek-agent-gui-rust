const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
window.addEventListener("error", (e) => { try { invoke("log_frontend", { msg: "ERROR: " + (e.message||"") + " @ " + (e.filename||"") + ":" + (e.lineno||"") }); } catch(_) {} });
window.addEventListener("unhandledrejection", (e) => { try { invoke("log_frontend", { msg: "REJECT: " + (e.reason && (e.reason.stack||e.reason.message||e.reason)) }); } catch(_) {} });

const messagesEl = document.getElementById("messages");
const inputEl = document.getElementById("input");
const sendBtn = document.getElementById("send");
const stopBtn = document.getElementById("stop");
const modal = document.getElementById("modal");
const modalName = document.getElementById("modal-name");
const modalOk = document.getElementById("modal-ok");
const modalCancel = document.getElementById("modal-cancel");

let running = false;
let allAccounts = [];
let activeProjectId = null;
const busyProjects = new Set();
const views = new Map();

function view(pid) {
  if (!views.has(pid)) {
    const ge = messagesEl.querySelector(".global-empty");
    if (ge) ge.remove();
    const el = document.createElement("div");
    el.className = "msg-view hidden";
    el.dataset.pid = pid;
    messagesEl.appendChild(el);
    views.set(pid, {
      el,
      currentAssistant: null,
      currentAssistantText: "",
      currentThinking: null,
      currentTool: null,
      toolCards: [],
      toolCardsByCallId: {},
      thinkingEl: null,
      running: false,
      loaded: false,
      freeMode: false,
    });
  }
  return views.get(pid);
}

function activeView() {
  return activeProjectId ? view(activeProjectId) : null;
}

function showProject(pid) {
  activeProjectId = pid;
  const cur = view(pid);
  for (const [id, v] of views) {
    v.el.classList.toggle("hidden", id !== pid);
  }
  const v = cur;
  if (v) {
    setRunning(v.running);
    v.el.scrollTop = v.el.scrollHeight;
  }
  invoke("get_mode", { projectId: pid }).then((free) => { const vv = view(pid); vv.freeMode = !!free; if (activeProjectId === pid) applyMode(free); }).catch(() => {});
  invoke("get_workdir", { projectId: pid }).then((d) => {
    const el = document.getElementById("workdir-input");
    if (el) el.value = d;
  }).catch(() => {});
  invoke("get_restrict", { projectId: pid }).then((on) => {
    const el = document.getElementById("restrict-toggle");
    if (el) el.checked = !!on;
  }).catch(() => {});
  invoke("get_compact_threshold", { projectId: pid }).then((n) => {
    const el = document.getElementById("compact-input");
    if (el) el.value = n;
  }).catch(() => {});
  invoke("busy_projects").then((busy) => {
    const isBusy = Array.isArray(busy) && busy.includes(pid);
    if (v) v.running = isBusy;
    if (activeProjectId === pid) setRunning(isBusy);
  }).catch(() => {});
}

function addMsg(v, cls, text) {
  const empty = v.el.querySelector(".empty-state");
  if (empty) empty.remove();
  const el = document.createElement("div");
  el.className = "msg " + cls;
  el.textContent = text || "";
  v.el.appendChild(el);
  v.el.scrollTop = v.el.scrollHeight;
  return el;
}
function displayTextOf(v) {
  const raw = v.currentAssistantText || "";
  const m = raw.search(/<\s*(?:\|\s*)?(?:tool_calls|invoke)\b/i);
  return m === -1 ? raw : raw.slice(0, m).trimEnd();
}
function scheduleRender(v) {
  if (!v || v._renderTimer) return;
  v._renderTimer = setTimeout(() => {
    v._renderTimer = null;
    if (!v.currentAssistant) return;
    v.currentAssistant.textContent = displayTextOf(v);
    if (v.currentAssistant._caret) v.currentAssistant.appendChild(v.currentAssistant._caret);
    v.el.scrollTop = v.el.scrollHeight;
  }, 80);
}
function flushRender(v) {
  if (!v) return;
  if (v._renderTimer) { clearTimeout(v._renderTimer); v._renderTimer = null; }
  if (v.currentAssistant) {
    v.currentAssistant.textContent = displayTextOf(v);
    if (v.currentAssistant._caret) v.currentAssistant.appendChild(v.currentAssistant._caret);
  }
}
const TOOL_OUT_PREVIEW_TAIL = 20000;
function toolOutPreview(full) {
  if (!full) return "";
  if (full.length <= TOOL_OUT_PREVIEW_TAIL) return full;
  return "…（已省略前 " + (full.length - TOOL_OUT_PREVIEW_TAIL) + " 字符）\n" + full.slice(-TOOL_OUT_PREVIEW_TAIL);
}
function scheduleToolRender(card) {
  if (!card || card._outTimer) return;
  card._outTimer = setTimeout(() => {
    card._outTimer = null;
    flushToolRender(card);
  }, 80);
}
function flushToolRender(card) {
  if (!card) return;
  if (card._outTimer) { clearTimeout(card._outTimer); card._outTimer = null; }
  const pre = card.querySelector(".tool-out");
  if (!pre) return;
  const full = card._outText || "";
  const next = toolOutPreview(full);
  if (pre.textContent !== next) pre.textContent = next;
}
function appendToolOutput(card, chunk) {
  if (!card || !chunk) return;
  card._outText = (card._outText || "") + chunk;
  scheduleToolRender(card);
}
function renderAssistantText(v) { scheduleRender(v); }
function streamText(v, el, text) {
  if (!text) return;
  v.currentAssistantText += text;
  scheduleRender(v);
}
function attachCaret(el) {
  if (!el || el._caret) return;
  const c = document.createElement("span");
  c.className = "caret";
  el.appendChild(c);
  el._caret = c;
}
function detachCaret(el) {
  if (el && el._caret) { el._caret.remove(); el._caret = null; }
}

function scrollDown(v) { if (v) v.el.scrollTop = v.el.scrollHeight; }
function scheduleScrollDown(v) {
  if (!v || v._scrollTimer) return;
  v._scrollTimer = setTimeout(() => {
    v._scrollTimer = null;
    v.el.scrollTop = v.el.scrollHeight;
  }, 80);
}
function setRunning(v) { running = !!v; sendBtn.classList.toggle("hidden", running); stopBtn.classList.toggle("hidden", !running); }
function showThinking(v) {
  if (!v) return;
  hideThinking(v);
  const el = addMsg(v, "assistant thinking-bubble", "");
  el.innerHTML = String.raw`<span class="typing"><i></i><i></i><i></i></span>`;
  v.thinkingEl = el;
}
function hideThinking(v) {
  if (!v || !v.thinkingEl) return;
  v.thinkingEl.remove();
  v.thinkingEl = null;
}
function escapeHtml(s) { return s.replace(/[&<>]/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;"}[c])); }

function fileIconClass(name) {
  const ext = (name || "").split(".").pop().toLowerCase();
  if (["png","jpg","jpeg","gif","webp","bmp","svg","heic"].includes(ext)) return { cls: "img", label: ext };
  if (ext === "pdf") return { cls: "pdf", label: "PDF" };
  if (["doc","docx"].includes(ext)) return { cls: "doc", label: "DOC" };
  if (["xls","xlsx","csv"].includes(ext)) return { cls: "xls", label: "XLS" };
  if (["ppt","pptx"].includes(ext)) return { cls: "ppt", label: "PPT" };
  if (["zip","rar","7z","tar","gz"].includes(ext)) return { cls: "zip", label: "ZIP" };
  if (["js","ts","py","rs","c","cpp","h","java","go","json","html","css","sh","xml","yaml","yml","toml"].includes(ext)) return { cls: "code", label: ext };
  if (["txt","md","log"].includes(ext)) return { cls: "txt", label: ext || "TXT" };
  return { cls: "", label: ext ? ext.slice(0,4) : "FILE" };
}
function fileIconHtml(name) {
  const { cls, label } = fileIconClass(name);
  return '<span class="file-ico ' + cls + '">' + escapeHtml(label) + '</span>';
}

const SVG_SPIN = '<div class="spinner"></div>';
const SVG_CHECK = '<svg viewBox="0 0 24 24" width="16" height="16"><path d="M5 13l4 4L19 7" stroke="#34c759" stroke-width="2.2" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const SVG_ERR = '<svg viewBox="0 0 24 24" width="16" height="16"><path d="M12 3l9 16H3z" fill="none" stroke="#ff3b30" stroke-width="1.8" stroke-linejoin="round"/><path d="M12 10v4M12 17h.01" stroke="#ff3b30" stroke-width="1.8" stroke-linecap="round"/></svg>';
const SVG_CHEV = '<svg class="tool-chev" viewBox="0 0 24 24" width="14" height="14"><path d="M8 10l4 4 4-4" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>';

function toolCard(v, name, command) {
  const empty = v.el.querySelector(".empty-state");
  if (empty) empty.remove();
  const card = document.createElement("div");
  card.className = "tool-card running";
  card.innerHTML =
    '<div class="tool-head">' +
    '<span class="tool-ico">' + SVG_SPIN + '</span>' +
    '<span class="tool-name">' + escapeHtml(name) + '</span>' +
    '<span class="tool-cmd">' + escapeHtml(command || "") + '</span>' +
    '<span class="tool-meta"></span>' +
    '<button class="tool-stop" title="中断">' +
    '<svg viewBox="0 0 24 24" width="12" height="12"><rect x="6" y="6" width="12" height="12" rx="3" fill="currentColor"/></svg>' +
    '</button>' + SVG_CHEV +
    '</div>' +
    '<div class="tool-body"><div class="tool-body-inner"><pre class="tool-out"></pre></div></div>';
  card._start = performance.now();
  card.querySelector(".tool-head").addEventListener("click", () => card.classList.toggle("open"));
  card.querySelector(".tool-stop").addEventListener("click", (ev) => {
    ev.stopPropagation();
    invoke("interrupt", { projectId: activeProjectId || "" }).catch(() => {});
  });
  v.el.appendChild(card);
  scrollDown(v);
  return card;
}

function toolDone(v, card, error) {
  if (!card) return;
  card.classList.remove("running");
  card.classList.add(error ? "error" : "done");
  const ico = card.querySelector(".tool-ico");
  ico.innerHTML = error ? SVG_ERR : SVG_CHECK;
  if (!error) ico.firstChild.classList.add("check-pop");
  const ms = Math.round(performance.now() - (card._start || performance.now()));
  card.querySelector(".tool-meta").textContent = (ms / 1000).toFixed(1) + "s";
  card._done = true;
}

function toolComplete(v, card, ms, error) {
  if (!card) return;
  card.classList.remove("running");
  card.classList.add(error ? "error" : "done");
  const ico = card.querySelector(".tool-ico");
  ico.innerHTML = error ? SVG_ERR : SVG_CHECK;
  if (!error) ico.firstChild.classList.add("check-pop");
  const total = Number(ms) || 0;
  card.querySelector(".tool-meta").textContent = (total / 1000).toFixed(1) + "s";
  card._done = true;
}


listen("agent://delta", (e) => {
  const { project_id, kind, text } = e.payload || {};
  const v = project_id ? view(project_id) : activeView();
  if (!v) return;
  if (kind === "thinking") return;
  hideThinking(v);
  if (!v.currentAssistant) {
    v.currentAssistant = addMsg(v, "assistant", "");
    v.currentAssistantText = "";
    attachCaret(v.currentAssistant);
  }
  v.currentAssistantText += (text || "");
  renderAssistantText(v);
});

listen("agent://tool", (e) => {
  const { project_id, phase, name, command, output, image, ms, error, call_id } = e.payload || {};
  const v = project_id ? view(project_id) : activeView();
  if (!v) return;
  if (phase === "call") hideThinking(v);
  if (phase === "call") {
    flushRender(v);
    if (v.currentAssistant) {
      v.currentAssistantText = v.currentAssistantText.replace(/<\s*(?:\|\s*)?(?:tool_calls|invoke)[\s\S]*$/i, "").trimEnd();
      if (!v.currentAssistantText.trim()) {
        v.currentAssistant.remove();
        v.currentAssistant = null;
        v.currentAssistantText = "";
      } else {
        detachCaret(v.currentAssistant);
        v.currentAssistant.textContent = v.currentAssistantText;
      }
    }
    v.currentTool = toolCard(v, name, command);
    v.currentTool.dataset.toolName = name || "tool";
    if (call_id != null) { v.currentTool.dataset.callId = String(call_id); v.toolCardsByCallId[call_id] = v.currentTool; }
    v.toolCards.push(v.currentTool);
    v.currentAssistant = null;
    v.currentAssistantText = "";
  } else if (phase === "output") {
    let card = (call_id != null) ? v.toolCardsByCallId[call_id] : null;
    if (!card) card = v.currentTool;
    if (!card || card._done) {
      card = toolCard(v, name || "tool", "");
      card.dataset.toolName = name || "tool";
      if (call_id != null) { card.dataset.callId = String(call_id); v.toolCardsByCallId[call_id] = card; }
      v.toolCards.push(card);
      v.currentTool = card;
    }
    appendToolOutput(card, output);
    if (v.el.scrollTop + v.el.clientHeight >= v.el.scrollHeight - 40) scheduleScrollDown(v);
  } else if (phase === "image") {
    let card = (call_id != null) ? v.toolCardsByCallId[call_id] : null;
    if (!card) card = v.currentTool;
    if (!card || card._done) {
      card = toolCard(v, name || "tool", "");
      card.dataset.toolName = name || "tool";
      if (call_id != null) { card.dataset.callId = String(call_id); v.toolCardsByCallId[call_id] = card; }
      v.toolCards.push(card);
      v.currentTool = card;
    }
    const out = card.querySelector(".tool-out");
    const img = document.createElement("img");
    img.src = image;
    img.className = "tool-image";
    out.appendChild(img);
    if (v.el.scrollTop + v.el.clientHeight >= v.el.scrollHeight - 40) scheduleScrollDown(v);
  } else if (phase === "done") {
    let card = (call_id != null) ? v.toolCardsByCallId[call_id] : null;
    if (!card) card = v.currentTool;
    if (card) flushToolRender(card);
    toolComplete(v, card, ms || 0, !!error);
    if (call_id != null) delete v.toolCardsByCallId[call_id];
    if (card === v.currentTool) v.currentTool = null;
    if (v.running && !v.thinkingEl) showThinking(v);
  }
});

listen("agent://done", (e) => {
  const project_id = e.payload && e.payload.project_id ? e.payload.project_id : activeProjectId;
  const v = project_id ? view(project_id) : activeView();
  if (!v) return;
  hideThinking(v);
  v.toolCards.forEach(c => { if (!c._done) toolDone(v, c, false); });
  v.toolCards = []; v.toolCardsByCallId = {}; v.currentTool = null;
  v.running = false;
  flushRender(v);
  if (v.currentAssistant) {
    v.currentAssistantText = v.currentAssistantText.replace(/<\s*(?:\|\s*)?(?:tool_calls|invoke)[\s\S]*$/i, "").trimEnd();
    detachCaret(v.currentAssistant);
    v.currentAssistant.textContent = v.currentAssistantText;
    v.currentAssistant = null; v.currentAssistantText = ""; v.currentThinking = null;
  }
  if (project_id === activeProjectId) {
    setRunning(false);
    pendingFiles = [];
    renderFileChips();
  }
});
listen("agent://error", (e) => {
  const { project_id, message } = e.payload || {};
  const v = project_id ? view(project_id) : activeView();
  if (!v) return;
  hideThinking(v);
  v.toolCards.forEach(c => { if (!c._done) toolDone(v, c, true); });
  v.toolCards = []; v.toolCardsByCallId = {}; v.currentTool = null;
  flushRender(v);
  if (v.currentAssistant) {
    v.currentAssistantText = v.currentAssistantText.replace(/<\s*(?:\|\s*)?(?:tool_calls|invoke)[\s\S]*$/i, "").trimEnd();
    detachCaret(v.currentAssistant);
    if (v.currentAssistantText.trim()) {
      v.currentAssistant.textContent = v.currentAssistantText;
    } else {
      v.currentAssistant.remove();
    }
    v.currentAssistant = null; v.currentAssistantText = ""; v.currentThinking = null;
  }
  addMsg(v, "assistant", "⚠ " + (message || "未知错误"));
  v.running = false;
  if (project_id === activeProjectId) setRunning(false);
});

listen("agent://approval", (e) => {
  const id = e.payload.id;
  const project_id = e.payload && e.payload.project_id ? e.payload.project_id : activeProjectId;
  const v = project_id ? view(project_id) : activeView();
  const card = v ? v.currentTool : null;
  if (!card) { invoke("approve_command", { id, allow: false }); return; }

  const bar = document.createElement("div");
  bar.className = "tool-approval";
  bar.innerHTML =
    '<span class="tool-approval-txt">是否允许执行？</span>' +
    '<button class="tool-approve-deny">拒绝</button>' +
    '<button class="tool-approve-allow">允许</button>';

  bar.querySelector(".tool-approve-allow").onclick = () => {
    bar.remove();
    card.classList.remove("awaiting");
    invoke("approve_command", { id, allow: true });
  };
  bar.querySelector(".tool-approve-deny").onclick = () => {
    bar.remove();
    card.classList.remove("awaiting");
    invoke("approve_command", { id, allow: false });
  };

  card.querySelector(".tool-body").before(bar);
  card.classList.add("awaiting", "open");
});

async function send() {
const text = inputEl.value.trim();
  if ((!text && !pendingFiles.length) || running) return;
  if (!activeProjectId) { iosToast("请先选择一个项目"); return; }
  const v = view(activeProjectId);
  if (!v.loaded) await loadActiveConversation(activeProjectId);
  const sentFiles = pendingFiles.slice();
  const userEl = addMsg(v, "user", text);
  if (sentFiles.length) {
    const wrap = document.createElement("div");
    wrap.className = "msg-files";
    for (const f of sentFiles) {
      const item = document.createElement("span");
      item.className = "msg-file";
      item.innerHTML = fileIconHtml(f.name) +
        '<span class="msg-file-name">' + escapeHtml(f.name || "") + '</span>';
      wrap.appendChild(item);
    }
    userEl.appendChild(wrap);
  }
  inputEl.value = ""; inputEl.style.height = "auto";
  v.loaded = true;
  setRunning(true); v.running = true;
  v.currentAssistant = null; v.currentAssistantText = ""; v.currentThinking = null; v.currentTool = null; v.toolCards = []; v.toolCardsByCallId = {};
  showThinking(v);
  try {
    await invoke("send_message", { projectId: activeProjectId, text });
    pendingFiles = [];
    renderFileChips();
  }
  catch (err) { hideThinking(v); addMsg(v, "assistant", "⚠ " + err); setRunning(false); v.running = false; }
}

sendBtn.onclick = send;
inputEl.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } });
inputEl.addEventListener("input", () => { inputEl.style.height = "auto"; inputEl.style.height = Math.min(inputEl.scrollHeight, 160) + "px"; });
stopBtn.onclick = () => invoke("interrupt", { projectId: activeProjectId || "" }).catch(() => {});

// 工作目录栏
const workdirInput = document.getElementById("workdir-input");
const restrictToggle = document.getElementById("restrict-toggle");
const restrictCheck = document.getElementById("restrict-check");
if (workdirInput) {
  workdirInput.onclick = async () => {
    if (!activeProjectId) { iosToast("请先选择一个项目"); return; }
    try {
      const dlg = window.__TAURI__.dialog;
      const picked = await dlg.open({ directory: true, multiple: false, defaultPath: workdirInput.value || undefined });
      if (!picked) return;
      const path = typeof picked === "string" ? picked : (picked.path || picked);
      const next = await invoke("set_workdir", { projectId: activeProjectId, path });
      workdirInput.value = next;
      iosToast("工作目录已切换");
    } catch (e) { iosToast("切换失败：" + e); }
  };
}
if (restrictCheck) {
  restrictCheck.onchange = async () => {
    if (!activeProjectId) { restrictCheck.checked = false; return; }
    const on = restrictCheck.checked;
    try {
      await invoke("set_restrict", { projectId: activeProjectId, on });
      iosToast(on ? "已开启目录限制" : "已关闭目录限制");
    } catch (e) {
      restrictCheck.checked = !on;
      iosToast("切换失败：" + e);
    }
  };
}
const compactInput = document.getElementById("compact-input");
if (compactInput) {
  const applyCompact = async () => {
    if (!activeProjectId) { iosToast("请先选择一个项目"); return; }
    const n = parseInt(compactInput.value, 10);
    if (!Number.isFinite(n) || n <= 0) {
      iosToast("压缩阈值必须为正整数");
      invoke("get_compact_threshold", { projectId: activeProjectId })
        .then((v) => { compactInput.value = v; }).catch(() => {});
      return;
    }
    try {
      const saved = await invoke("set_compact_threshold", { projectId: activeProjectId, value: n });
      compactInput.value = saved;
      iosToast("压缩阈值已设为 " + saved + "（下一轮生效）");
    } catch (e) { iosToast("保存失败：" + e); }
  };
  compactInput.onchange = applyCompact;
  compactInput.onkeydown = (e) => { if (e.key === "Enter") { e.preventDefault(); compactInput.blur(); } };
  compactInput.onblur = applyCompact;
}

const fileBtn = document.getElementById("file-btn");
const fileChips = document.getElementById("file-chips");
let pendingFiles = [];

function renderFileChips() {
  fileChips.innerHTML = "";
  fileChips.classList.toggle("hidden", pendingFiles.length === 0);
  pendingFiles.forEach((f, i) => {
    const chip = document.createElement("span");
    chip.className = "file-chip";
    chip.innerHTML = fileIconHtml(f.name) +
      '<span class="file-chip-name">' + escapeHtml(f.name || "") + '</span>' +
      '<button class="file-chip-del" title="移除">✕</button>';
    chip.querySelector(".file-chip-del").onclick = async () => {
      const removed = pendingFiles[i];
      pendingFiles.splice(i, 1);
      renderFileChips();
      try { await invoke("set_pending_files", { projectId: activeProjectId, files: pendingFiles }); } catch {}
      if (removed) { try { await invoke("remove_file", { projectId: activeProjectId, name: removed.name }); } catch {} }
    };
    fileChips.appendChild(chip);
  });
}

fileBtn.onclick = async () => {
  if (!activeProjectId) { iosToast("请先选择一个项目"); return; }
  const v = activeView();
  if ((v && v.running) || running) { iosToast("正在生成回复，请稍后"); return; }
  let selected;
  try {
    selected = await window.__TAURI__.dialog.open({
      multiple: true,
      title: "选择文件（最多 50 个，单个 ≤100MB）",
    });
  } catch (e) { iosToast("打开文件选择器失败：" + e); return; }
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  if (!paths.length) return;
  if (pendingFiles.length + paths.length > 50) {
    iosToast("一次最多上传 50 个文件");
    return;
  }
  iosToast("上传中…");
  try {
    const files = await invoke("upload_files", { projectId: activeProjectId, paths });
    pendingFiles = pendingFiles.concat(files);
    renderFileChips();
  } catch (e) {
    iosToast("上传失败：" + e);
  }
};

const modeToggle = document.getElementById("mode-toggle");
const modeLabel = modeToggle.querySelector(".mode-label");
let freeMode = false;

function applyMode(free) {
  freeMode = !!free;
  const av = activeView();
  if (av) av.freeMode = freeMode;
  modeLabel.textContent = freeMode ? "Free" : "Safe";
  modeToggle.classList.toggle("free", freeMode);
  modeToggle.classList.toggle("safe", !freeMode);
  modeToggle.title = freeMode ? "Free 模式：自动执行命令" : "Safe 模式：危险命令需确认";
}

modeToggle.onclick = async () => {
  const av = activeView();
  const cur = av ? av.freeMode : freeMode;
  const next = !cur;
  const pid = activeProjectId || "";
  applyMode(next);
  try { await invoke("set_mode", { projectId: pid, free: next }); }
  catch (e) { applyMode(!next); if (activeView()) addMsg(activeView(), "assistant", "⚠ 切换模式失败：" + e); }
};

invoke("get_mode", { projectId: activeProjectId || "" }).then(applyMode).catch(() => applyMode(false));

const thinkToggle = document.getElementById("think-toggle");
let thinkMode = false;

function applyThink(on) {
  thinkMode = !!on;
  thinkToggle.classList.toggle("on", thinkMode);
  thinkToggle.title = thinkMode ? "深度思考：已开启" : "深度思考：已关闭";
}

thinkToggle.onclick = async () => {
  const next = !thinkMode;
  applyThink(next);
  try { await invoke("set_thinking", { projectId: activeProjectId || "", on: next }); }
  catch (e) { applyThink(!next); if (activeView()) addMsg(activeView(), "assistant", "⚠ 切换深度思考失败：" + e); }
};

invoke("get_thinking", { projectId: activeProjectId || "" }).then(applyThink).catch(() => applyThink(false));

const searchToggle = document.getElementById("search-toggle");
let searchMode = false;

function applySearch(on) {
  searchMode = !!on;
  searchToggle.classList.toggle("on", searchMode);
  searchToggle.title = searchMode ? "联网搜索：已开启" : "联网搜索：已关闭";
}

searchToggle.onclick = async () => {
  const next = !searchMode;
  applySearch(next);
  try { await invoke("set_search", { projectId: activeProjectId || "", on: next }); }
  catch (e) { applySearch(!next); if (activeView()) addMsg(activeView(), "assistant", "⚠ 切换联网搜索失败：" + e); }
};

invoke("get_search", { projectId: activeProjectId || "" }).then(applySearch).catch(() => applySearch(false));

const qqToggle = document.getElementById("qq-toggle");
function applyQq(on) {
  qqToggle.classList.toggle("on", !!on);
  qqToggle.title = on ? "QQ 接入：已开启" : "QQ 接入：已关闭";
}
if (qqToggle) {
  qqToggle.onclick = async () => {
    const next = !qqToggle.classList.contains("on");
    applyQq(next);
    try { await invoke("qq_toggle", { enable: next }); }
    catch (e) { applyQq(!next); iosToast("QQ 切换失败：" + e); }
  };
  invoke("qq_status").then((s) => applyQq(!!(s && s.running))).catch(() => applyQq(false));
}

function iosToast(message) {
  let host = document.getElementById("toast-host");
  if (!host) {
    host = document.createElement("div");
    host.id = "toast-host";
    document.body.appendChild(host);
  }
  const t = document.createElement("div");
  t.className = "ios-toast";
  t.textContent = message;
  host.appendChild(t);
  requestAnimationFrame(() => t.classList.add("show"));
  setTimeout(() => {
    t.classList.remove("show");
    setTimeout(() => t.remove(), 260);
  }, 2000);
}

function iosConfirm(message, okText) {
  return new Promise((resolve) => {
    let host = document.getElementById("confirm-host");
    if (!host) {
      host = document.createElement("div");
      host.id = "confirm-host";
      document.body.appendChild(host);
    }
    const mask = document.createElement("div");
    mask.className = "ios-confirm-mask";
    mask.innerHTML =
      '<div class="ios-confirm-box">' +
        '<div class="ios-confirm-title">确认</div>' +
        '<div class="ios-confirm-msg"></div>' +
        '<div class="ios-confirm-actions">' +
          '<button class="ios-confirm-btn cancel">取消</button>' +
          '<button class="ios-confirm-btn ok danger"></button>' +
        '</div>' +
      '</div>';
    mask.querySelector(".ios-confirm-msg").textContent = message;
    mask.querySelector(".ios-confirm-btn.ok").textContent = okText || "确定";
    const done = (v) => {
      mask.classList.remove("show");
      setTimeout(() => mask.remove(), 220);
      resolve(v);
    };
    mask.querySelector(".ios-confirm-btn.cancel").onclick = () => done(false);
    mask.querySelector(".ios-confirm-btn.ok").onclick = () => done(true);
    mask.onclick = (e) => { if (e.target === mask) done(false); };
    host.appendChild(mask);
    requestAnimationFrame(() => mask.classList.add("show"));
  });
}

function showDeletingOverlay(text) {
  hideDeletingOverlay();
  const ov = document.createElement("div");
  ov.id = "delete-overlay";
  ov.className = "delete-overlay";
  ov.innerHTML =
    '<div class="delete-overlay-box">' +
      '<div class="delete-overlay-spin"></div>' +
      '<div class="delete-overlay-txt">' + (text || "删除中…") + '</div>' +
    '</div>';
  document.body.appendChild(ov);
  requestAnimationFrame(() => ov.classList.add("show"));
  return {
    setText: (s) => { const el = ov.querySelector(".delete-overlay-txt"); if (el) el.textContent = s; },
    close: () => { ov.classList.remove("show"); setTimeout(() => ov.remove(), 220); },
  };
}

function hideDeletingOverlay() {
  const ov = document.getElementById("delete-overlay");
  if (ov) ov.remove();
}

function inlineConfirm(container, message, onConfirm) {
  const old = container.querySelector(":scope > .inline-confirm");
  if (old) old.remove();
  const bar = document.createElement("div");
  bar.className = "inline-confirm";
  const txt = document.createElement("span");
  txt.className = "inline-confirm-txt";
  txt.textContent = message;
  const cancel = document.createElement("button");
  cancel.className = "ic-cancel";
  cancel.textContent = "取消";
  const ok = document.createElement("button");
  ok.className = "ic-ok";
  ok.textContent = "删除";
  bar.appendChild(txt);
  bar.appendChild(cancel);
  bar.appendChild(ok);
  const close = () => bar.remove();
  cancel.onclick = (ev) => { ev.stopPropagation(); close(); };
  ok.onclick = async (ev) => { ev.stopPropagation(); close(); await onConfirm(); };
  container.appendChild(bar);
  return bar;
}

function clearMessages() {
  const v = activeView();
  if (!v) return;
  v.el.innerHTML = '<div class="empty-state"><div class="empty-logo"><svg viewBox="0 0 60 60" width="56" height="56" fill="none"><defs><linearGradient id="el-grad" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#007aff"/><stop offset="100%" stop-color="#5ac8fa"/></linearGradient></defs><circle cx="30" cy="30" r="22" stroke="url(#el-grad)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="104 34" class="el-arc"/><circle cx="30" cy="30" r="10" stroke="var(--faint)" stroke-width="2" class="el-core"/></svg></div><div class="empty-title">今天想聊些什么？</div></div>';
}

function showEmptyState() {
  const html = '<div class="empty-state"><div class="empty-logo"><svg viewBox="0 0 60 60" width="56" height="56" fill="none"><defs><linearGradient id="el-grad" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#007aff"/><stop offset="100%" stop-color="#5ac8fa"/></linearGradient></defs><circle cx="30" cy="30" r="22" stroke="url(#el-grad)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="104 34" class="el-arc"/><circle cx="30" cy="30" r="10" stroke="var(--faint)" stroke-width="2" class="el-core"/></svg></div><div class="empty-title">今天想聊些什么？</div></div>';
  const v = activeView();
  if (v) { v.el.innerHTML = html; return; }
  if (!messagesEl.querySelector(".msg-view")) {
    let holder = messagesEl.querySelector(".global-empty");
    if (!holder) {
      holder = document.createElement("div");
      holder.className = "msg-view global-empty";
      messagesEl.appendChild(holder);
    }
    holder.innerHTML = html;
  }
}

async function loadConversation(projectId, conversationId) {
  const v = projectId ? view(projectId) : activeView();
  if (!v) return;
  if (!conversationId) {
    v.el.innerHTML = '<div class="empty-state"><div class="empty-logo"><svg viewBox="0 0 60 60" width="56" height="56" fill="none"><defs><linearGradient id="el-grad" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#007aff"/><stop offset="100%" stop-color="#5ac8fa"/></linearGradient></defs><circle cx="30" cy="30" r="22" stroke="url(#el-grad)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="104 34" class="el-arc"/><circle cx="30" cy="30" r="10" stroke="var(--faint)" stroke-width="2" class="el-core"/></svg></div><div class="empty-title">今天想聊些什么？</div></div>';
    v.loaded = true;
    return;
  }
  let msgs = [];
  try { msgs = await invoke("load_conversation_messages", { conversationId }); }
  catch { msgs = []; }
  v.el.innerHTML = '<div class="empty-state"><div class="empty-logo"><svg viewBox="0 0 60 60" width="56" height="56" fill="none"><defs><linearGradient id="el-grad" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#007aff"/><stop offset="100%" stop-color="#5ac8fa"/></linearGradient></defs><circle cx="30" cy="30" r="22" stroke="url(#el-grad)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="104 34" class="el-arc"/><circle cx="30" cy="30" r="10" stroke="var(--faint)" stroke-width="2" class="el-core"/></svg></div><div class="empty-title">今天想聊些什么？</div></div>';
  for (const m of msgs) {
    if (m.role === "user") {
      addMsg(v, "user", m.text);
    } else if (m.role === "tool" && m.tool) {
      addHistoryToolCard(v, m.tool);
    } else {
      addMsg(v, "assistant", m.text);
    }
  }
  v.loaded = true;
}

function addHistoryToolCard(v, tool) {
  const empty = v.el.querySelector(".empty-state");
  if (empty) empty.remove();
  const card = document.createElement("div");
  card.className = "tool-card " + (tool.error ? "error" : "done");
  const ms = (tool.ms ? tool.ms / 1000 : 0).toFixed(1);
  card.innerHTML =
    '<div class="tool-head">' +
      '<span class="tool-ico">' + (tool.error ? SVG_ERR : SVG_CHECK) + '</span>' +
      '<span class="tool-name">' + escapeHtml(tool.name || "") + '</span>' +
      (tool.command ? '<span class="tool-cmd">' + escapeHtml(tool.command) + '</span>' : '') +
      '<span class="tool-meta">' + (tool.ms ? ms + "s" : "") + '</span>' +
      SVG_CHEV +
    '</div>' +
    '<div class="tool-body"><div class="tool-body-inner"><pre class="tool-out"></pre></div></div>';
  const ico = card.querySelector(".tool-ico");
  if (!tool.error && ico.firstChild) ico.firstChild.classList.add("check-pop");
  card._outText = tool.output || "";
  card.querySelector(".tool-out").textContent = toolOutPreview(card._outText);
  card.querySelector(".tool-head").addEventListener("click", () => card.classList.toggle("open"));
  v.el.appendChild(card);
  scrollDown(v);
  return card;
}

async function loadActiveConversation(projectId) {
if (!projectId) return;
  const v = view(projectId);
  if (!v || v.loaded || v._loading) return;
  v._loading = true;
  try {
    let msgs = [];
    try { msgs = await invoke("load_project_messages", { projectId }); }
    catch { msgs = []; }
    v.el.innerHTML = '<div class="empty-state"><div class="empty-logo"><svg viewBox="0 0 60 60" width="56" height="56" fill="none"><defs><linearGradient id="el-grad" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#007aff"/><stop offset="100%" stop-color="#5ac8fa"/></linearGradient></defs><circle cx="30" cy="30" r="22" stroke="url(#el-grad)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="104 34" class="el-arc"/><circle cx="30" cy="30" r="10" stroke="var(--faint)" stroke-width="2" class="el-core"/></svg></div><div class="empty-title">今天想聊些什么？</div></div>';
    for (const m of msgs) {
      if (m.role === "user") addMsg(v, "user", m.text);
      else if (m.role === "tool" && m.tool) addHistoryToolCard(v, m.tool);
      else addMsg(v, "assistant", m.text);
    }
    v.loaded = true;
    scrollDown(v);
  } finally { v._loading = false; }
}

async function refreshStatus() {
  const info = await invoke("account_status").catch(() => ({ accounts: [] }));
  allAccounts = info.accounts || [];
  renderAccountMenu();
  return info;
}

document.getElementById("add-account").onclick = () => {
  modalName.value = "";
  modal.classList.remove("hidden");
  modalName.focus();
};
modalOk.onclick = async () => {
  const name = modalName.value.trim();
  if (!name) { iosToast("账号名不能为空"); return; }
  modal.classList.add("hidden");
  try {
    await invoke("login_browser", { name });
    iosToast("浏览器登录完成");
    await refreshStatus();
  } catch (e) { iosToast("登录失败：" + e); }
};
modalCancel.onclick = () => modal.classList.add("hidden");


// ---------- 项目管理 ----------
const projectList = document.getElementById("project-list");
const projectModal = document.getElementById("project-modal");
const projectModalTitle = document.getElementById("project-modal-title");
const projectModalDesc = document.getElementById("project-modal-desc");
const projectModalName = document.getElementById("project-modal-name");
const projectModalCancel = document.getElementById("project-modal-cancel");
const projectModalOk = document.getElementById("project-modal-ok");
const projectAccountToggle = document.getElementById("project-account-toggle");
const projectAccountMenu = document.getElementById("project-account-menu");
const projectAccountLabel = document.getElementById("project-account-label");
let selectedAccount = "";

function renderAccountMenu() {
  if (!projectAccountMenu) return;
  projectAccountMenu.innerHTML = "";
  if (!allAccounts.length) {
    const d = document.createElement("div");
    d.className = "dropdown-item disabled";
    d.textContent = "（无账号）";
    projectAccountMenu.appendChild(d);
    return;
  }
  allAccounts.forEach((a) => {
    const it = document.createElement("div");
    it.className = "dropdown-item" + (a === selectedAccount ? " active" : "");
    it.textContent = a;
    it.onclick = () => {
      selectedAccount = a;
      projectAccountLabel.textContent = a;
      projectAccountMenu.classList.add("hidden");
      projectAccountToggle.classList.remove("open");
      renderAccountMenu();
    };
    projectAccountMenu.appendChild(it);
  });
}

projectAccountToggle.onclick = (ev) => {
  ev.stopPropagation();
  const hidden = projectAccountMenu.classList.toggle("hidden");
  projectAccountToggle.classList.toggle("open", !hidden);
};
document.addEventListener("click", (ev) => {
  if (!document.getElementById("project-account").contains(ev.target)) {
    if (projectAccountMenu) projectAccountMenu.classList.add("hidden");
    if (projectAccountToggle) projectAccountToggle.classList.remove("open");
  }
});

function openProjectModal(mode, id, curName) {
  projectModalMode = mode;
  projectModalTarget = id || null;
  projectModalTitle.textContent = mode === "create" ? "新建项目" : "重命名项目";
  projectModalDesc.textContent = mode === "create" ? "给项目起个名字并选择账号。" : "输入新的项目名。";
  projectModalName.value = curName || "";
  if (mode === "create") {
    selectedAccount = allAccounts[0] || "";
    projectAccountLabel.textContent = selectedAccount || "请选择账号";
    renderAccountMenu();
    document.getElementById("project-account").classList.remove("hidden");
  } else {
    document.getElementById("project-account").classList.add("hidden");
  }
  projectModal.classList.remove("hidden");
  projectModalName.focus();
}
function closeProjectModal() { projectModal.classList.add("hidden"); }

projectModalCancel.onclick = closeProjectModal;
projectModalOk.onclick = async () => {
  const name = projectModalName.value.trim();
  if (!name) { iosToast("项目名不能为空"); return; }
  try {
    if (projectModalMode === "create") {
      const account = selectedAccount;
      if (!account) { iosToast("请选择账号"); return; }
      const pv = await invoke("create_project", { name, account });
      closeProjectModal();
      activeProjectId = pv.id;
      showProject(pv.id);
      await renderProjects();
      await invoke("open_project", { id: pv.id });
      await loadActiveConversation(pv.id);
      await renderProjects();
    } else {
      await invoke("rename_project", { id: projectModalTarget, name });
      closeProjectModal();
      await renderProjects();
    }
  } catch (e) { iosToast("操作失败：" + e); }
};
projectModalName.addEventListener("keydown", (e) => { if (e.key === "Enter") projectModalOk.click(); });

async function renderProjects() {
  let projects = [];
  try { projects = await invoke("list_projects"); } catch { projects = []; }
  projects.sort((a, b) => b.created_at - a.created_at);
  projectList.innerHTML = "";
  if (!projects.length) {
    const empty = document.createElement("div");
    empty.className = "project-empty";
    empty.textContent = "暂无项目";
    projectList.appendChild(empty);
    return;
  }
  projects.forEach((p) => {
    const item = document.createElement("div");
    item.className = "project-item" + (p.active ? " active" : "") + (busyProjects.has(p.id) ? " working" : "");
    item.dataset.pid = p.id;
    item.innerHTML =
      '<span class="project-name">' + escapeHtml(p.name) + '</span>' +
      '<span class="project-account">' + escapeHtml(p.account || "") + '</span>' +
      '<span class="project-count">' + p.conversation_count + '</span>' +
      '<button class="project-set" title="项目设置">' +
        '<svg viewBox="0 0 24 24" width="14" height="14"><circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>' +
      '</button>';
    item.onclick = async () => {
      try {
        await invoke("open_project", { id: p.id });
        showProject(p.id);
        const v = view(p.id);
        if (!v.running && !v.loaded) await loadActiveConversation(p.id);
        await renderProjects();
      } catch (e) { addMsg(view(p.id), "assistant", "⚠ 打开项目失败：" + e); }
    };
    item.querySelector(".project-set").onclick = (ev) => {
      ev.stopPropagation();
      openProjectSettings(p.id, p.name);
    };
    projectList.appendChild(item);
  });
}

const settingsModal = document.getElementById("settings-modal");
const settingsTitle = document.getElementById("settings-title");
const settingsClose = document.getElementById("settings-close");
const settingsNewConv = document.getElementById("settings-new-conv");
const settingsRename = document.getElementById("settings-rename");
const settingsDelete = document.getElementById("settings-delete");
const settingsClearHistory = document.getElementById("settings-clear-history");
const settingsConvCount = document.getElementById("settings-conv-count");
const settingsConvList = document.getElementById("settings-conv-list");

let settingsProjectId = null;
let settingsProjectName = null;

async function openProjectSettings(id, name) {
  settingsProjectId = id;
  settingsProjectName = name;
  settingsTitle.textContent = name;
  settingsModal.classList.remove("hidden");
  await renderConversations();
}

function closeProjectSettings() {
  settingsModal.classList.add("hidden");
  settingsProjectId = null;
}

async function renderConversations() {
  let convs = [];
  try { convs = await invoke("list_conversations", { projectId: settingsProjectId }); }
  catch { convs = []; }
  settingsConvCount.textContent = convs.length;
  settingsConvList.innerHTML = "";
  if (!convs.length) {
    const empty = document.createElement("div");
    empty.className = "conv-empty";
    empty.textContent = "暂无对话，点「新建对话」开始。";
    settingsConvList.appendChild(empty);
    return;
  }
  convs.forEach((c) => {
    const row = document.createElement("div");
    row.className = "conv-item" + (c.active ? " active" : "");
    row.innerHTML =
      '<span class="conv-box">' + (c.active ? "●" : "○") + '</span>' +
      '<span class="conv-title">' + escapeHtml(c.title || "新对话") + '</span>' +
      '<span class="conv-meta">共 ' + c.message_count + ' 条</span>' +
      '<button class="conv-detach" title="移出项目（保留本地记录与云端会话）">' +
        '<svg viewBox="0 0 24 24" width="13" height="13"><path d="M9 7H6v10h10v-3M14 6h4v4M18 6l-7 7" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>' +
      '</button>' +
      '<button class="conv-del" title="删除对话（同时删除云端会话）">' +
        '<svg viewBox="0 0 24 24" width="13" height="13"><path d="M6 7h12M9 7V5h6v2M8 7l1 12h6l1-12" stroke="currentColor" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>' +
      '</button>';
    row.onclick = async () => {
      if (c.active) return;
      try {
        await invoke("switch_conversation", { projectId: settingsProjectId, conversationId: c.id });
        await loadConversation(settingsProjectId, c.id);
        await renderConversations();
        await renderProjects();
      } catch (e) { addMsg(view(settingsProjectId), "assistant", "⚠ 切换对话失败：" + e); }
    };
    row.querySelector(".conv-detach").onclick = (ev) => {
      ev.stopPropagation();
      inlineConfirm(row, "移出该对话？本地记录与云端会话都会保留，可再加入其他项目。", async () => {
        const ov = showDeletingOverlay("移出中…");
        try {
          await invoke("detach_conversation", { projectId: settingsProjectId, conversationId: c.id });
          await loadActiveConversation(settingsProjectId);
          await renderConversations();
          await renderProjects();
        } catch (e) { iosToast("移出失败：" + e); }
        finally { ov.close(); }
      });
    };
    row.querySelector(".conv-del").onclick = (ev) => {
      ev.stopPropagation();
      inlineConfirm(row, "删除该对话？", async () => {
        const ov = showDeletingOverlay("删除中…");
        try {
          await invoke("delete_conversation", { projectId: settingsProjectId, conversationId: c.id });
          await loadActiveConversation(settingsProjectId);
          await renderConversations();
          await renderProjects();
        } catch (e) { iosToast("删除对话失败：" + e); }
        finally { ov.close(); }
      });
    };
    settingsConvList.appendChild(row);
  });
}

settingsClose.onclick = closeProjectSettings;

settingsNewConv.onclick = async () => {
  try {
    const pv = await invoke("new_conversation", { projectId: settingsProjectId });
    await loadConversation(settingsProjectId, pv.id);
    await renderConversations();
    await renderProjects();
  } catch (e) { addMsg(view(settingsProjectId), "assistant", "⚠ 新建对话失败：" + e); }
};

const settingsAddHistory = document.getElementById("settings-add-history");

settingsAddHistory.onclick = async () => {
  const host = document.getElementById("settings-conv-list");
  const old = host.parentElement.querySelector(":scope > .inline-picker");
  if (old) { old.remove(); return; }

  let sessions = [];
  try {
    sessions = await invoke("list_project_account_sessions", { projectId: settingsProjectId });
  } catch (e) {
    iosToast("读取历史会话失败：" + e);
    return;
  }

  let attached = [];
  try { attached = await invoke("list_conversations", { projectId: settingsProjectId }); }
  catch { attached = []; }
  const used = new Set(attached.map((c) => c.id));
  const items = sessions.filter((s) => !used.has(s.id));

  const bar = document.createElement("div");
  bar.className = "inline-picker";

  const head = document.createElement("div");
  head.className = "inline-picker-head";
  const txt = document.createElement("span");
  txt.className = "inline-picker-txt";
  txt.textContent = items.length ? "选择要加入的历史对话" : "没有可添加的历史会话";
  const cancel = document.createElement("button");
  cancel.className = "ic-cancel";
  cancel.textContent = "取消";
  head.appendChild(txt);
  head.appendChild(cancel);
  bar.appendChild(head);

  if (items.length) {
    const list = document.createElement("div");
    list.className = "inline-picker-list";
    items.forEach((it) => {
      const row = document.createElement("div");
      row.className = "inline-picker-item";
      row.textContent = it.title || it.id;
      row.onclick = async () => {
        bar.remove();
        const ov = showDeletingOverlay("添加中…");
        try {
          const pv = await invoke("attach_account_session", {
            projectId: settingsProjectId,
            sessionId: it.id,
          });
          await loadConversation(settingsProjectId, pv.id);
          await renderConversations();
          await renderProjects();
        } catch (e) { iosToast("添加失败：" + e); }
        finally { ov.close(); }
      };
      list.appendChild(row);
    });
    bar.appendChild(list);
  }

  cancel.onclick = () => bar.remove();
  host.parentElement.insertBefore(bar, host);
};

settingsRename.onclick = () => {
  const pid = settingsProjectId;
  const pname = settingsProjectName;
  closeProjectSettings();
  openProjectModal("rename", pid, pname);
};

settingsDelete.onclick = () => {
  const delId = settingsProjectId;
  inlineConfirm(settingsDelete.parentElement, "删除项目「" + settingsProjectName + "」及其全部对话？", async () => {
    const ov = showDeletingOverlay("删除中…");
    try {
      await invoke("delete_project", { id: delId });
      closeProjectSettings();
      const v = views.get(delId);
      if (v) { v.el.remove(); views.delete(delId); }
      busyProjects.delete(delId);
      if (activeProjectId === delId) {
        activeProjectId = null;
        let projects = [];
        try { projects = await invoke("list_projects"); } catch { projects = []; }
        projects.sort((a, b) => b.created_at - a.created_at);
        if (projects.length) {
          const next = projects[0];
          await invoke("open_project", { id: next.id });
          showProject(next.id);
          const nv = view(next.id);
          if (!nv.running && !nv.loaded) await loadActiveConversation(next.id);
        } else {
          for (const [, vv] of views) vv.el.remove();
          views.clear();
          showEmptyState();
        }
      }
      await renderProjects();
    } catch (e) { iosToast("删除项目失败：" + e); }
    finally { ov.close(); }
  });
};

settingsClearHistory.onclick = async () => {
  const btn = settingsClearHistory;
  if (btn.dataset.busy === "1") return;
  if (!confirm("清除本项目所有本地对话记录？此操作不可恢复（不影响服务端对话）。")) return;
  btn.dataset.busy = "1";
  btn.disabled = true;
  const original = btn.textContent;
  btn.textContent = "清除中…";
  try {
    const n = await invoke("clear_local_history", { projectId: settingsProjectId });
    iosToast("已清除 " + n + " 个对话记录");
    await renderProjects();
    const v = activeView();
    if (v) v.loaded = false;
    await loadActiveConversation(activeProjectId);
    if (settingsProjectId) await renderConversations();
  } catch (e) {
    iosToast("清除失败：" + e);
  } finally {
    btn.disabled = false;
    btn.textContent = original;
    btn.dataset.busy = "";
  }
};

listen("agent://projects-changed", async () => {
  await renderProjects();
  if (settingsProjectId) await renderConversations();
});

listen("agent://project-busy", (e) => {
  const { id, busy } = e.payload;
  if (!id) return;
  if (busy) busyProjects.add(id); else busyProjects.delete(id);
  const el = projectList.querySelector('[data-pid="' + id + '"]');
  if (el) el.classList.toggle("working", busy);
});

listen("agent://handoff", () => {});
listen("agent://retry", () => {
  iosToast("回复为空，正在重试…");
});
listen("agent://auto-continue", () => {});

listen("agent://delete-progress", (e) => {
  const p = e.payload || {};
  const ov = document.getElementById("delete-overlay");
  if (!ov) return;
  const el = ov.querySelector(".delete-overlay-txt");
  if (!el) return;
  if (p.phase === "deleting") el.textContent = "删除中…（" + p.current + "/" + p.total + "）";
  else if (p.phase === "finishing") el.textContent = "正在删除项目…";
});

document.getElementById("new-project").onclick = async () => {
  await refreshStatus();
  openProjectModal("create");
};

(async () => {
  await refreshStatus();
  await renderProjects();
  let projects = [];
  try { projects = await invoke("list_projects"); } catch { projects = []; }
  if (projects.length) {
    const p = projects.find((x) => x.active) || projects[0];
    try {
      await invoke("open_project", { id: p.id });
      showProject(p.id);
      await loadActiveConversation(p.id);
      await renderProjects();
    } catch (e) { iosToast("载入项目失败：" + e); }
  } else {
    showEmptyState();
  }
})();

const dragBar = document.getElementById("titlebar-drag");
dragBar.addEventListener("mousedown", (e) => {
  if (e.buttons === 1) invoke("window_start_drag").catch(() => {});
});
document.getElementById("win-min").onclick = () => invoke("window_minimize").catch(() => {});
let allowClose = false;
if (window.__TAURI__ && window.__TAURI__.window) {
  window.__TAURI__.window.getCurrentWindow().onCloseRequested(async (event) => {
    if (allowClose) return;
    event.preventDefault();
    let busy = [];
    try { busy = await invoke("busy_projects"); } catch {}
    if (busy.length) {
      const ok = await iosConfirm("有 " + busy.length + " 个项目正在工作，确定关闭？", "关闭");
      if (!ok) return;
    }
    allowClose = true;
    invoke("window_close").catch(() => {});
  });
}
document.getElementById("win-close").onclick = async () => {
  let busy = [];
  try { busy = await invoke("busy_projects"); } catch {}
  if (busy.length) {
    const ok = await iosConfirm("有 " + busy.length + " 个项目正在工作，确定关闭？", "关闭");
    if (!ok) return;
  }
  allowClose = true;
  if (window.__TAURI__ && window.__TAURI__.window) {
    window.__TAURI__.window.getCurrentWindow().close();
  } else {
    invoke("window_close").catch(() => {});
  }
};
// ---------- 账号管理 ----------
let sessionAccount = null;

async function openManageAccounts() {
  const info = await invoke("account_status").catch(() => ({ accounts: [] }));
  const list = document.getElementById("account-list");
  list.innerHTML = "";
  const accounts = info.accounts || [];
  if (!accounts.length) {
    const d = document.createElement("div");
    d.className = "empty";
    d.textContent = "暂无账号，点击左侧“添加账号”";
    list.appendChild(d);
  } else {
    accounts.forEach(name => {
      const row = document.createElement("div");
      row.className = "account-row";
      row.innerHTML =
        '<span class="account-name">' + escapeHtml(name) + '</span>' +
        '<button class="acc-view">历史</button>' +
        '<button class="acc-del">删除</button>';
      row.querySelector(".acc-view").onclick = () => openAccountSessions(name);
      row.querySelector(".acc-del").onclick = async (ev) => {
        const btn = ev.currentTarget;
        if (btn.dataset.busy === "1") return;
        if (!confirm("删除账号 " + name + "？")) return;
        const original = btn.textContent;
        btn.dataset.busy = "1";
        btn.disabled = true;
        btn.textContent = "删除中…";
        btn.classList.add("loading");
        row.classList.add("deleting");
        try {
          await invoke("delete_account", { name });
          iosToast("已删除");
          await openManageAccounts();
          await refreshStatus();
        } catch (e) {
          iosToast("删除失败：" + e);
          btn.dataset.busy = "";
          btn.disabled = false;
          btn.textContent = original;
          btn.classList.remove("loading");
          row.classList.remove("deleting");
        }
      };
      list.appendChild(row);
    });
  }
  document.getElementById("account-modal").classList.remove("hidden");
}

async function openAccountSessions(name) {
  sessionAccount = name;
  document.getElementById("session-title").textContent = name + " 的历史对话";
  const list = document.getElementById("session-list");
  list.innerHTML = '<div class="loading">加载中…</div>';
  document.getElementById("session-modal").classList.remove("hidden");
  try {
    const sessions = await invoke("list_account_sessions", { name });
    list.innerHTML = "";
    if (!sessions.length) {
      const d = document.createElement("div");
      d.className = "empty";
      d.textContent = "无历史对话";
      list.appendChild(d);
      return;
    }
    sessions.forEach(s => {
      const row = document.createElement("label");
      row.className = "session-row";
      row.innerHTML =
        '<input type="checkbox" value="' + escapeHtml(s.id) + '">' +
        '<span class="session-title">' + escapeHtml(s.title || "(无标题)") + '</span>';
      list.appendChild(row);
    });
  } catch (e) {
    list.innerHTML = '<div class="empty">加载失败：' + escapeHtml(String(e)) + '</div>';
  }
}

document.getElementById("manage-account").onclick = openManageAccounts;
document.getElementById("account-close").onclick = () => {
  document.getElementById("account-modal").classList.add("hidden");
};
document.getElementById("session-close").onclick = () => {
  document.getElementById("session-modal").classList.add("hidden");
};
document.getElementById("session-delete").onclick = async (ev) => {
  const btn = ev.currentTarget;
  if (btn.dataset.busy === "1") return;
  const ids = [...document.querySelectorAll("#session-list input:checked")].map(i => i.value);
  if (!ids.length) { iosToast("请先选择"); return; }
  const original = btn.textContent;
  btn.dataset.busy = "1";
  btn.disabled = true;
  btn.textContent = "删除中… (" + ids.length + ")";
  btn.classList.add("loading");
  try {
    await invoke("delete_account_sessions", { name: sessionAccount, ids });
    iosToast("已删除 " + ids.length + " 个对话");
    await openAccountSessions(sessionAccount);
  } catch (e) {
    iosToast("删除失败：" + e);
  } finally {
    btn.dataset.busy = "";
    btn.disabled = false;
    btn.textContent = original;
    btn.classList.remove("loading");
  }
};