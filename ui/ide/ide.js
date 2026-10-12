// IDE 入口：项目信息、多标签编辑器、文件树、保存与快捷键。

import { createTree, fileIcon } from "./tree.js";
import { createEditor, langForPath } from "./editor.js";
import { renderPreview, previewKind } from "./viewer.js";
import {
  readFile, writeFile, tauriWindow,
  deletePath, copyPath, movePath, createPath, restorePath, purgeTrash,
  getWorkdir, setWorkdir, getRestrict, setRestrict,
  gitStatus, gitDiff, gitStage, gitUnstage, gitDiscard, gitCommit, gitPush, gitPull,
  gitBranches, gitBranchCreate, gitCheckout, gitBranchDelete, gitLog, gitShow, gitReset,
} from "./api.js";

// 剪贴板：仅记录一次复制/剪切操作的相对路径，粘贴时读取。
let clip = null; // { items: [{rel, isDir}], mode: "copy" | "cut" }

const params = new URLSearchParams(location.search);
const projectId = params.get("project") || "";
const projectName = params.get("name") || projectId;
const restrict = params.get("restrict") === "1";

const els = {
  projectName: document.getElementById("project-name"),
  dirtyFlag: document.getElementById("dirty-flag"),
  treeHost: document.getElementById("file-tree"),
  editorHost: document.getElementById("editor-host"),
  monacoHost: document.getElementById("monaco-host"),
  previewHost: document.getElementById("preview-host"),
  welcome: document.getElementById("welcome"),
  statusPath: document.getElementById("status-path"),
  statusMeta: document.getElementById("status-meta"),
  breadcrumb: document.getElementById("breadcrumb"),
  tabs: document.getElementById("tabs"),
  saveBtn: document.getElementById("save-btn"),
  undoBtn: document.getElementById("undo-btn"),
  redoBtn: document.getElementById("redo-btn"),
  refreshBtn: document.getElementById("refresh-btn"),
  upBtn: document.getElementById("up-btn"),
  searchInput: document.getElementById("file-search"),
  diffHost: document.getElementById("diff-host"),
  agentDiffBanner: document.getElementById("agent-diff-banner"),
  agentDiffTitle: document.getElementById("agent-diff-title"),
  btnDiffMode: document.getElementById("btn-diff-mode"),
  btnCloseDiff: document.getElementById("btn-close-diff"),
  gitDiffBanner: document.getElementById("git-diff-banner"),
  gitDiffTitle: document.getElementById("git-diff-title"),
  btnGitDiffMode: document.getElementById("btn-git-diff-mode"),
  btnCloseGitDiff: document.getElementById("btn-close-git-diff"),
  scmBranch: document.getElementById("scm-branch"),
  scmSync: document.getElementById("scm-sync"),
  scmToolbar: document.getElementById("scm-toolbar"),
  scmStageAll: document.getElementById("scm-stage-all"),
  scmUnstageAll: document.getElementById("scm-unstage-all"),
  activitybar: document.getElementById("activitybar"),
  explorerPane: document.getElementById("explorer-pane"),
  scmPane: document.getElementById("scm-pane"),
  actBadge: document.getElementById("act-scm-badge"),
  scmBody: document.getElementById("scm-body"),
  scmEmpty: document.getElementById("scm-empty"),
  scmGroups: document.getElementById("scm-groups"),
  scmHistory: document.getElementById("scm-history"),
  scmTabChanges: document.getElementById("scm-tab-changes"),
  scmTabHistory: document.getElementById("scm-tab-history"),
  scmCommitWrap: document.getElementById("scm-commit-wrap"),
  scmMessage: document.getElementById("scm-message"),
  scmCommitBtn: document.getElementById("scm-commit-btn"),
  scmPullBtn: document.getElementById("scm-pull-btn"),
  scmPushBtn: document.getElementById("scm-push-btn"),
  scmRefreshBtn: document.getElementById("scm-refresh-btn"),
};

// 图标
const SVG_BRANCH = '<svg viewBox="0 0 24 24" width="12" height="12" fill="none"><circle cx="7" cy="6" r="2.2" stroke="currentColor" stroke-width="1.5"/><circle cx="7" cy="18" r="2.2" stroke="currentColor" stroke-width="1.5"/><circle cx="17" cy="9" r="2.2" stroke="currentColor" stroke-width="1.5"/><path d="M7 8.2v7.6M17 11.2c0 3-2.5 4-6 4.3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>';
const SVG_CHEVRON = '<svg viewBox="0 0 24 24" width="12" height="12" fill="none"><path d="M6 9l6 6 6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const SVG_PLUS = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none"><path d="M12 5v14M5 12h14" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"/></svg>';
const SVG_MINUS = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none"><path d="M5 12h14" stroke="currentColor" stroke-width="1.9" stroke-linecap="round"/></svg>';
const SVG_DISCARD = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none"><path d="M9 7L4 12l5 5" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/><path d="M4 12h9a6 6 0 010 12h-2" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>';

let editor = null;       // Monaco editor 实例
let monaco = null;       // monaco 命名空间
let tree = null;
const openDocs = new Map(); // rel -> { model, dirty, savedValue }
let activeRel = null;       // 当前激活标签的 rel
let projectRootAbs = "";    // 项目工作目录绝对值（缓存，用于侧栏路径显示）

let diffEditor = null;
let diffActive = false;
let diffViewState = null;
const pendingAgentDiffs = new Map();
const clearedAgentDiffs = new Set();

// 侧栏当前文件夹路径：显示 项目根 + 相对位移。
function updateSidebarPath(rel) {
  const el = document.getElementById("sidebar-path");
  if (!el) return;
  const abs = joinAbs(projectRootAbs, rel);
  el.textContent = abs || "/";
  el.title = abs || "/";
}

function activeDoc() {
  return activeRel ? openDocs.get(activeRel) : null;
}

function setGlobalDirty() {
  const dirtyRels = [];
  for (const [rel, doc] of openDocs) if (doc.dirty) dirtyRels.push(rel);
  els.dirtyFlag.classList.toggle("hidden", dirtyRels.length === 0);
  if (tree) tree.setDirty(dirtyRels);
}

function updateMeta() {
  const doc = activeDoc();
  if (!activeRel || !doc) { els.statusMeta.textContent = ""; return; }
  if (doc.kind === "preview") {
    els.statusMeta.textContent = "预览";
    return;
  }
  if (!editor) { els.statusMeta.textContent = ""; return; }
  const model = editor.getModel();
  const lines = model ? model.getLineCount() : 0;
  els.statusMeta.textContent = `${lines} 行 · ${langForPath(activeRel)}`;
}

function renderTabs() {
  els.tabs.innerHTML = "";
  for (const [rel, doc] of openDocs) {
    const tab = document.createElement("div");
    tab.className = "tab" + (rel === activeRel ? " active" : "");
    tab.dataset.rel = rel;
    const name = document.createElement("span");
    name.className = "tab-name";
    name.textContent = rel.split("/").pop();
    name.title = rel;
    const dirty = document.createElement("span");
    dirty.className = "tab-dirty" + (doc.dirty ? "" : " hidden");
    dirty.textContent = "●";
    const close = document.createElement("span");
    close.className = "tab-close";
    close.textContent = "✕";
    close.title = "关闭";
    close.addEventListener("click", (e) => { e.stopPropagation(); closeTab(rel); });
    tab.append(name, dirty, close);
    tab.addEventListener("click", () => activateTab(rel));
    els.tabs.appendChild(tab);
  }
  refreshTabDecorations();
}

async function ensureEditor() {
  if (editor) return;
  els.monacoHost.classList.remove("hidden");
  const created = await createEditor(els.monacoHost);
  editor = created.editor;
  monaco = window.monaco;
  els.welcome?.remove();
  editor.onDidChangeModelContent(() => {
    const doc = activeDoc();
    if (!doc) return;
    doc.dirty = editor.getValue() !== doc.savedValue;
    setGlobalDirty();
    renderTabs();
    updateMeta();
    scheduleTextSnapshot(activeRel, doc);
  });
  ensureDiffEditor();
}

function ensureDiffEditor() {
  if (diffEditor || !monaco) return;
  diffEditor = monaco.editor.createDiffEditor(els.diffHost, {
    readOnly: true,
    originalEditable: false,
    renderSideBySide: false,
    automaticLayout: true,
    maxComputationTime: 1000,
    ignoreTrimWhitespace: true,
    fontSize: 15,
    fontFamily: "'JetBrainsMono Nerd Font', 'JetBrains Mono', 'Cascadia Code', Menlo, Consolas, monospace",
  });
  els.btnCloseDiff.onclick = closeAgentDiff;
  els.btnDiffMode.onclick = () => toggleDiffMode();
  els.btnGitDiffMode.onclick = () => toggleDiffMode();
  els.btnCloseGitDiff.onclick = () => gotoGitDiffFile();
}

function toggleDiffMode() {
  if (!diffEditor || !monaco) return;
  const cur = diffEditor.getOption(monaco.editor.EditorOption.renderSideBySide);
  diffEditor.updateOptions({ renderSideBySide: !cur });
}

function openAgentDiff(rel) {
  const d = pendingAgentDiffs.get(rel);
  if (!d) return;
  const LIMIT = 1.5 * 1024 * 1024;
  if (d.old.length > LIMIT || d.new.length > LIMIT) {
    els.statusPath.textContent = rel + "（文件过大，已禁用内联对比）";
    return;
  }
  ensureDiffEditor();
  if (!diffEditor) return;
  if (!diffActive) diffViewState = editor ? editor.saveViewState() : null;

  els.monacoHost.classList.add("hidden");
  els.previewHost.classList.add("hidden");
  els.diffHost.classList.remove("hidden");
  els.agentDiffBanner.classList.remove("hidden");

  const lang = langForPath(rel);
  const nonce = Date.now() + "_" + Math.random().toString(36).slice(2);
  const oldUri = monaco.Uri.parse(`inmemory://agent-diff/${nonce}/original/${rel}`);
  const newUri = monaco.Uri.parse(`inmemory://agent-diff/${nonce}/modified/${rel}`);
  const oldModel = monaco.editor.createModel(d.old, lang, oldUri);
  const newModel = monaco.editor.createModel(d.new, lang, newUri);
  const prev = diffEditor.getModel();
  diffEditor.setModel({ original: oldModel, modified: newModel });
  if (prev) { prev.original?.dispose(); prev.modified?.dispose(); }
  diffEditor.layout();
  diffActive = true;

  els.agentDiffTitle.textContent = "Agent 改动";
  els.statusPath.textContent = rel + "（只读 Diff）";
}

function closeAgentDiff(clearMark = true) {
  if (!diffActive) return;
  const m = diffEditor.getModel();
  diffEditor.setModel(null);
  if (m) { m.original?.dispose(); m.modified?.dispose(); }

  els.diffHost.classList.add("hidden");
  els.agentDiffBanner.classList.add("hidden");

  const doc = activeRel ? openDocs.get(activeRel) : null;
  const isPreview = doc && doc.kind === "preview";
  els.previewHost.classList.toggle("hidden", !isPreview);
  els.monacoHost.classList.toggle("hidden", isPreview);

  if (editor && !isPreview && doc && doc.kind === "text") {
    editor.setModel(doc.model);
  }

  diffActive = false;
  if (editor) {
    editor.layout();
    if (!isPreview && doc && doc.kind === "text") editor.focus();
    diffViewState = null;
  }
  if (clearMark && activeRel) markAgentDiffCleared(activeRel);
}

function markAgentDiffUnread(rel) {
  clearedAgentDiffs.delete(rel);
  renderTabs();
  refreshAgentDiffDots();
}

function markAgentDiffCleared(rel) {
  clearedAgentDiffs.add(rel);
  pendingAgentDiffs.delete(rel);
  renderTabs();
  refreshAgentDiffDots();
}

function hasUnreadAgentDiff(rel) {
  return pendingAgentDiffs.has(rel) && !clearedAgentDiffs.has(rel);
}

function refreshAgentDiffDots() {
  const host = els.treeHost;
  if (!host) return;
  const under = window.__hasUnreadAgentDiffUnder;
  host.querySelectorAll(".tree-row").forEach((row) => {
    const rel = row.dataset.rel;
    if (!rel) return;
    const isDir = row.dataset.isDir === "1";
    const unread = isDir
      ? (under ? under(rel) : false)
      : hasUnreadAgentDiff(rel);
    const existing = row.querySelector(".agent-diff-dot");
    if (unread && !existing) {
      const dot = document.createElement("span");
      dot.className = "agent-diff-dot";
      row.append(dot);
    } else if (!unread && existing) {
      existing.remove();
    }
  });
}
window.__refreshAgentDiffDots = refreshAgentDiffDots;
window.__hasUnreadAgentDiff = hasUnreadAgentDiff;
window.__hasUnreadAgentDiffUnder = (dirRel) => {
  const prefix = dirRel + "/";
  for (const rel of pendingAgentDiffs.keys()) {
    if (clearedAgentDiffs.has(rel)) continue;
    if (rel.startsWith(prefix)) return true;
  }
  return false;
};
window.__openAgentDiff = openAgentDiff;

// —— 源代码管理（Git） ——
let gitState = null;         // 最近一次 GitStatus
let gitDiffActive = false;   // 当前是否展示 Git diff
let gitDiffKey = null;       // 当前 diff 的 rel + staged 标识

// 折叠状态：分组标签 -> 是否收起
const scmCollapsed = new Set();

// rel -> { staged, worktree, untracked, conflicted }
function gitFileMap() {
  const m = new Map();
  if (gitState && gitState.files) for (const f of gitState.files) m.set(f.rel, f);
  return m;
}

// 计算状态字母与语义类别：M/A/D/R/U/C
function gitCode(gf) {
  if (!gf) return "";
  if (gf.conflicted) return "C";
  if (gf.untracked) return "U";
  const s = gf.staged, w = gf.worktree;
  const staged = s !== " " && s !== "";
  const worktree = w !== " " && w !== "";
  if (staged && worktree) return s === "A" ? "A" : "M";
  if (staged) return s;            // A/D/R/M
  if (worktree) return w === "D" ? "D" : "M";
  return "M";
}

// 供文件树/标签使用。
function gitDecor(gf) { return gitCode(gf); }

function gitStatusClass(gf) {
  if (!gf) return "";
  if (gf.conflicted) return "conflict";
  if (gf.untracked) return "untracked";
  const staged = gf.staged !== " " && gf.staged !== "";
  const worktree = gf.worktree !== " " && gf.worktree !== "";
  if (staged && !worktree) return "staged";
  return "modified";
}

// 目录状态：取其下所有文件状态的最高优先。
// 优先级：冲突 C > 更改 M/D > 新增 A > 未跟踪 U > 已暂存。
function dirGitCode(rel, map) {
  const prefix = rel + "/";
  let best = "";
  let bestScore = -1;
  for (const [frel, gf] of map) {
    if (!frel.startsWith(prefix)) continue;
    const code = gitCode(gf);
    if (!code) continue;
    let score;
    if (gf.conflicted) score = 5;
    else if (gf.untracked) score = 2;
    else {
      const staged = gf.staged !== " " && gf.staged !== "";
      const worktree = gf.worktree !== " " && gf.worktree !== "";
      if (worktree) score = 4;            // 工作区有改动
      else if (staged && gf.staged === "A") score = 3;
      else score = 1;                     // 仅暂存
    }
    if (score > bestScore) { bestScore = score; best = code; }
  }
  return best;
}

// 刷新文件树着色（不改结构，只改类名/角标）。
function refreshGitDecorations() {
  const host = els.treeHost;
  if (!host) return;
  const map = gitFileMap();
  host.querySelectorAll(".tree-row").forEach((row) => {
    const rel = row.dataset.rel;
    if (!rel) return;
    const isDir = row.dataset.isDir === "1";
    const gf = isDir ? null : map.get(rel);
    const code = isDir ? dirGitCode(rel, map) : gitCode(gf);
    const existing = row.querySelector(".git-badge");
    if (code) {
      row.dataset.gitCode = code;
      if (existing) existing.textContent = code;
      else {
        const b = document.createElement("span");
        b.className = "git-badge";
        b.textContent = code;
        row.append(b);
      }
    } else {
      delete row.dataset.gitCode;
      if (existing) existing.remove();
    }
  });
}
window.__refreshGitDecorations = refreshGitDecorations;

// 标签着色。
function refreshTabDecorations() {
  const map = gitFileMap();
  els.tabs.querySelectorAll(".tab").forEach((tab) => {
    const rel = tab.dataset.rel;
    const gf = rel ? map.get(rel) : null;
    const code = gitCode(gf);
    let badge = tab.querySelector(".tab-git");
    if (code) {
      tab.dataset.gitCode = code;
      if (!badge) {
        badge = document.createElement("span");
        badge.className = "tab-git";
        tab.querySelector(".tab-close")?.before(badge);
      }
      badge.textContent = code;
    } else {
      delete tab.dataset.gitCode;
      badge?.remove();
    }
  });
}

function scmGroupLabel(gf) {
  if (gf.conflicted) return "冲突";
  if (gf.untracked) return "更改";
  const staged = gf.staged !== " " && gf.staged !== "";
  const worktree = gf.worktree !== " " && gf.worktree !== "";
  if (staged && !worktree) return "已暂存";
  return "更改";
}

// 当前页签：changes | history
let scmTab = "changes";

function switchScmTab(tab) {
  scmTab = tab;
  els.scmTabChanges.classList.toggle("active", tab === "changes");
  els.scmTabHistory.classList.toggle("active", tab === "history");
  const isChanges = tab === "changes";
  els.scmGroups.classList.toggle("hidden", !isChanges);
  els.scmToolbar.classList.toggle("hidden", !isChanges || !gitState || !gitState.is_repo);
  els.scmHistory.classList.toggle("hidden", isChanges);
  if (isChanges) {
    renderScmGroups();
  } else {
    renderHistory();
  }
}

// 分支下拉。
let branchMenuEl = null;
function closeBranchMenu() {
  if (branchMenuEl) { branchMenuEl.remove(); branchMenuEl = null; }
}
async function openBranchMenu() {
  if (branchMenuEl) { closeBranchMenu(); return; }
  let branches = [];
  try { branches = await gitBranches(projectId); }
  catch (e) { els.statusPath.textContent = "读取分支失败：" + e; return; }

  const menu = document.createElement("div");
  menu.className = "branch-menu";
  const rect = els.scmBranch.getBoundingClientRect();
  menu.style.left = Math.max(8, rect.left) + "px";
  menu.style.top = (rect.bottom + 4) + "px";

  const head = document.createElement("div");
  head.className = "branch-menu-head";
  head.textContent = "分支";
  menu.append(head);

  for (const b of branches) {
    const item = document.createElement("div");
    item.className = "branch-item" + (b.current ? " current" : "");
    const dot = document.createElement("span");
    dot.className = "branch-check";
    dot.textContent = b.current ? "✓" : "";
    const nm = document.createElement("span");
    nm.className = "branch-name";
    nm.textContent = b.name;
    item.append(dot, nm);
    if (!b.current) {
      const del = document.createElement("button");
      del.className = "branch-del";
      del.textContent = "✕";
      del.title = "删除分支";
      del.addEventListener("click", async (e) => {
        e.stopPropagation();
        const ok = await showModal({ title: "删除分支", text: `删除本地分支「${b.name}」？`, okText: "删除", danger: true });
        if (!ok) return;
        try { await gitBranchDelete(projectId, b.name, true); closeBranchMenu(); await refreshGit(); }
        catch (err) { els.statusPath.textContent = "删除失败：" + err; }
      });
      item.append(del);
    }
    if (!b.current) {
      item.addEventListener("click", async () => {
        try { await gitCheckout(projectId, b.name); closeBranchMenu(); await refreshGit(); await tree?.refresh(); }
        catch (err) { els.statusPath.textContent = "切换失败：" + err; }
      });
    }
    menu.append(item);
  }

  const newBtn = document.createElement("div");
  newBtn.className = "branch-new";
  newBtn.textContent = "＋ 新建分支…";
  newBtn.addEventListener("click", async () => {
    closeBranchMenu();
    const name = await showModal({ title: "新建分支", input: true, text: "输入新分支名（将从当前 HEAD 创建）", okText: "创建" });
    if (!name) return;
    try { await gitBranchCreate(projectId, name); await refreshGit(); await tree?.refresh(); }
    catch (e) { els.statusPath.textContent = "创建失败：" + e; }
  });
  menu.append(newBtn);

  document.body.append(menu);
  branchMenuEl = menu;

  setTimeout(() => {
    const onDoc = (e) => {
      if (!menu.contains(e.target) && e.target !== els.scmBranch) {
        closeBranchMenu();
        document.removeEventListener("mousedown", onDoc);
      }
    };
    document.addEventListener("mousedown", onDoc);
  }, 0);
}

async function renderHistory() {
  const wrap = els.scmHistory;
  wrap.innerHTML = "";
  let commits = [];
  try { commits = await gitLog(projectId, 100); }
  catch (e) { wrap.textContent = "读取历史失败：" + e; return; }
  if (!commits.length) {
    const e = document.createElement("div");
    e.className = "scm-clean";
    e.textContent = "没有提交";
    wrap.append(e);
    return;
  }
  for (const c of commits) {
    const row = document.createElement("div");
    row.className = "hist-row";
    row.dataset.hash = c.hash;
    row.title = `${c.hash}\n${c.author}  ${c.time}`;

    const subj = document.createElement("div");
    subj.className = "hist-subj";
    subj.textContent = c.subject || "(无提交信息)";
    const meta = document.createElement("div");
    meta.className = "hist-meta";
    meta.textContent = `${c.short} · ${c.author} · ${c.time}`;
    row.append(subj, meta);
    row.addEventListener("click", () => openCommitDiff(c));
    wrap.append(row);
  }
}

// 查看某提交的 diff（整个提交）。
async function openCommitDiff(c) {
  let text;
  try { text = await gitShow(projectId, c.hash, null); }
  catch (e) { els.statusPath.textContent = "Diff 失败：" + e; return; }
  await showDiffText(`${c.short} ${c.subject}`, text, { hash: c.hash });
}

async function refreshGit() {
  let st;
  try { st = await gitStatus(projectId); }
  catch (e) { els.scmBody.textContent = "Git 状态获取失败：" + e; return; }
  gitState = st;

  if (!st.is_repo) {
    els.scmEmpty.classList.remove("hidden");
    els.scmEmpty.textContent = "未检测到 Git 仓库";
    els.scmGroups.innerHTML = "";
    els.scmCommitWrap.classList.add("hidden");
    els.scmBranch.classList.add("hidden");
    els.scmSync.classList.add("hidden");
    els.scmToolbar.classList.add("hidden");
    refreshGitDecorations();
    refreshTabDecorations();
    return;
  }
  els.scmEmpty.classList.add("hidden");
  els.scmCommitWrap.classList.remove("hidden");
  els.scmBranch.classList.remove("hidden");
  els.scmToolbar.classList.remove("hidden");
  els.scmBranch.innerHTML = SVG_BRANCH + "<span>" + (st.branch || "(detached)") + "</span>";
  els.scmBranch.title = st.upstream ? `上游：${st.upstream}` : "无上游";

  // 同步指示
  if (st.ahead || st.behind) {
    els.scmSync.classList.remove("hidden");
    els.scmSync.innerHTML =
      (st.behind ? `<span class="behind">↓${st.behind}</span>` : "") +
      (st.ahead ? `<span class="ahead">↑${st.ahead}</span>` : "");
    els.scmSync.title = `领先 ${st.ahead} / 落后 ${st.behind}`;
  } else {
    els.scmSync.classList.add("hidden");
  }

  switchScmTab(scmTab);
  refreshGitDecorations();
  refreshTabDecorations();
  updateActBadge();
}

function renderScmGroups() {
  const wrap = els.scmGroups;
  wrap.innerHTML = "";
  if (!gitState || !gitState.files.length) {
    const e = document.createElement("div");
    e.className = "scm-clean";
    e.textContent = "没有更改";
    wrap.append(e);
    return;
  }
  // 按组聚合：冲突 / 已暂存 / 更改。
  const order = ["冲突", "已暂存", "更改"];
  const groups = new Map();
  for (const f of gitState.files) {
    const g = scmGroupLabel(f);
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(f);
  }
  for (const label of order) {
    const files = groups.get(label);
    if (!files || !files.length) continue;
    const collapsed = scmCollapsed.has(label);
    const h = document.createElement("div");
    h.className = "scm-group-head" + (collapsed ? " collapsed" : "");
    h.innerHTML = `<span class="chev">${SVG_CHEVRON}</span><span>${label}</span><span class="cnt">${files.length}</span>`;
    h.addEventListener("click", () => {
      if (scmCollapsed.has(label)) scmCollapsed.delete(label); else scmCollapsed.add(label);
      renderScmGroups();
    });
    wrap.append(h);
    if (!collapsed) for (const f of files) wrap.append(scmRow(f, label));
  }
}

function scmRow(f, groupLabel) {
  const row = document.createElement("div");
  row.className = "scm-row";
  row.dataset.rel = f.rel;

  // 文件类型图标
  const icon = document.createElement("span");
  icon.className = "scm-icon";
  const ic = fileIcon(f.rel.split("/").pop());
  icon.innerHTML = ic.svg;
  icon.style.color = ic.color;
  row.append(icon);

  // 文件名
  const name = document.createElement("span");
  name.className = "scm-name";
  const slash = f.rel.lastIndexOf("/");
  name.textContent = slash >= 0 ? f.rel.slice(slash + 1) : f.rel;
  name.title = f.rel;
  row.append(name);

  // 目录
  const dir = slash >= 0 ? f.rel.slice(0, slash) : "";
  if (dir) {
    const d = document.createElement("span");
    d.className = "scm-dir";
    d.textContent = dir;
    row.append(d);
  }

  // hover 操作按钮
  const acts = document.createElement("span");
  acts.className = "scm-acts";
  const stagedGroup = groupLabel === "已暂存";
  const mkBtn = (svg, title, fn) => {
    const b = document.createElement("button");
    b.className = "scm-act";
    b.innerHTML = svg;
    b.title = title;
    b.addEventListener("click", (e) => { e.stopPropagation(); fn(); });
    return b;
  };
  acts.append(mkBtn(stagedGroup ? SVG_MINUS : SVG_PLUS, stagedGroup ? "取消暂存" : "暂存", async () => {
    if (stagedGroup) await gitUnstage(projectId, f.rel); else await gitStage(projectId, f.rel);
    await refreshGit();
  }));
  acts.append(mkBtn(SVG_DISCARD, "丢弃更改", async () => {
    const ok = await showModal({ title: "丢弃更改", text: `确定丢弃「${f.rel}」的更改？此操作不可恢复。`, okText: "丢弃", danger: true });
    if (!ok) return;
    await gitDiscard(projectId, f.rel);
    await refreshGit();
    if (gitDiffActive && gitDiffKey && gitDiffKey.startsWith(f.rel + "|")) closeGitDiff();
  }));
  row.append(acts);

  // 状态字母徽章
  const code = document.createElement("span");
  code.className = "scm-code " + gitCode(f);
  code.textContent = gitCode(f);
  code.title = "暂存:" + (f.staged === " " ? "·" : f.staged) + "  工作区:" + (f.worktree === " " ? "·" : f.worktree);
  row.append(code);

  row.addEventListener("click", () => openGitDiff(f.rel, stagedGroup));
  return row;
}

async function openGitDiff(rel, staged) {
  let text;
  try { text = await gitDiff(projectId, rel, staged); }
  catch (e) { els.statusPath.textContent = "Diff 失败：" + e; return; }
  await showDiffText(`${rel}（${staged ? "已暂存" : "工作区"}）`, text, {
    jumpRel: rel,
    key: rel + "|" + (staged ? "staged" : "worktree"),
  });
}

// 通用 diff 展示：`opts.jumpRel` 有值则 banner 显示「跳转到文件」，否则隐藏该按钮。
async function showDiffText(title, text, opts = {}) {
  await ensureEditor();
  ensureDiffEditor();
  if (!diffEditor || !monaco) return;
  if (!gitDiffActive && !diffActive) diffViewState = editor ? editor.saveViewState() : null;

  els.monacoHost.classList.add("hidden");
  els.previewHost.classList.add("hidden");
  els.diffHost.classList.remove("hidden");
  els.gitDiffBanner.classList.remove("hidden");
  els.agentDiffBanner.classList.add("hidden");

  const relForLang = opts.jumpRel || title;
  const lang = langForPath(relForLang);
  const nonce = Date.now() + "_" + Math.random().toString(36).slice(2);
  const oldUri = monaco.Uri.parse(`inmemory://git-diff/${nonce}/original`);
  const newUri = monaco.Uri.parse(`inmemory://git-diff/${nonce}/modified`);
  const { original, modified } = splitUnifiedDiff(text, relForLang);
  const oldModel = monaco.editor.createModel(original, lang, oldUri);
  const newModel = monaco.editor.createModel(modified, lang, newUri);
  const prev = diffEditor.getModel();
  diffEditor.setModel({ original: oldModel, modified: newModel });
  if (prev) { prev.original?.dispose(); prev.modified?.dispose(); }
  diffEditor.layout();
  gitDiffActive = true;
  gitDiffKey = opts.key || null;
  els.gitDiffTitle.textContent = title;
  els.statusPath.textContent = title;
  els.btnCloseGitDiff.style.display = opts.jumpRel ? "" : "none";
  els.btnCloseGitDiff.textContent = "跳转到文件";
}

// 极简 unified diff 还原：无 diff 头时把文本当修改侧。
function splitUnifiedDiff(text, rel) {
  if (!text || text.indexOf("@@") === -1) return { original: "", modified: text || "" };
  const oldLines = [];
  const newLines = [];
  for (const line of text.split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") ||
        line.startsWith("index ") || line.startsWith("new file") || line.startsWith("deleted file") ||
        line.startsWith("similarity") || line.startsWith("rename ") || line.startsWith("old mode") ||
        line.startsWith("new mode")) continue;
    if (line.startsWith("@@")) continue;
    if (line.startsWith("+")) newLines.push(line.slice(1));
    else if (line.startsWith("-")) oldLines.push(line.slice(1));
    else if (line.startsWith(" ")) { oldLines.push(line.slice(1)); newLines.push(line.slice(1)); }
    else oldLines.push(line);
  }
  return { original: oldLines.join("\n"), modified: newLines.join("\n") };
}

// 从 Git diff 跳转到该文件：若存在 Agent diff 则先进入 Agent diff，否则直接打开文件。
async function gotoGitDiffFile() {
  const rel = gitDiffKey ? gitDiffKey.split("|")[0] : null;
  closeGitDiff();
  if (!rel) return;
  const name = rel.split("/").pop();
  // 先登记为激活文档，保证关闭 Agent diff 后能回到该文件的编辑界面。
  await openFile(rel, name);
  if (pendingAgentDiffs.has(rel)) openAgentDiff(rel);
  if (tree && tree.reveal) await tree.reveal(rel);
}

function closeGitDiff() {
  if (!gitDiffActive) return;
  const m = diffEditor.getModel();
  diffEditor.setModel(null);
  if (m) { m.original?.dispose(); m.modified?.dispose(); }
  els.diffHost.classList.add("hidden");
  els.gitDiffBanner.classList.add("hidden");
  const doc = activeRel ? openDocs.get(activeRel) : null;
  const isPreview = doc && doc.kind === "preview";
  els.previewHost.classList.toggle("hidden", !isPreview);
  els.monacoHost.classList.toggle("hidden", isPreview);
  if (editor && !isPreview && doc && doc.kind === "text") editor.setModel(doc.model);
  gitDiffActive = false;
  gitDiffKey = null;
  if (editor) { editor.layout(); diffViewState = null; }
}

// 活动栏：切换 资源管理器 / 源代码管理 面板（一次显示一个）。
let activeView = "explorer";
function switchView(view) {
  activeView = view;
  els.activitybar.querySelectorAll(".act-item").forEach((b) => {
    b.classList.toggle("active", b.dataset.view === view);
  });
  els.explorerPane.classList.toggle("hidden", view !== "explorer");
  els.scmPane.classList.toggle("hidden", view !== "scm");
  if (view === "scm") refreshGit();
}

// 活动栏 SCM 图标上的更改数角标。
function updateActBadge() {
  const n = gitState && gitState.is_repo ? gitState.files.length : 0;
  els.actBadge.classList.toggle("hidden", n === 0);
  els.actBadge.textContent = n > 99 ? "99+" : String(n);
}

function installScm() {
  els.activitybar?.querySelectorAll(".act-item").forEach((b) => {
    b.addEventListener("click", () => switchView(b.dataset.view));
  });
  els.scmRefreshBtn?.addEventListener("click", refreshGit);
  els.scmTabChanges?.addEventListener("click", () => switchScmTab("changes"));
  els.scmTabHistory?.addEventListener("click", () => switchScmTab("history"));
  els.scmBranch?.addEventListener("click", openBranchMenu);
  els.scmCommitBtn?.addEventListener("click", doCommit);
  els.scmPullBtn?.addEventListener("click", doPull);
  els.scmPushBtn?.addEventListener("click", doPush);
  els.scmStageAll?.addEventListener("click", async () => {
    try { await gitStage(projectId, null); await refreshGit(); }
    catch (e) { els.statusPath.textContent = "暂存失败：" + e; }
  });
  els.scmUnstageAll?.addEventListener("click", async () => {
    try { await gitUnstage(projectId, null); await refreshGit(); }
    catch (e) { els.statusPath.textContent = "取消暂存失败：" + e; }
  });
  els.scmMessage?.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); doCommit(); }
  });
}

async function doCommit() {
  const msg = (els.scmMessage.value || "").trim();
  if (!msg) { els.statusPath.textContent = "请输入提交信息"; return; }
  const hasStaged = gitState && gitState.files.some((f) => f.staged !== " " && f.staged !== "" && !f.untracked);
  try {
    if (!hasStaged && gitState && gitState.files.length) await gitStage(projectId, null);
    await gitCommit(projectId, msg);
    els.scmMessage.value = "";
    els.statusPath.textContent = "已提交";
    await refreshGit();
    await tree?.refresh();
  } catch (e) { els.statusPath.textContent = "提交失败：" + e; }
}

async function doPush() {
  els.statusPath.textContent = "推送中…";
  try { await gitPush(projectId); els.statusPath.textContent = "推送完成"; await refreshGit(); }
  catch (e) { els.statusPath.textContent = "推送失败：" + e; }
}

async function doPull() {
  els.statusPath.textContent = "拉取中…";
  try { await gitPull(projectId); els.statusPath.textContent = "拉取完成"; await refreshGit(); await tree?.refresh(); }
  catch (e) { els.statusPath.textContent = "拉取失败：" + e; }
}

// —— 应用级统一历史（文本 + 文件操作，按发生顺序） ——
const history = [];
let hIndex = -1;
let applyingHistory = false; // 回退/重放期间抑制新记录

function pushHistory(rec) {
  if (applyingHistory) return;
  history.splice(hIndex + 1);
  history.push(rec);
  hIndex = history.length - 1;
}

// 文本快照：debounce 500ms，把「上次快照 → 当前值」记为一次编辑。
const lastSnap = new Map(); // rel -> value
const snapTimers = new Map(); // rel -> timer

function scheduleTextSnapshot(rel, doc) {
  if (!rel || applyingHistory) return;
  clearTimeout(snapTimers.get(rel));
  snapTimers.set(rel, setTimeout(() => {
    snapTimers.delete(rel);
    commitTextSnapshot(rel, doc);
  }, 500));
}

function commitTextSnapshot(rel, doc) {
  const cur = doc.model.getValue();
  const before = lastSnap.has(rel) ? lastSnap.get(rel) : doc.savedValue;
  if (cur === before) return;
  lastSnap.set(rel, cur);
  pushHistory({
    label: "编辑 " + rel,
    undo: async () => { doc.model.setValue(before); afterSetValue(rel, doc); },
    redo: async () => { doc.model.setValue(cur); afterSetValue(rel, doc); },
  });
}

// setValue 之后同步脏标记/快照（避免把回退自身记入历史）。
function afterSetValue(rel, doc) {
  lastSnap.set(rel, doc.model.getValue());
  doc.dirty = doc.model.getValue() !== doc.savedValue;
  setGlobalDirty();
  renderTabs();
  updateMeta();
}

async function doUndo() {
  if (hIndex <0) return;
  const rec = history[hIndex];
  applyingHistory = true;
  try { await rec.undo(); } catch (e) { els.statusPath.textContent = "撤回失败：" + e; }
  applyingHistory = false;
  hIndex--;
  els.statusPath.textContent = "已撤回：" + rec.label;
}

async function doRedo() {
  if (hIndex >= history.length - 1) return;
  hIndex++;
  const rec = history[hIndex];
  applyingHistory = true;
  try { await rec.redo(); } catch (e) { els.statusPath.textContent = "前进失败：" + e; }
  applyingHistory = false;
  els.statusPath.textContent = "已前进：" + rec.label;
}

window.__ideUndo = doUndo;
window.__ideRedo = doRedo;

async function openFile(rel, name) {
  if (openDocs.has(rel)) { activateTab(rel); return; }

  const kind = previewKind(name);
  if (kind) {
    const dispose = await mountPreview(rel, name);
    openDocs.set(rel, { kind: "preview", dirty: false, dispose });
    activateTab(rel);
    return;
  }

  await ensureEditor();
  let content;
  try {
    content = await readFile(projectId, rel);
  } catch (e) {
    els.statusPath.textContent = "打开失败：" + e;
    return;
  }
  const lang = langForPath(rel);
  const uri = monaco.Uri.file(rel);
  let model = monaco.editor.getModel(uri);
  if (!model) model = monaco.editor.createModel(content, lang, uri);
  openDocs.set(rel, { kind: "text", model, dirty: false, savedValue: content });
  lastSnap.set(rel, content);
  activateTab(rel);
}

// 挂载预览到 preview-host，返回 dispose。若已挂载同文件则复用。
let mountedPreviewRel = null;
async function mountPreview(rel, name) {
  if (mountedPreviewRel === rel) return () => {};
  els.previewHost.innerHTML = "";
  mountedPreviewRel = rel;
  const res = await renderPreview(els.previewHost, projectId, rel, name);
  return () => {
    if (mountedPreviewRel === rel) mountedPreviewRel = null;
    res.dispose();
  };
}

function activateTab(rel) {
  if (diffActive) closeAgentDiff(false);
  if (gitDiffActive) closeGitDiff();
  const doc = openDocs.get(rel);
  if (!doc) return;
  activeRel = rel;
  els.statusPath.textContent = rel;
  els.breadcrumb.textContent = rel;

  const isPreview = doc.kind === "preview";
  els.previewHost.classList.toggle("hidden", !isPreview);
  els.monacoHost.classList.toggle("hidden", isPreview);
  if (isPreview) {
    if (editor) editor.setModel(null);
  } else {
    editor.setModel(doc.model);
  }

  renderTabs();
  updateMeta();
  if (!isPreview && editor) editor.focus();
}

async function closeTab(rel) {
  const doc = openDocs.get(rel);
  if (!doc) return;
  if (doc.dirty) {
    const ok = await showModal({
      title: "未保存的修改",
      text: `「${rel.split("/").pop()}」有未保存的修改，仍要关闭吗？`,
      okText: "关闭",
      danger: true,
    });
    if (!ok) return;
  }
  doc.dispose?.();
  doc.model?.dispose?.();
  openDocs.delete(rel);
  if (activeRel === rel) {
    const next = openDocs.keys().next().value || null;
    activeRel = next;
    if (next) activateTab(next);
    else {
      editor?.setModel(null);
      els.previewHost.classList.add("hidden");
      els.monacoHost.classList.add("hidden");
      els.welcome?.classList.remove("hidden");
      els.statusPath.textContent = "就绪";
      els.breadcrumb.textContent = "";
      els.statusMeta.textContent = "";
    }
  }
  renderTabs();
  setGlobalDirty();
}

async function save() {
  const doc = activeDoc();
  if (!doc || !activeRel || doc.kind === "preview") return;
  const value = doc.model.getValue();
  try {
    await writeFile(projectId, activeRel, value);
    doc.savedValue = value;
    doc.dirty = false;
    lastSnap.set(activeRel, value);
    setGlobalDirty();
    renderTabs();
    els.statusPath.textContent = activeRel + "（已保存）";
    markAgentDiffCleared(activeRel);
    if (gitState && gitState.is_repo) refreshGit().catch(() => {});
  } catch (e) {
    els.statusPath.textContent = "保存失败：" + e;
  }
}

// —— 自定义模态弹窗 ——
// 返回 Promise：输入框返回字符串或 null；确认框返回 true/false。
function showModal({ title, input, text, okText, danger, defaultValue }) {
  return new Promise((resolve) => {
    const ov = document.getElementById("modal-overlay");
    const tEl = document.getElementById("modal-title");
    const iEl = document.getElementById("modal-input");
    const xEl = document.getElementById("modal-text");
    const okEl = document.getElementById("modal-ok");
    const cancelEl = document.getElementById("modal-cancel");

    tEl.textContent = title || "";
    if (text) { xEl.textContent = text; xEl.classList.remove("hidden"); }
    else xEl.classList.add("hidden");

    if (input) {
      iEl.classList.remove("hidden");
      iEl.value = defaultValue || "";
    } else {
      iEl.classList.add("hidden");
      iEl.value = "";
    }

    okEl.textContent = okText || "确定";
    okEl.classList.toggle("danger", !!danger);
    okEl.classList.toggle("primary", !danger);

    ov.classList.remove("hidden");

    const done = (val) => {
      ov.classList.add("hidden");
      okEl.onclick = null;
      cancelEl.onclick = null;
      iEl.onkeydown = null;
      ov.onmousedown = null;
      resolve(val);
    };
    const submit = () => {
      if (input) {
        const v = iEl.value.trim();
        if (!v) { iEl.focus(); return; }
        done(v);
      } else done(true);
    };
    okEl.onclick = submit;
    cancelEl.onclick = () => done(input ? null : false);
    iEl.onkeydown = (e) => {
      if (e.key === "Enter") { e.preventDefault(); submit(); }
      if (e.key === "Escape") { e.preventDefault(); done(input ? null : false); }
    };
    ov.onmousedown = (e) => { if (e.target === ov) done(input ? null : false); };

    setTimeout(() => { if (input) { iEl.focus(); iEl.select(); } }, 30);
  });
}

// —— 上一层：切树根并把项目工作目录同步过去，主 UI 顶栏会实时跟随 ——
async function goUpAndSync() {
  if (restrict) { els.statusPath.textContent = "已开启目录限制，无法浏览上一层"; return; }
  const before = tree.getRoot();
  const after = normRelLocal(before + "/..");
  if (after === before) { els.statusPath.textContent = "已到顶层"; return; }
  // 先更新工作目录，再切树刷新：确保 refresh 时 listDir 使用新 root，
  // 这样 entry.rel 才是相对新工作目录的路径（否则会多带一层 ../）。
  try {
    const cur = await getWorkdir(projectId);
    const abs = newAbsFromRel(cur, before, after);
    await setWorkdir(projectId, abs);
    projectRootAbs = abs;
    await tree.setRoot(after); // 内部已 refresh，勿重复调用
    updateSidebarPath(after);
    els.statusPath.textContent = "工作目录 → " + abs;
  } catch (e) {
    els.statusPath.textContent = "上一层失败：" + e;
  }
}

function normRelLocal(rel) {
  const parts = (rel || "").split("/").filter((p) => p && p !== ".");
  const out = [];
  for (const p of parts) {
    if (p === "..") { if (out.length) out.pop(); else out.push(".."); }
    else out.push(p);
  }
  return out.join("/");
}

// 由“原工作目录”和相对路径位移，推出新的绝对目录。
function newAbsFromRel(curAbs, fromRel, toRel) {
  const parts = curAbs.split("/");
  const a = (fromRel || "").split("/").filter(Boolean);
  const b = (toRel || "").split("/").filter(Boolean);
  // 去掉 fromRel 部分
  parts.splice(parts.length - a.length, a.length);
  for (const seg of b) {
    if (seg === "..") parts.pop();
    else parts.push(seg);
  }
  let out = parts.join("/");
  if (!out.startsWith("/")) out = "/" + out;
  return out;
}

// —— 右键菜单 ——
let menuEl = null;
let menuTarget = null; // { rel, isDir }

function ensureMenu() {
  if (menuEl) return menuEl;
  menuEl = document.createElement("div");
  menuEl.className = "ctx-menu hidden";
  document.body.appendChild(menuEl);
  window.addEventListener("mousedown", (e) => {
    if (menuEl && !menuEl.contains(e.target)) hideMenu();
  });
  window.addEventListener("blur", hideMenu);
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideMenu(); });
  return menuEl;
}

function hideMenu() { if (menuEl) menuEl.classList.add("hidden"); }

function menuItem(label, onClick, opts = {}) {
  const it = document.createElement("div");
  it.className = "ctx-item" + (opts.disabled ? " disabled" : "");
  it.textContent = label;
  if (!opts.disabled) it.addEventListener("click", () => { hideMenu(); onClick(); });
  return it;
}

function showContextMenu(entry, x, y) {
  const m = ensureMenu();
  menuTarget = { rel: entry.rel, isDir: entry.is_dir };
  m.innerHTML = "";

  if (entry.is_dir) {
    m.append(
      menuItem("设为项目文件夹", () => setAsProjectFolder(entry.rel)),
      menuItem("在此新建文件", () => newEntry(entry.rel, false)),
      menuItem("在此新建文件夹", () => newEntry(entry.rel, true)),
    );
  } else {
    m.append(
      menuItem("打开", () => openFile(entry.rel, entry.name)),
    );
  }
  m.append(sep());
  const sel = tree.getSelection();
  const items = sel.length ? sel : [{ rel: entry.rel, isDir: entry.is_dir }];
  m.append(
    menuItem("复制", () => { clip = { items, mode: "copy" }; els.statusPath.textContent = items.length === 1 ? "已复制：" + items[0].rel : `已复制 ${items.length} 个项目`; tree.clearSelection(); }),
    menuItem("剪切", () => { clip = { items, mode: "cut" }; els.statusPath.textContent = items.length === 1 ? "已剪切：" + items[0].rel : `已剪切 ${items.length} 个项目`; tree.clearSelection(); }),
  );
  const pasteDir = entry.is_dir ? entry.rel : parentOf(entry.rel);
  m.append(menuItem("粘贴到此处", () => pasteInto(pasteDir), { disabled: !clip }));
  m.append(sep());
  m.append(menuItem("重命名", () => tree.startRename(entry.rel)));
  m.append(menuItem("删除", () => removeEntry(entry), { danger: true }));

  m.classList.remove("hidden");
  // 位置钳制，避免超出视口。
  const pad = 6;
  const r = m.getBoundingClientRect();
  const px = Math.min(x, window.innerWidth - r.width - pad);
  const py = Math.min(y, window.innerHeight - r.height - pad);
  m.style.left = px + "px";
  m.style.top = py + "px";
}

function sep() { const d = document.createElement("div"); d.className = "ctx-sep"; return d; }
function parentOf(rel) { const i = rel.lastIndexOf("/"); return i <0 ? "" : rel.slice(0, i); }

function installContextMenu() {
  // 空白处右键：粘贴到当前根。
  els.treeHost.addEventListener("contextmenu", (e) => {
    if (e.target.closest(".tree-row")) return;
    e.preventDefault();
    const m = ensureMenu();
    menuTarget = { rel: tree.getRoot(), isDir: true };
    m.innerHTML = "";
    m.append(
      menuItem("在此新建文件", () => newEntry(tree.getRoot(), false)),
      menuItem("在此新建文件夹", () => newEntry(tree.getRoot(), true)),
      menuItem("粘贴到此处", () => pasteInto(tree.getRoot()), { disabled: !clip }),
    );
    m.classList.remove("hidden");
    const r = m.getBoundingClientRect();
    m.style.left = Math.min(e.clientX, window.innerWidth - r.width - 6) + "px";
    m.style.top = Math.min(e.clientY, window.innerHeight - r.height - 6) + "px";
  });
}

async function setAsProjectFolder(rel) {
  try {
    const cur = await getWorkdir(projectId);
    const abs = joinAbs(cur, rel);
    await setWorkdir(projectId, abs);
    projectRootAbs = abs;
    await tree.setRoot("");
    updateSidebarPath("");
    els.statusPath.textContent = "项目文件夹 → " + abs;
  } catch (e) {
    els.statusPath.textContent = "设置失败：" + e;
  }
}

function joinAbs(curAbs, rel) {
  const parts = curAbs.split("/").filter(Boolean);
  for (const seg of (rel || "").split("/").filter(Boolean)) {
    if (seg === "..") parts.pop();
    else parts.push(seg);
  }
  return "/" + parts.join("/");
}

async function newEntry(parentDir, isDir) {
  const name = await showModal({
    title: isDir ? "新建文件夹" : "新建文件",
    input: true,
    okText: "创建",
    defaultValue: isDir ? "新建文件夹" : "untitled.txt",
  });
  if (!name) return;
  try {
    const rel = await createPath(projectId, parentDir, name, isDir);
    els.statusPath.textContent = "已创建：" + rel;
    await tree.refresh();
    if (!isDir) openFile(rel, name);
    pushHistory({
      label: (isDir ? "新建文件夹 " : "新建文件 ") + rel,
      undo: async () => { closeTabsUnder(rel); await deletePath(projectId, rel); await tree.refresh(); },
      redo: async () => { await createPath(projectId, parentDir, name, isDir); await tree.refresh(); },
    });
  } catch (e) {
    els.statusPath.textContent = "创建失败：" + e;
  }
}

async function removeEntry(entry) {
  // 多选：对当前选中集合批量删除（右键未选中时会先单选，故至少有一项）。
  const sel = tree.getSelection();
  const targets = sel.length ? sel : [{ rel: entry.rel, isDir: entry.is_dir }];
  const only = targets.length === 1;
  const ok = await showModal({
    title: only ? `删除${targets[0].isDir ? "文件夹" : "文件"}` : `删除 ${targets.length} 个项目`,
    text: only ? `确定删除「${targets[0].rel}」吗？此操作不可撤销。` : `确定删除选中的 ${targets.length} 个项目吗？此操作不可撤销。`,
    okText: "删除",
    danger: true,
  });
  if (!ok) return;
  const deleted = []; // { rel, trashRel }
  try {
    for (const t of targets) {
      const trashRel = await deletePath(projectId, t.rel);
      deleted.push({ rel: t.rel, trashRel });
      closeTabsUnder(t.rel);
    }
    els.statusPath.textContent = only ? "已删除：" + targets[0].rel : `已删除 ${deleted.length} 个项目`;
    tree.clearSelection();
    await tree.refresh();
    pushHistory({
      label: only ? "删除 " + targets[0].rel : `删除 ${deleted.length} 个项目`,
      undo: async () => { for (const d of deleted) await restorePath(projectId, d.trashRel, d.rel); await tree.refresh(); },
      redo: async () => { for (const d of deleted) await deletePath(projectId, d.rel); await tree.refresh(); },
    });
  } catch (e) {
    els.statusPath.textContent = "删除失败：" + e;
  }
}

function closeTabsUnder(rel) {
  const prefix = rel + "/";
  for (const key of [...openDocs.keys()]) {
    if (key === rel || key.startsWith(prefix)) closeTab(key);
  }
}

async function pasteInto(destDir) {
  if (!clip) return;
  try {
    if (clip.mode === "cut") {
      const moved = []; // { from, to }
      for (const item of clip.items) {
        const rel = await movePath(projectId, item.rel, destDir);
        moved.push({ from: item.rel, to: rel });
        closeTabsUnder(item.rel);
      }
      clip = null;
      els.statusPath.textContent = moved.length === 1 ? "已移动到：" + moved[0].to : `已移动 ${moved.length} 个项目`;
      await tree.refresh();
      pushHistory({
        label: moved.length === 1 ? "移动 " + moved[0].from : `移动 ${moved.length} 个项目`,
        undo: async () => { for (const m of moved) await movePath(projectId, m.to, parentOf(m.from)); await tree.refresh(); },
        redo: async () => { for (const m of moved) await movePath(projectId, m.from, destDir); await tree.refresh(); },
      });
    } else {
      const created = []; // { from, to }
      for (const item of clip.items) {
        const rel = await copyPath(projectId, item.rel, destDir);
        created.push({ from: item.rel, to: rel });
      }
      els.statusPath.textContent = created.length === 1 ? "已复制到：" + created[0].to : `已复制 ${created.length} 个项目`;
      await tree.refresh();
      pushHistory({
        label: created.length === 1 ? "复制 " + created[0].from : `复制 ${created.length} 个项目`,
        undo: async () => { for (const c of created) await deletePath(projectId, c.to); await tree.refresh(); },
        redo: async () => { for (const c of created) await copyPath(projectId, c.from, destDir); await tree.refresh(); },
      });
    }
  } catch (e) {
    els.statusPath.textContent = "粘贴失败：" + e;
  }
}



async function boot() {
  if (!projectId) { els.statusPath.textContent = "缺少 project 参数"; return; }
  els.projectName.textContent = projectName;

  try { projectRootAbs = await getWorkdir(projectId); } catch { projectRootAbs = ""; }

  tree = createTree(els.treeHost, projectId, (rel, name) => {
    if (hasUnreadAgentDiff(rel)) {
      openFile(rel, name).then(() => openAgentDiff(rel));
    } else {
      openFile(rel, name);
    }
  }, restrict, {
    onContextMenu: showContextMenu,
    onRootChange: (rel) => updateSidebarPath(rel),
    onRename: async (rel, dir, newName) => {
      try {
        const oldName = rel.split("/").pop();
        const moved = await movePath(projectId, rel, dir, newName);
        els.statusPath.textContent = "已重命名为：" + moved;
        closeTabsUnder(rel);
        await tree.refresh();
        pushHistory({
          label: "重命名 " + rel,
          undo: async () => { await movePath(projectId, moved, dir, oldName); await tree.refresh(); },
          redo: async () => { await movePath(projectId, rel, dir, newName); await tree.refresh(); },
        });
        return true;
      } catch (e) {
        els.statusPath.textContent = "重命名失败：" + e;
        return false;
      }
    },
    onMove: async (src, destDir) => {
      try {
        const rel = await movePath(projectId, src, destDir);
        els.statusPath.textContent = "已移动：" + rel;
        closeTabsUnder(src);
        await tree.refresh();
        pushHistory({
          label: "移动 " + src,
          undo: async () => { await movePath(projectId, rel, parentOf(src)); await tree.refresh(); },
          redo: async () => { await movePath(projectId, src, destDir); await tree.refresh(); },
        });
      } catch (e) {
        els.statusPath.textContent = "移动失败：" + e;
      }
    },
  });
  await tree.refresh();

  els.refreshBtn.onclick = () => tree.refresh();
  els.upBtn.onclick = () => goUpAndSync();
  els.saveBtn.onclick = save;
  els.undoBtn.onclick = () => doUndo();
  els.redoBtn.onclick = () => doRedo();
  installContextMenu();

  // 搜索：防抖
  let searchTimer = null;
  els.searchInput.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => tree.search(els.searchInput.value), 180);
  });

  window.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      if (diffActive) { els.statusPath.textContent = "只读 Diff 模式不可保存，请返回编辑"; return; }
      save();
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "w") { e.preventDefault(); if (activeRel) closeTab(activeRel); }
  });

  document.querySelectorAll("[data-win]").forEach((b) => {
    b.onclick = () => {
      const a = b.getAttribute("data-win");
      if (a === "close") { purgeTrash(projectId).catch(() => {}); tauriWindow.close(); }
      else if (a === "min") tauriWindow.minimize();
      else if (a === "max") tauriWindow.toggleMaximize();
    };
  });
  document.getElementById("titlebar").addEventListener("mousedown", (e) => {
    if (e.target.closest("button")) return;
    tauriWindow.startDragging();
  });

  const splitter = document.getElementById("splitter");
  const sidebar = document.getElementById("sidebar");
  let dragging = false;
  splitter.addEventListener("mousedown", () => { dragging = true; document.body.style.userSelect = "none"; });
  window.addEventListener("mousemove", (e) => {
    if (!dragging) return;
    sidebar.style.width = Math.max(160, Math.min(560, e.clientX)) + "px";
  });
  window.addEventListener("mouseup", () => { dragging = false; document.body.style.userSelect = ""; });

  installScm();
  installFsWatch();
  installAgentDiffWatch();
  refreshGit();
}

function installAgentDiffWatch() {
  const tauriEvent = window.__TAURI__ && window.__TAURI__.event;
  if (!tauriEvent) return;
  tauriEvent.listen("ide://agent-diff", (ev) => {
    const p = ev.payload || {};
    if (p.project_id && p.project_id !== projectId) return;
    const rel = p.rel;
    if (!rel) return;

    const prev = pendingAgentDiffs.get(rel);
    const merged = {
      old: prev ? prev.old : (p.old_content || ""),
      new: p.new_content || "",
      ts: p.timestamp || Date.now(),
      toolCallId: p.tool_call_id || "",
    };
    pendingAgentDiffs.set(rel, merged);
    clearedAgentDiffs.delete(rel);
    markAgentDiffUnread(rel);

    if (tree && tree.applyChanges) {
      const row = document.querySelector('.tree-row[data-rel="' + CSS.escape(rel) + '"]');
      if (!row) tree.applyChanges([{ rel, kind: "create", is_dir: false }]);
    }

    if (diffActive && activeRel === rel) {
      els.agentDiffTitle.textContent = "Agent 已更新，返回后可重开查看";
    }
  });
}

// —— 外部文件变化同步 ——
// 后端会以 ide://fs-changes 批量推送；前端在窗口内聚合后刷新文件树与已打开文件。
function installFsWatch() {
  const tauriEvent = window.__TAURI__ && window.__TAURI__.event;
  if (!tauriEvent) return;

  let timer = null;
  let flushing = false;
  const pendingChanges = new Map();
  const pendingDocs = new Map();

  tauriEvent.listen("ide://fs-changes", (ev) => {
    const list = Array.isArray(ev.payload) ? ev.payload : [ev.payload || {}];
    for (const p of list) {
      if (p.project_id && p.project_id !== projectId) continue;
      if (p.kind === "create" || p.kind === "remove") {
        pendingChanges.set(p.rel, { rel: p.rel, kind: p.kind, is_dir: !!p.is_dir });
      }
      if (!p.is_dir) pendingDocs.set(p.rel, p.kind);
    }
    if (flushing) return;
    clearTimeout(timer);
    timer = setTimeout(flushFsChanges, 250);
  });

  async function flushFsChanges() {
    if (flushing) return;
    flushing = true;
    try {
      const changes = [...pendingChanges.values()];
      pendingChanges.clear();
      const docsToSync = new Map(pendingDocs);
      pendingDocs.clear();

      if (changes.length && tree && tree.applyChanges) {
        try { await tree.applyChanges(changes); } catch {}
      } else if (changes.length) {
        try { await tree.refresh(); } catch {}
      }

      for (const [rel, kind] of docsToSync) {
        const doc = openDocs.get(rel);
        if (!doc || doc.kind !== "text") continue;
        if (kind === "remove") {
          if (!doc.dirty) closeTab(rel);
          continue;
        }
        if (doc.dirty) {
          els.statusPath.textContent = rel + "（磁盘已变化，保留本地修改）";
          continue;
        }
        try {
          const content = await readFile(projectId, rel);
          if (content === doc.model.getValue()) continue;
          const viewState = editor && activeRel === rel ? editor.saveViewState() : null;
          doc.model.setValue(content);
          doc.savedValue = content;
          lastSnap.set(rel, content);
          if (viewState && activeRel === rel) editor.restoreViewState(viewState);
          doc.dirty = false;
          setGlobalDirty();
          renderTabs();
        } catch {}
      }

      if ((changes.length || docsToSync.size) && gitState && gitState.is_repo) {
        refreshGit().catch(() => {});
      }
    } finally {
      flushing = false;
      if ((pendingChanges.size || pendingDocs.size) && !timer) {
        timer = setTimeout(flushFsChanges, 250);
      }
    }
  }
}

boot();