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
  gitInit, gitFetch, gitClone, gitMerge, gitMergeAbort, gitResolve,
  gitStashPush, gitStashList, gitStashOp, gitFileLog, gitBlame,
  searchContent, uploadPaths, downloadFile,
  forwardOpen, forwardClose, forwardList,
} from "./api.js";
import { initTerminal, newRemoteTerminal } from "./term.js";
import { initBottom, toggleBottom, setProjectId, setPortsHandlers } from "./bottom.js";
import { getProjectRemote, setProjectRemote, remoteTerminalArgv } from "./api.js";

// 剪贴板：仅记录一次复制/剪切操作的相对路径，粘贴时读取。
let clip = null; // { items: [{rel, isDir}], mode: "copy" | "cut" }

const params = new URLSearchParams(location.search);
const projectId = params.get("project") || "";
const projectName = params.get("name") || projectId;
const restrict = params.get("restrict") === "1";
const remoteLabel = params.get("remote") || "";

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
  scmInitBtn: document.getElementById("scm-init-btn"),
  scmCloneBtn: document.getElementById("scm-clone-btn"),
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
window.__ideOpenFile = (rel) => { if (rel) openFile(rel, String(rel).split("/").pop()); };

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
let historyLimit = 50;
let historyFileRel = null;
let historyCommits = [];
let scmStashes = [];

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
      const merge = document.createElement("button");
      merge.className = "branch-del";
      merge.textContent = "合并";
      merge.title = "合并到当前分支";
      merge.addEventListener("click", async (e) => {
        e.stopPropagation();
        const ok = await showModal({ title: "合并分支", text: `将分支「${b.name}」合并到当前分支？`, okText: "合并" });
        if (!ok) return;
        try {
          const text = await gitMerge(projectId, b.name);
          if (text.includes("CONFLICT") || text.includes("Automatic merge failed")) {
            await showModal({ title: "合并冲突", text: text || "发生冲突，请手动解决。", okText: "关闭" });
            await refreshGit();
            return;
          }
          els.statusPath.textContent = `已合并分支：${b.name}`;
          closeBranchMenu();
          await refreshGit();
          await tree?.refresh();
        } catch (err) {
          els.statusPath.textContent = "合并失败：" + err;
        }
      });
      item.append(merge);
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

  const mergeNote = document.createElement("div");
  mergeNote.className = "branch-new";
  mergeNote.textContent = "↳ 合并到当前分支…";
  mergeNote.style.marginTop = "8px";
  mergeNote.addEventListener("click", async () => {
    closeBranchMenu();
    const target = branches.find((b) => !b.current);
    if (!target) return;
    const ok = await showModal({ title: "合并分支", text: `将分支「${target.name}」合并到当前分支？`, okText: "合并" });
    if (!ok) return;
    try {
      const text = await gitMerge(projectId, target.name);
      if (text.includes("CONFLICT") || text.includes("Automatic merge failed")) {
        await showModal({ title: "合并冲突", text: text || "发生冲突，请手动解决。", okText: "关闭" });
        await refreshGit();
        return;
      }
      els.statusPath.textContent = `已合并分支：${target.name}`;
      await refreshGit();
      await tree?.refresh();
    } catch (e) {
      els.statusPath.textContent = "合并失败：" + e;
    }
  });
  menu.append(mergeNote);
  document.body.append(menu);
  branchMenuEl = menu;

  const onDoc = (e) => {
    if (!menu.contains(e.target) && !els.scmBranch.contains(e.target)) {
      closeBranchMenu();
      document.removeEventListener("mousedown", onDoc);
    }
  };
  document.addEventListener("mousedown", onDoc);
}

async function renderHistory() {
  const wrap = els.scmHistory;
  wrap.innerHTML = "";
  let commits = [];
  try {
    commits = historyFileRel
      ? await gitFileLog(projectId, historyFileRel, historyLimit)
      : await gitLog(projectId, historyLimit);
    historyCommits = commits;
  } catch (e) { wrap.textContent = "读取历史失败：" + e; return; }

  if (historyFileRel) {
    const bar = document.createElement("div");
    bar.className = "hist-file-bar";
    const back = document.createElement("button");
    back.className = "hist-file-back";
    back.textContent = "← 全部历史";
    back.addEventListener("click", () => { historyFileRel = null; historyLimit = 100; renderHistory(); });
    const label = document.createElement("span");
    label.className = "hist-file-label";
    label.textContent = historyFileRel;
    label.title = historyFileRel;
    bar.append(back, label);
    wrap.append(bar);
  }

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

    const reset = document.createElement("button");
    reset.className = "hist-reset";
    reset.textContent = "还原";
    reset.title = "还原到此提交（混合模式）";
    reset.addEventListener("click", async (e) => {
      e.stopPropagation();
      const ok = await showModal({ title: "还原提交", text: `将当前分支还原到「${c.short}」？`, okText: "还原", danger: true });
      if (!ok) return;
      try { await gitReset(projectId, c.hash, "mixed"); els.statusPath.textContent = "已还原到 " + c.short; await refreshGit(); await tree?.refresh(); }
      catch (err) { els.statusPath.textContent = "还原失败：" + err; }
    });
    row.append(reset);
    wrap.append(row);
  }
  if (commits.length >= historyLimit) {
    const more = document.createElement("button");
    more.className = "scm-load-more";
    more.textContent = "加载更多";
    more.addEventListener("click", async () => { historyLimit += 50; await renderHistory(); });
    wrap.append(more);
  }
}

async function showFileHistory(rel) {
  historyFileRel = rel;
  historyLimit = 100;
  scmTab = "history";
  switchView("scm");
  switchScmTab("history");
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
    if (els.scmEmpty.firstChild) els.scmEmpty.firstChild.nodeValue = "未检测到 Git 仓库";
    els.scmGroups.innerHTML = "";
    els.scmCommitWrap.classList.add("hidden");
    els.scmBranch.classList.add("hidden");
    els.scmSync.classList.add("hidden");
    els.scmToolbar.classList.add("hidden");
    els.scmInitBtn?.classList.remove("hidden");
    els.scmCloneBtn?.classList.remove("hidden");
    refreshGitDecorations();
    refreshTabDecorations();
    return;
  }
  els.scmEmpty.classList.add("hidden");
  els.scmInitBtn?.classList.add("hidden");
  els.scmCloneBtn?.classList.add("hidden");
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

  await loadStashes();
  if (scmTab === "changes") {
    renderScmGroups();
    els.scmToolbar.classList.remove("hidden");
  } else {
    els.scmToolbar.classList.add("hidden");
  }
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
    renderStashGroup(wrap);
    return;
  }
  const conflicts = gitState.files.filter((f) => f.conflicted);
  if (conflicts.length) {
    const bar = document.createElement("div"); bar.className = "scm-conflict-bar";
    const text = document.createElement("span"); text.textContent = `检测到 ${conflicts.length} 个冲突，请选择我方/对方版本`;
    const abort = document.createElement("button"); abort.className = "scm-btn danger"; abort.textContent = "中止合并";
    abort.addEventListener("click", async () => {
      try { await gitMergeAbort(projectId); els.statusPath.textContent = "已中止合并"; await refreshGit(); }
      catch (e) { els.statusPath.textContent = "中止合并失败：" + e; }
    });
    bar.append(text, abort); wrap.append(bar);
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
  renderStashGroup(wrap);
}

async function loadStashes() {
  try { scmStashes = await gitStashList(projectId); }
  catch (e) { scmStashes = []; }
}

function renderStashGroup(wrap) {
  const head = document.createElement("div");
  head.className = "scm-group-head stash-head";
  head.innerHTML = `<span class="chev">${SVG_CHEVRON}</span><span>Stash</span><span class="cnt">${scmStashes.length}</span>`;
  head.addEventListener("click", () => {
    head.classList.toggle("collapsed");
    body.classList.toggle("hidden", head.classList.contains("collapsed"));
  });
  wrap.append(head);
  const body = document.createElement("div");
  body.className = "stash-items";
  for (const s of scmStashes) {
    const row = document.createElement("div"); row.className = "stash-row";
    const label = document.createElement("span"); label.className = "stash-label";
    label.textContent = s.message || s.name; label.title = s.name;
    row.append(label);
    for (const [action, text] of [["apply", "应用"], ["pop", "弹出"], ["drop", "删除"]]) {
      const b = document.createElement("button"); b.className = "scm-act stash-act"; b.textContent = text;
      b.addEventListener("click", async () => {
        if (action === "drop" && !(await showModal({ title: "删除 Stash", text: `删除「${s.message || s.name}」？`, okText: "删除", danger: true }))) return;
        try { await gitStashOp(projectId, action, s.index); await loadStashes(); await refreshGit(); }
        catch (e) { els.statusPath.textContent = "Stash 操作失败：" + e; }
      }); row.append(b);
    }
    body.append(row);
  }
  const save = document.createElement("button"); save.className = "scm-load-more stash-save"; save.textContent = "保存当前更改到 Stash";
  save.addEventListener("click", async () => {
    const msg = await showModal({ title: "保存 Stash", input: true, text: "可选的说明", okText: "保存" });
    if (msg === false) return;
    try { await gitStashPush(projectId, msg || null); await loadStashes(); await refreshGit(); }
    catch (e) { els.statusPath.textContent = "Stash 保存失败：" + e; }
  });
  body.append(save); wrap.append(body);
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
  if (f.conflicted) {
    for (const [side, title] of [["ours", "我方"], ["theirs", "对方"]]) {
      acts.append(mkBtn(side === "ours" ? "我" : "他", title, async () => {
        try { await gitResolve(projectId, f.rel, side); await refreshGit(); }
        catch (e) { els.statusPath.textContent = "冲突解决失败：" + e; }
      }));
    }
  }
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
  els.scmInitBtn?.addEventListener("click", async () => {
    try { await gitInit(projectId); els.statusPath.textContent = "Git 仓库已初始化"; await refreshGit(); await tree?.refresh(); }
    catch (e) { els.statusPath.textContent = "初始化失败：" + e; }
  });
  els.scmCloneBtn?.addEventListener("click", async () => {
    const url = await showModal({ title: "克隆仓库", input: true, text: "输入 Git 仓库地址（例如 https://github.com/xxx/yyy.git）", okText: "克隆", defaultValue: "" });
    if (!url) return;
    try {
      await gitClone(projectId, url);
      els.statusPath.textContent = "Git 仓库已克隆";
      await refreshGit();
      await tree?.refresh();
    } catch (e) {
      els.statusPath.textContent = "克隆失败：" + e;
    }
  });
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
    // 每个预览文档拥有独立常驻容器，与文本文档的常驻 model 对称。
    const el = document.createElement("div");
    el.className = "preview-item hidden";
    els.previewHost.append(el);
    let dispose = () => {};
    try {
      dispose = await renderPreview(el, projectId, rel, name);
    } catch (e) {
      els.statusPath.textContent = "预览失败：" + e;
      el.remove();
      return;
    }
    openDocs.set(rel, { kind: "preview", dirty: false, el, dispose });
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

function activateTab(rel) {
  if (diffActive) closeAgentDiff(false);
  if (gitDiffActive) closeGitDiff();
  const doc = openDocs.get(rel);
  if (!doc) return;
  activeRel = rel;
  if (blameOn && blameRel !== rel) clearBlame();
  els.statusPath.textContent = rel;
  els.breadcrumb.textContent = rel;

  const isPreview = doc.kind === "preview";
  els.previewHost.classList.toggle("hidden", !isPreview);
  els.monacoHost.classList.toggle("hidden", isPreview);
  if (isPreview) {
    if (editor) editor.setModel(null);
    // 显示当前预览、隐藏其它预览（各自常驻，切换零成本）。
    for (const [r, d] of openDocs) {
      if (d.kind === "preview" && d.el) d.el.classList.toggle("hidden", r !== rel);
    }
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
  // 释放资源；任一步失败都不能阻断标签移除。
  try { doc.dispose?.(); } catch {}
  try { doc.model?.dispose?.(); } catch {}
  try { doc.el?.remove(); } catch {}
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
  try {
    const spec = remoteLabel ? await getProjectRemote(projectId) : null;
    if (spec) {
      const newRoot = remoteParent(spec.root);
      if (!newRoot || newRoot === spec.root) { els.statusPath.textContent = "已到顶层"; return; }
      await setProjectRemote(projectId, {
        host: spec.host, user: spec.user || null, port: spec.port || null,
        root: newRoot, ssh_alias: spec.ssh_alias || null,
        proxy_jump: spec.proxy_jump || null, restrict: !!spec.restrict,
      });
      projectRootAbs = newRoot;
      await tree.setRoot("");
      updateSidebarPath("");
      els.statusPath.textContent = "工作目录 → " + newRoot;
      return;
    }
    const before = tree.getRoot();
    const after = normRelLocal(before + "/..");
    if (after === before) { els.statusPath.textContent = "已到顶层"; return; }
    const cur = await getWorkdir(projectId);
    const abs = newAbsFromRel(cur, before, after);
    await setWorkdir(projectId, abs);
    projectRootAbs = abs;
    await tree.setRoot(after);
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
    if (remoteLabel) m.append(menuItem("上传文件到此处…", () => uploadToDir(entry.rel)));
  } else {
    m.append(
      menuItem("打开", () => openFile(entry.rel, entry.name)),
      menuItem("文件历史", () => showFileHistory(entry.rel)),
      menuItem("查看 Blame", () => showBlame(entry.rel)),
    );
    if (remoteLabel) m.append(menuItem("下载到本地…", () => downloadTo(entry.rel)));
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

// —— Blame ——
let blameOn = false;
let blameRel = null;
let blameCollection = null;
let blameLines = [];

async function showBlame(rel) {
  await openFile(rel, rel.split("/").pop());
  if (!isTextDoc(rel)) { els.statusPath.textContent = "该文件不支持 Blame"; return; }
  if (!editor) return;
  let data = [];
  try { data = await gitBlame(projectId, rel); }
  catch (e) { els.statusPath.textContent = "Blame 失败：" + e; return; }
  if (!data.length) { els.statusPath.textContent = "无 Blame 信息（未跟踪或未提交）"; return; }
  blameLines = data;
  blameRel = rel;
  blameOn = true;
  applyBlameDecorations();
  els.statusPath.textContent = "Blame：" + rel;
}

function clearBlame() {
  blameOn = false;
  blameRel = null;
  blameLines = [];
  if (blameCollection) { blameCollection.clear(); blameCollection = null; }
  if (editor) editor.updateOptions({ glyphMargin: false });
}

function applyBlameDecorations() {
  if (!editor || !blameOn || !monaco) return;
  editor.updateOptions({ glyphMargin: true });
  if (blameCollection) blameCollection.clear();
  const decorations = blameLines.map((b) => ({
    range: new monaco.Range(b.line, 1, b.line, 1),
    options: {
      isWholeLine: true,
      glyphMarginClassName: "blame-glyph",
      glyphMarginHoverMessage: { value: `**${b.author}**  ${blameTime(b.time)}\n\n\`${b.short}\`` },
    },
  }));
  if (editor.createDecorationsCollection) {
    blameCollection = editor.createDecorationsCollection(decorations);
  } else if (editor.deltaDecorations) {
    const prev = editor.deltaDecorations([], decorations);
    blameCollection = { clear: () => editor.deltaDecorations(prev, []) };
  }
}

function blameTime(v) {
  const n = Number(v);
  if (!n) return v || "";
  const d = new Date(n * 1000);
  const p = (x) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function isTextDoc(rel) {
  const doc = openDocs.get(rel);
  return !!doc && doc.kind === "text";
}

function sep() { const d = document.createElement("div"); d.className = "ctx-sep"; return d; }
function parentOf(rel) { const i = rel.lastIndexOf("/"); return i <0 ? "" : rel.slice(0, i); }

// —— 远程端口转发面板 ——
let forwardSpec = null; // { host, user, port, sshAlias }

// 打开一个远程 SSH 交互终端（复用后端 ssh::terminal_argv，含 -tt 与远端 cd root）。
async function openRemoteTerminal() {
  els.statusPath.textContent = "建立远程终端中…";
  try {
    const argv = await remoteTerminalArgv(projectId);
    newRemoteTerminal(argv.prog, argv.args, "SSH " + (argv.label || ""));
    els.statusPath.textContent = "远程终端已打开：" + (argv.label || "");
  } catch (e) {
    els.statusPath.textContent = "远程终端失败：" + e;
  }
}

async function refreshPorts() {
  const list = document.getElementById("ports-list");
  if (!list) return;
  let items = [];
  try { items = await forwardList(); } catch { items = []; }
  list.innerHTML = "";
  if (!items.length) {
    const empty = document.createElement("div");
    empty.className = "ports-empty";
    empty.textContent = "暂无转发。填写本地端口与远端目标后点「转发」。";
    list.appendChild(empty);
    return;
  }
  for (const f of items) {
    const row = document.createElement("div");
    row.className = "ports-row";
    const label = document.createElement("span");
    label.className = "ports-row-label";
    label.textContent = `127.0.0.1:${f.local_port} → ${f.remote_host}:${f.remote_port}`;
    const open = document.createElement("button");
    open.className = "term-btn";
    open.textContent = "打开";
    open.title = "在浏览器打开 http://127.0.0.1:" + f.local_port;
    open.addEventListener("click", () => window.open("http://127.0.0.1:" + f.local_port));
    const del = document.createElement("button");
    del.className = "term-btn";
    del.textContent = "✕";
    del.title = "关闭转发";
    del.addEventListener("click", async () => {
      try { await forwardClose(f.local_port); } catch (e) { els.statusPath.textContent = "关闭失败：" + e; }
      await refreshPorts();
    });
    row.append(label, open, del);
    list.appendChild(row);
  }
}

async function addForward() {
  const localEl = document.getElementById("fwd-local");
  const hostEl = document.getElementById("fwd-remote-host");
  const portEl = document.getElementById("fwd-remote-port");
  const localPort = parseInt(localEl.value, 10);
  const remoteHost = (hostEl.value || "127.0.0.1").trim();
  const remotePort = parseInt(portEl.value, 10);
  if (!localPort || localPort <1 || localPort > 65535) { els.statusPath.textContent = "本地端口无效"; return; }
  if (!remotePort || remotePort <1 || remotePort > 65535) { els.statusPath.textContent = "远端端口无效"; return; }
  if (!forwardSpec) { els.statusPath.textContent = "当前项目不是远程项目，无法转发"; return; }
  els.statusPath.textContent = "建立转发中…";
  try {
    await forwardOpen({
      host: forwardSpec.host,
      user: forwardSpec.user || null,
      port: forwardSpec.port || null,
      sshAlias: forwardSpec.sshAlias || null,
      localPort,
      remoteHost,
      remotePort,
    });
    els.statusPath.textContent = `转发已建立：127.0.0.1:${localPort} → ${remoteHost}:${remotePort}`;
    await refreshPorts();
  } catch (e) {
    els.statusPath.textContent = "转发失败：" + e;
  }
}

// 远程项目：上传本地文件到远程目录。
async function uploadToDir(relDir) {
  let selected;
  try {
    selected = await window.__TAURI__.dialog.open({ multiple: true, title: "选择要上传的本地文件" });
  } catch (e) { els.statusPath.textContent = "打开选择器失败：" + e; return; }
  if (!selected) return;
  const paths = Array.isArray(selected) ? selected : [selected];
  if (!paths.length) return;
  els.statusPath.textContent = "上传中…";
  try {
    const res = await uploadPaths(projectId, paths, relDir);
    els.statusPath.textContent = `已上传 ${res.uploaded} 个文件` + (res.failed && res.failed.length ? `，失败 ${res.failed.length} 个` : "");
    await refreshParent(relDir ? relDir + "/x" : "x");
  } catch (e) {
    els.statusPath.textContent = "上传失败：" + e;
  }
}

// 远程项目：下载远程文件到本地。
async function downloadTo(rel) {
  const name = rel.split("/").pop();
  let dest;
  try {
    dest = await window.__TAURI__.dialog.save({ title: "保存到本地", defaultPath: name });
  } catch (e) { els.statusPath.textContent = "打开保存框失败：" + e; return; }
  if (!dest) return;
  els.statusPath.textContent = "下载中…";
  try {
    await downloadFile(projectId, rel, dest);
    els.statusPath.textContent = "已下载到 " + dest;
  } catch (e) {
    els.statusPath.textContent = "下载失败：" + e;
  }
}

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
    const spec = remoteLabel ? await getProjectRemote(projectId) : null;
    if (spec) {
      const newRoot = remoteJoin(spec.root, rel);
      await setProjectRemote(projectId, {
        host: spec.host, user: spec.user || null, port: spec.port || null,
        root: newRoot, ssh_alias: spec.ssh_alias || null,
        proxy_jump: spec.proxy_jump || null, restrict: !!spec.restrict,
      });
      projectRootAbs = newRoot;
      await tree.setRoot("");
      updateSidebarPath("");
      els.statusPath.textContent = "项目文件夹 → " + newRoot;
      return;
    }
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

// —— 远程路径跨平台工具 ——
function isWinPath(p) {
  return /^[A-Za-z]:/.test(p || "") || (p || "").includes("\\");
}
function remoteJoin(root, rel) {
  if (!rel) return root || "";
  if (isWinPath(root)) {
    const sep = "\\";
    const base = (root || "").replace(/[\\/]+$/, "");
    const segs = rel.split("/").filter((s) => s && s !== ".");
    return base + sep + segs.join(sep);
  }
  const base = (root || "").replace(/\/+$/, "");
  const segs = rel.split("/").filter((s) => s && s !== ".");
  return (base || "") + "/" + segs.join("/");
}
function remoteParent(p) {
  if (!p) return "";
  if (isWinPath(p)) {
    const norm = p.replace(/[\\/]+$/, "");
    if (/^[A-Za-z]:$/.test(norm)) return norm + "\\";
    const i = Math.max(norm.lastIndexOf("\\"), norm.lastIndexOf("/"));
    if (i <= 1) return norm.slice(0, 2) + "\\";
    const parent = norm.slice(0, i);
    return /^[A-Za-z]:$/.test(parent) ? parent + "\\" : parent;
  }
  const norm = p.replace(/\/+$/, "");
  if (norm === "" || norm === "/") return "/";
  const i = norm.lastIndexOf("/");
  return i <= 0 ? "/" : norm.slice(0, i);
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
    await refreshParent(rel);
    if (!isDir) openFile(rel, name);
    pushHistory({
      label: (isDir ? "新建文件夹 " : "新建文件 ") + rel,
      undo: async () => { closeTabsUnder(rel); await deletePath(projectId, rel); tree.removeNode(rel); },
      redo: async () => { await createPath(projectId, parentDir, name, isDir); await refreshParent(rel); },
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
    for (const d of deleted) tree.removeNode(d.rel);
    pushHistory({
      label: only ? "删除 " + targets[0].rel : `删除 ${deleted.length} 个项目`,
      undo: async () => { for (const d of deleted) await restorePath(projectId, d.trashRel, d.rel); for (const d of deleted) await refreshParent(d.rel); },
      redo: async () => { for (const d of deleted) await deletePath(projectId, d.rel); for (const d of deleted) tree.removeNode(d.rel); },
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

// 让某条目所在父目录重新拉取并原地 Diff（用于撤销/重做后的回填）。
async function refreshParent(rel) {
  try { await tree.applyChanges([{ rel, kind: "create" }]); } catch {}
}

// 移动/重命名后，把已打开文档的路径平滑迁移（保留未保存内容与 Tab），不关闭 Tab。
function rebaseOpenDocs(oldRel, newRel) {
  const prefix = oldRel + "/";
  for (const key of [...openDocs.keys()]) {
    if (key === oldRel || key.startsWith(prefix)) {
      const updated = newRel + key.slice(oldRel.length);
      const doc = openDocs.get(key);
      openDocs.delete(key);
      openDocs.set(updated, doc);
      if (activeRel === key) activeRel = updated;
    }
  }
  if (els.treeHost && els.treeHost.dataset.active) {
    const act = els.treeHost.dataset.active;
    if (act === oldRel || act.startsWith(prefix)) {
      els.treeHost.dataset.active = newRel + act.slice(oldRel.length);
    }
  }
  renderTabs();
}

async function pasteInto(destDir) {
  if (!clip) return;
  try {
    if (clip.mode === "cut") {
      const moved = []; // { from, to }
      for (const item of clip.items) {
        const rel = await movePath(projectId, item.rel, destDir);
        moved.push({ from: item.rel, to: rel });
        rebaseOpenDocs(item.rel, rel);
        await tree.moveNode(item.rel, rel);
      }
      clip = null;
      els.statusPath.textContent = moved.length === 1 ? "已移动到：" + moved[0].to : `已移动 ${moved.length} 个项目`;
      pushHistory({
        label: moved.length === 1 ? "移动 " + moved[0].from : `移动 ${moved.length} 个项目`,
        undo: async () => { for (const m of moved) { const back = await movePath(projectId, m.to, parentOf(m.from)); rebaseOpenDocs(m.to, back); await tree.moveNode(m.to, back); } },
        redo: async () => { for (const m of moved) { const again = await movePath(projectId, m.from, destDir); rebaseOpenDocs(m.from, again); await tree.moveNode(m.from, again); } },
      });
    } else {
      const created = []; // { from, to }
      for (const item of clip.items) {
        const rel = await copyPath(projectId, item.rel, destDir);
        created.push({ from: item.rel, to: rel });
      }
      els.statusPath.textContent = created.length === 1 ? "已复制到：" + created[0].to : `已复制 ${created.length} 个项目`;
      for (const c of created) await refreshParent(c.to);
      pushHistory({
        label: created.length === 1 ? "复制 " + created[0].from : `复制 ${created.length} 个项目`,
        undo: async () => { for (const c of created) { await deletePath(projectId, c.to); tree.removeNode(c.to); } },
        redo: async () => { for (const c of created) await copyPath(projectId, c.from, destDir); for (const c of created) await refreshParent(c.to); },
      });
    }
  } catch (e) {
    els.statusPath.textContent = "粘贴失败：" + e;
  }
}



async function boot() {
  if (!projectId) { els.statusPath.textContent = "缺少 project 参数"; return; }
  els.projectName.textContent = remoteLabel
    ? projectName + "  ·  SSH: " + remoteLabel
    : projectName;

  if (remoteLabel) {
    try {
      const spec = await getProjectRemote(projectId);
      projectRootAbs = (spec && spec.root) ? spec.root : "";
    } catch { projectRootAbs = ""; }
  } else {
    try { projectRootAbs = await getWorkdir(projectId); } catch { projectRootAbs = ""; }
  }

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
        rebaseOpenDocs(rel, moved);
        await tree.moveNode(rel, moved);
        pushHistory({
          label: "重命名 " + rel,
          undo: async () => { const back = await movePath(projectId, moved, dir, oldName); rebaseOpenDocs(moved, back); await tree.moveNode(moved, back); },
          redo: async () => { const again = await movePath(projectId, rel, dir, newName); rebaseOpenDocs(rel, again); await tree.moveNode(rel, again); },
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
        rebaseOpenDocs(src, rel);
        await tree.moveNode(src, rel);
        pushHistory({
          label: "移动 " + src,
          undo: async () => { const back = await movePath(projectId, rel, parentOf(src)); rebaseOpenDocs(rel, back); await tree.moveNode(rel, back); },
          redo: async () => { const again = await movePath(projectId, src, destDir); rebaseOpenDocs(src, again); await tree.moveNode(src, again); },
        });
      } catch (e) {
        els.statusPath.textContent = "移动失败：" + e;
      }
    },
    onMoveBatch: async (srcList, destDir) => {
      const moved = [];
      try {
        for (const src of srcList) {
          const rel = await movePath(projectId, src, destDir);
          rebaseOpenDocs(src, rel);
          await tree.moveNode(src, rel);
          moved.push({ old: src, rel });
        }
        els.statusPath.textContent = "已移动 " + moved.length + " 项";
        pushHistory({
          label: "移动 " + moved.length + " 项",
          undo: async () => { for (const m of moved) { const back = await movePath(projectId, m.rel, parentOf(m.old)); rebaseOpenDocs(m.rel, back); await tree.moveNode(m.rel, back); } },
          redo: async () => { for (const m of moved) { const again = await movePath(projectId, m.old, destDir); rebaseOpenDocs(m.old, again); await tree.moveNode(m.old, again); } },
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

  // 搜索：普通=按文件名；以 # 开头=按内容（rg/grep，远程走 ssh）。防抖。
  let searchTimer = null;
  els.searchInput.addEventListener("input", () => {
    clearTimeout(searchTimer);
    const raw = els.searchInput.value;
    const isContent = raw.trimStart().startsWith("#");
    els.searchInput.placeholder = isContent ? "搜索内容…" : "搜索文件…";
    searchTimer = setTimeout(() => {
      if (isContent) {
        tree.searchContent(raw.trimStart().slice(1));
      } else {
        tree.search(raw);
      }
    }, 180);
  });

  window.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      if (diffActive) { els.statusPath.textContent = "只读 Diff 模式不可保存，请返回编辑"; return; }
      save();
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "w") { e.preventDefault(); if (activeRel) closeTab(activeRel); }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "t") {
      // 终端内聚焦时不拦截，交由 shell 处理 Ctrl+T。
      if (e.target.closest && e.target.closest(".terminal-panel")) return;
      e.preventDefault();
      toggleBottom("term");
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "j") {
      // 输入框内聚焦时不拦截（对话输入框用 Ctrl+J 无意义，但避免误触）。
      if (e.target.closest && e.target.closest(".agent-panel")) return;
      e.preventDefault();
      toggleBottom("agent");
    }
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
  setProjectId(projectId);
  initTerminal({
    projectId,
    cwdProvider: () => projectRootAbs || "",
    onRemoteNew: remoteLabel ? openRemoteTerminal : null,
  });
  initBottom({ projectId, cwdProvider: () => projectRootAbs || "" });
  setPortsHandlers({ onOpen: refreshPorts, onAdd: addForward });
  if (remoteLabel) {
    try {
      const spec = await getProjectRemote(projectId);
      if (spec) forwardSpec = { host: spec.host, user: spec.user, port: spec.port, sshAlias: spec.ssh_alias };
    } catch {}
  }
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