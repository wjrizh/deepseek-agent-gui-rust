// 左侧文件树：状态驱动 + Keyed DOM 原地 Diff + VS Code 级拖拽内核。
// 懒加载目录、展开/折叠、点击文件回调、搜索、上一层、多选、键盘导航。
// 对外保持 createTree/fileIcon 及原有 13 个方法的签名不变。

import { listDir } from "./api.js";

// —— SVG 图标 ——
const SVG_FOLDER = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none"><path d="M3 6.5A1.5 1.5 0 014.5 5h4l2 2h7A1.5 1.5 0 0119 8.5v8A1.5 1.5 0 0117.5 18h-13A1.5 1.5 0 013 16.5v-10z" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>';
const SVG_FOLDER_OPEN = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none"><path d="M3 6.5A1.5 1.5 0 014.5 5h4l2 2h7A1.5 1.5 0 0119 8.5V10" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M3 10h16.2a1 1 0 01.96 1.28l-1.9 6A1.5 1.5 0 0116.8 18H4.5A1.5 1.5 0 013 16.5V10z" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>';
const SVG_FILE = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none"><path d="M6 3.5h7L18 8.5v12A1.5 1.5 0 0116.5 22h-10A1.5 1.5 0 015 20.5v-15A1.5 1.5 0 016.5 4" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M13 3.5V9h5" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/></svg>';
const SVG_FILE_CODE = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none"><path d="M6 3.5h7L18 8.5v12A1.5 1.5 0 0116.5 22h-10A1.5 1.5 0 015 20.5v-15A1.5 1.5 0 016.5 4" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M13 3.5V9h5" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M10 13l-2 2 2 2M14 13l2 2-2 2" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const SVG_FILE_MARKDOWN = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none"><path d="M6 3.5h7L18 8.5v12A1.5 1.5 0 0116.5 22h-10A1.5 1.5 0 015 20.5v-15A1.5 1.5 0 016.5 4" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M13 3.5V9h5" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M8 17v-3l1.6 2L11 14v3M13.5 14v3M13.5 17l1.5-1.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>';
const SVG_FILE_JSON = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none"><path d="M6 3.5h7L18 8.5v12A1.5 1.5 0 0116.5 22h-10A1.5 1.5 0 015 20.5v-15A1.5 1.5 0 016.5 4" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M13 3.5V9h5" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M10 13.5c-1 0-1 1-1 1.5s0 1.5-1 1.5M14 13.5c1 0 1 1 1 1.5s0 1.5 1 1.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';
const SVG_FILE_IMAGE = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none"><path d="M6 3.5h7L18 8.5v12A1.5 1.5 0 0116.5 22h-10A1.5 1.5 0 015 20.5v-15A1.5 1.5 0 016.5 4" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><path d="M13 3.5V9h5" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><circle cx="9.5" cy="14" r="1" stroke="currentColor" stroke-width="1.3"/><path d="M8 20l3-3 2 2 2.5-2.5L18 19" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>';

const COLOR_FOLDER = "#7fa8ff";
const COLOR_CODE = "#4ec9b0";
const COLOR_MARKDOWN = "#9aa4b2";
const COLOR_JSON = "#e2c08d";
const COLOR_IMAGE = "#c586c0";
const COLOR_PLAIN = "#8a94a6";

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function iconFor(entry, expanded) {
  if (entry.is_dir) {
    return { svg: expanded ? SVG_FOLDER_OPEN : SVG_FOLDER, color: COLOR_FOLDER };
  }
  const n = (entry.name || "").toLowerCase();
  if (/\.(js|mjs|cjs|jsx|ts|tsx|rs|go|py|java|c|h|cpp|cc|hpp|cs|rb|php|sh|bash|zsh|lua|swift|kt)$/.test(n)) {
    return { svg: SVG_FILE_CODE, color: COLOR_CODE };
  }
  if (/\.(md|markdown)$/.test(n)) return { svg: SVG_FILE_MARKDOWN, color: COLOR_MARKDOWN };
  if (/\.(json|jsonc|json5)$/.test(n)) return { svg: SVG_FILE_JSON, color: COLOR_JSON };
  if (/\.(png|jpg|jpeg|gif|svg|webp|bmp|ico)$/.test(n)) return { svg: SVG_FILE_IMAGE, color: COLOR_IMAGE };
  return { svg: SVG_FILE, color: COLOR_PLAIN };
}

// 规范化相对路径：折叠 `..` 与 `.`。
function normRel(rel) {
  const parts = (rel || "").replace(/\\/g, "/").split("/").filter((p) => p && p !== ".");
  const out = [];
  for (const p of parts) {
    if (p === "..") {
      if (out.length) out.pop();
      else out.push("..");
    } else out.push(p);
  }
  return out.join("/");
}

function parentOf(rel) {
  const n = normRel(rel);
  const idx = n.lastIndexOf("/");
  return idx === -1 ? "" : n.slice(0, idx);
}

function baseName(rel) {
  const n = normRel(rel);
  const idx = n.lastIndexOf("/");
  return idx === -1 ? n : n.slice(idx + 1);
}

function sortEntries(entries) {
  return [...entries].sort((a, b) => {
    if (!!a.is_dir !== !!b.is_dir) return a.is_dir ? -1 : 1;
    return collator.compare(a.name || "", b.name || "");
  });
}

// 供其他模块（如源代码管理面板）按文件名取图标。
export function fileIcon(name) {
  return iconFor({ name, is_dir: false }, false);
}


export function createTree(host, projectId, onOpenFile, restrict, opts = {}) {
  const state = new Map(); // rel -> { expanded }（兼容现有命名）
  let rootRel = "";        // 当前显示根（相对工作目录）
  const activePath = () => host.dataset.active || "";
  const onContextMenu = opts.onContextMenu || null;
  const selection = new Set(); // 当前选中的 rel 集合（支持 Ctrl 多选）
  let lastClickedRel = "";     // Shift 连选锚点
  let focusedRel = "";         // 键盘焦点
  const nodeMap = new Map();   // rel -> .tree-node DOM 容器

  if (!host.hasAttribute("tabindex")) host.setAttribute("tabindex", "0");

  function depthOf(rel) {
    const n = normRel(rel);
    if (!n || n === rootRel) return 0;
    const sub = rootRel && n.startsWith(rootRel + "/") ? n.slice(rootRel.length + 1) : n;
    return sub.split("/").filter(Boolean).length - 1;
  }

  function getChildrenBox(dirRel) {
    const d = normRel(dirRel);
    if (d === rootRel) return host;
    const node = nodeMap.get(d);
    return node ? node.querySelector(":scope > .tree-children") : null;
  }

  // —— 选择 ——
  function applySelection() {
    for (const [rel, node] of nodeMap.entries()) {
      const row = node.firstElementChild;
      if (!row) continue;
      row.classList.toggle("selected", selection.has(rel));
      row.classList.toggle("focused", rel === focusedRel);
      row.classList.toggle("active", !node._entry?.is_dir && rel === activePath());
    }
  }
  function selectSingle(rel) {
    selection.clear();
    if (rel) {
      selection.add(rel);
      lastClickedRel = rel;
      focusedRel = rel;
    }
    applySelection();
  }
  function toggleSelect(rel) {
    if (!rel) return;
    if (selection.has(rel)) selection.delete(rel);
    else selection.add(rel);
    lastClickedRel = rel;
    focusedRel = rel;
    applySelection();
  }
  function selectRange(targetRel) {
    const visible = getVisibleRels();
    const a = visible.indexOf(lastClickedRel || targetRel);
    const b = visible.indexOf(targetRel);
    if (a === -1 || b === -1) { selectSingle(targetRel); return; }
    const [lo, hi] = a < b ? [a, b] : [b, a];
    selection.clear();
    for (let i = lo; i <= hi; i++) selection.add(visible[i]);
    focusedRel = targetRel;
    applySelection();
  }
  function clearSelection() {
    selection.clear();
    applySelection();
  }
  function getSelection() {
    const out = [];
    for (const rel of selection) {
      const node = nodeMap.get(rel);
      if (node && node._entry) out.push({ rel, isDir: !!node._entry.is_dir, name: node._entry.name });
    }
    return out;
  }
  function getTopLevelSelectedRels(fallbackRel) {
    const base = selection.has(fallbackRel) ? [...selection] : [fallbackRel];
    const sorted = base.map(normRel).filter(Boolean).sort((x, y) => x.length - y.length);
    const result = [];
    for (const r of sorted) {
      if (!result.some((p) => r === p || r.startsWith(p + "/"))) result.push(r);
    }
    return result;
  }
  function getVisibleRels() {
    const list = [];
    for (const r of host.querySelectorAll(".tree-row[data-rel]")) {
      if (r.offsetParent !== null) list.push(r.dataset.rel);
    }
    return list;
  }

  // —— 脏标记与 Agent Diff 角标 ——
  function updateAgentDiffDot(row, entry) {
    let hasDiff = false;
    try {
      if (entry.is_dir) {
        if (window.__hasUnreadAgentDiffUnder) hasDiff = window.__hasUnreadAgentDiffUnder(entry.rel);
      } else if (window.__hasUnreadAgentDiff) {
        hasDiff = window.__hasUnreadAgentDiff(entry.rel);
      }
    } catch {}
    const dot = row.querySelector(".agent-diff-dot");
    if (hasDiff && !dot) {
      const d = document.createElement("span");
      d.className = "agent-diff-dot";
      row.append(d);
    } else if (!hasDiff && dot) {
      dot.remove();
    }
  }

  let dirtyRelSet = new Set();
  function applyDirty() {
    const dirs = new Set();
    for (const rel of dirtyRelSet) {
      const parts = rel.split("/");
      for (let i = 1; i < parts.length; i++) dirs.add(parts.slice(0, i).join("/"));
    }
    for (const [rel, node] of nodeMap.entries()) {
      const row = node.firstElementChild;
      if (!row) continue;
      const isDir = !!node._entry?.is_dir;
      const dirty = isDir ? dirs.has(rel) : dirtyRelSet.has(rel);
      row.classList.toggle("dirty", dirty);
      row.classList.toggle("active", !isDir && rel === activePath());
      if (node._entry) updateAgentDiffDot(row, node._entry);
    }
    if (window.__refreshGitDecorations) window.__refreshGitDecorations();
  }
  function setDirty(relSet) {
    dirtyRelSet = new Set(relSet || []);
    applyDirty();
  }

  // —— 拖拽辅助（VS Code 式：拖到文件=其父目录；拖到目录=该目录并悬停展开）——
  let dragHoverTimer = null;
  let dragHoverDirRel = null;
  let currentDropDirEl = null;
  function clearDragVisuals() {
    if (dragHoverTimer) { clearTimeout(dragHoverTimer); dragHoverTimer = null; }
    dragHoverDirRel = null;
    if (currentDropDirEl) {
      currentDropDirEl.classList.remove("drop-target-dir");
      const r = currentDropDirEl.querySelector?.(":scope > .tree-row");
      if (r) r.classList.remove("drop-target");
      currentDropDirEl = null;
    }
    host.classList.remove("drop-root-target");
  }
  function highlightDropDir(dirRel) {
    const targetEl = dirRel === rootRel ? host : nodeMap.get(dirRel);
    if (currentDropDirEl === targetEl) return;
    clearDragVisuals();
    currentDropDirEl = targetEl;
    if (dirRel === rootRel) {
      host.classList.add("drop-root-target");
    } else if (targetEl) {
      targetEl.classList.add("drop-target-dir");
      const r = targetEl.querySelector(":scope > .tree-row");
      if (r) r.classList.add("drop-target");
    }
  }
  function canDropInto(dragRels, destDirRel) {
    const dest = normRel(destDirRel);
    if (!dragRels || !dragRels.length) return false;
    for (const src of dragRels) {
      const s = normRel(src);
      if (s === dest || dest.startsWith(s + "/") || parentOf(s) === dest) return false;
    }
    return true;
  }
  function getDraggedRels() {
    try {
      const raw = host.dataset.dragRels;
      if (raw) return JSON.parse(raw);
    } catch {}
    return host.dataset.dragSrc ? [host.dataset.dragSrc] : [];
  }

  // —— 节点视觉同步 ——
  function syncNodeVisual(node, entry, depth) {
    node._entry = entry;
    node.dataset.rel = entry.rel;
    node.dataset.isDir = entry.is_dir ? "1" : "0";

    const row = node.firstElementChild;
    row.dataset.rel = entry.rel;
    row.dataset.isDir = entry.is_dir ? "1" : "0";
    row._entry = entry;
    row.style.paddingLeft = 6 + depth * 14 + "px";

    const isExp = !!(entry.is_dir && state.get(entry.rel)?.expanded);
    const twisty = row.querySelector(".tree-twisty");
    if (twisty) {
      twisty.classList.toggle("open", isExp);
      twisty.classList.toggle("empty", !entry.is_dir);
    }
    const iconSpan = row.querySelector(".tree-icon");
    if (iconSpan) {
      const ic = iconFor(entry, isExp);
      iconSpan.innerHTML = ic.svg;
      iconSpan.style.color = ic.color;
    }
    const labelSpan = row.querySelector(".tree-label");
    if (labelSpan && !labelSpan.querySelector("input")) {
      labelSpan.textContent = entry.name;
      labelSpan.title = entry.rel;
    }
    row.classList.toggle("selected", selection.has(entry.rel));
    row.classList.toggle("focused", entry.rel === focusedRel);
    row.classList.toggle("active", !entry.is_dir && entry.rel === activePath());
    updateAgentDiffDot(row, entry);
  }

  // —— 创建单个 .tree-node（.tree-row + 可选 .tree-children）——
  function createNodeEl(entry, depth) {
    const node = document.createElement("div");
    node.className = "tree-node";

    const row = document.createElement("div");
    row.className = "tree-row";
    row.draggable = true;

    const twisty = document.createElement("span");
    twisty.className = "tree-twisty";
    twisty.innerHTML = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none"><path d="M9 6l6 6-6 6" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>';

    const icon = document.createElement("span");
    icon.className = "tree-icon";

    const label = document.createElement("span");
    label.className = "tree-label";

    row.append(twisty, icon, label);
    node.appendChild(row);

    const isExp = !!(entry.is_dir && state.get(entry.rel)?.expanded);
    if (entry.is_dir) {
      const childBox = document.createElement("div");
      childBox.className = "tree-children" + (isExp ? "" : " hidden");
      node.appendChild(childBox);
    }

    syncNodeVisual(node, entry, depth);
    nodeMap.set(entry.rel, node);

    row.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      if (row.querySelector("input")) return;
      const curEntry = node._entry;
      if (ev.shiftKey && lastClickedRel) { selectRange(curEntry.rel); return; }
      if (ev.ctrlKey || ev.metaKey) { toggleSelect(curEntry.rel); return; }
      selectSingle(curEntry.rel);
      if (curEntry.is_dir) {
        await toggleDir(curEntry.rel);
      } else {
        host.dataset.active = curEntry.rel;
        applySelection();
        onOpenFile(curEntry.rel, curEntry.name);
      }
    });

    row.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      const curEntry = node._entry;
      if (!selection.has(curEntry.rel)) selectSingle(curEntry.rel);
      if (onContextMenu) onContextMenu(curEntry, ev.clientX, ev.clientY);
    });

    row.addEventListener("dragstart", (ev) => {
      const curEntry = node._entry;
      if (!selection.has(curEntry.rel)) selectSingle(curEntry.rel);
      const dragRels = getTopLevelSelectedRels(curEntry.rel);
      ev.dataTransfer.effectAllowed = "move";
      ev.dataTransfer.setData("text/plain", dragRels.join("\n"));
      try {
        const absPrefix = host.dataset.abs ? host.dataset.abs + "/" : "";
        ev.dataTransfer.setData("text/uri-list", dragRels.map((r) => absPrefix + r).join("\r\n"));
      } catch {}
      host.dataset.dragSrc = curEntry.rel;
      host.dataset.dragRels = JSON.stringify(dragRels);
      host.dataset.dragIsDir = curEntry.is_dir ? "1" : "0";
      row.classList.add("dragging");
    });

    row.addEventListener("dragend", () => {
      row.classList.remove("dragging");
      delete host.dataset.dragSrc;
      delete host.dataset.dragRels;
      delete host.dataset.dragIsDir;
      clearDragVisuals();
    });

    row.addEventListener("dragover", (ev) => {
      const dragRels = getDraggedRels();
      if (!dragRels.length) return;
      const curEntry = node._entry;
      const targetDir = curEntry.is_dir ? curEntry.rel : parentOf(curEntry.rel);
      if (!canDropInto(dragRels, targetDir)) {
        ev.dataTransfer.dropEffect = "none";
        clearDragVisuals();
        return;
      }
      ev.preventDefault();
      ev.stopPropagation();
      ev.dataTransfer.dropEffect = "move";
      highlightDropDir(targetDir);

      if (curEntry.is_dir && !state.get(curEntry.rel)?.expanded) {
        if (dragHoverDirRel !== curEntry.rel) {
          clearTimeout(dragHoverTimer);
          dragHoverDirRel = curEntry.rel;
          dragHoverTimer = setTimeout(() => {
            if (dragHoverDirRel === curEntry.rel && !state.get(curEntry.rel)?.expanded) {
              expandDir(curEntry.rel);
            }
          }, 500);
        }
      } else if (dragHoverTimer) {
        clearTimeout(dragHoverTimer);
        dragHoverTimer = null;
        dragHoverDirRel = null;
      }
    });

    row.addEventListener("drop", async (ev) => {
      const dragRels = getDraggedRels();
      clearDragVisuals();
      if (!dragRels.length) return;
      const curEntry = node._entry;
      const targetDir = curEntry.is_dir ? curEntry.rel : parentOf(curEntry.rel);
      if (!canDropInto(dragRels, targetDir)) return;
      ev.preventDefault();
      ev.stopPropagation();
      if (opts.onMoveBatch) await opts.onMoveBatch(dragRels, targetDir);
      else if (opts.onMove) for (const s of dragRels) await opts.onMove(s, targetDir);
    });

    return node;
  }

  // 根容器空白拖拽
  host.addEventListener("dragover", (ev) => {
    if (ev.target !== host) return;
    const dragRels = getDraggedRels();
    if (!canDropInto(dragRels, rootRel)) { clearDragVisuals(); return; }
    ev.preventDefault();
    ev.dataTransfer.dropEffect = "move";
    highlightDropDir(rootRel);
  });
  host.addEventListener("dragleave", (ev) => {
    if (ev.target === host && !host.contains(ev.relatedTarget)) clearDragVisuals();
  });
  host.addEventListener("drop", async (ev) => {
    if (ev.target !== host) return;
    const dragRels = getDraggedRels();
    clearDragVisuals();
    if (!canDropInto(dragRels, rootRel)) return;
    ev.preventDefault();
    if (opts.onMoveBatch) await opts.onMoveBatch(dragRels, rootRel);
    else if (opts.onMove) for (const s of dragRels) await opts.onMove(s, rootRel);
  });

  // —— 单层 Keyed DOM 原地 Reconciler（不清空容器，复用节点与展开子树）——
  function reconcileContainer(box, rawEntries, depth) {
    const entries = sortEntries(rawEntries || []);
    const keepRels = new Set(entries.map((e) => e.rel));

    // 1. 移除不再存在的旧节点（保留内联新建行；清掉旧的 tree-error）
    for (const child of [...box.children]) {
      if (child.classList.contains("tree-inline-create")) continue;
      if (child.classList.contains("tree-error")) { child.remove(); continue; }
      const cRel = child.dataset?.rel;
      if (!cRel || !keepRels.has(cRel)) {
        cleanupSubtree(child);
        child.remove();
      }
    }

    // 2. 依序复用/创建节点，收集到 nodes 后按顺序 append（避免反复 insertBefore）
    const nodes = [];
    for (const entry of entries) {
      let node = nodeMap.get(entry.rel);
      const typeChanged = node && !!node._entry?.is_dir !== !!entry.is_dir;
      if (!node || typeChanged) {
        if (node) { cleanupSubtree(node); node.remove(); }
        node = createNodeEl(entry, depth);
      } else {
        syncNodeVisual(node, entry, depth);
      }
      nodes.push(node);
    }

    // 3. 按顺序放置：已有节点若已在正确位置则不动，否则移动；appendChild 对尾部节点是 no-op
    let cursor = box.firstElementChild;
    while (cursor && cursor.classList.contains("tree-inline-create")) cursor = cursor.nextElementSibling;
    for (const n of nodes) {
      if (n === cursor) {
        cursor = cursor.nextElementSibling;
      } else {
        box.insertBefore(n, cursor);
      }
    }
  }

  function cleanupSubtree(nodeEl) {
    if (!nodeEl) return;
    const r = nodeEl.dataset?.rel;
    if (r) { nodeMap.delete(r); state.delete(r); }
    for (const sn of nodeEl.querySelectorAll(".tree-node[data-rel]")) {
      if (sn.dataset.rel) { nodeMap.delete(sn.dataset.rel); state.delete(sn.dataset.rel); }
    }
  }

  // —— 展开 / 折叠 ——
  async function expandDir(dirRel) {
    const rel = normRel(dirRel);
    const node = nodeMap.get(rel);
    if (!node || !node._entry?.is_dir) return;
    const childBox = node.querySelector(":scope > .tree-children");
    if (!childBox) return;

    state.set(rel, { expanded: true });
    syncNodeVisual(node, node._entry, depthOf(rel));
    childBox.classList.remove("hidden");

    if (childBox.dataset.loaded !== "1") {
      try {
        const entries = await listDir(projectId, rel);
        reconcileContainer(childBox, entries, depthOf(rel) + 1);
        childBox.dataset.loaded = "1";
      } catch (e) {
        childBox.innerHTML = '<div class="tree-error">加载失败: ' + e + '</div>';
      }
    }
    applyDirty();
    if (window.__refreshAgentDiffDots) window.__refreshAgentDiffDots();
  }

  function collapseDir(dirRel) {
    const rel = normRel(dirRel);
    const node = nodeMap.get(rel);
    if (!node || !node._entry?.is_dir) return;
    const childBox = node.querySelector(":scope > .tree-children");
    state.set(rel, { expanded: false });
    syncNodeVisual(node, node._entry, depthOf(rel));
    if (childBox) childBox.classList.add("hidden");
  }

  async function toggleDir(dirRel) {
    if (state.get(normRel(dirRel))?.expanded) collapseDir(dirRel);
    else await expandDir(dirRel);
  }

  // —— 局部乐观移动：DOM 子树迁移 + 路径重映射（0 请求、保留展开子树）——
  async function moveNode(oldRelRaw, newRelRaw) {
    const oldRel = normRel(oldRelRaw);
    const newRel = normRel(newRelRaw);
    if (!oldRel || !newRel || oldRel === newRel) return;

    const node = nodeMap.get(oldRel);
    const newParent = parentOf(newRel);
    const newName = baseName(newRel);
    const rebase = (p) => (p === oldRel ? newRel : p.startsWith(oldRel + "/") ? newRel + p.slice(oldRel.length) : p);

    // 状态集合路径前缀重映射
    const nextState = new Map();
    for (const [k, v] of state.entries()) nextState.set(rebase(k), v);
    state.clear();
    for (const [k, v] of nextState.entries()) state.set(k, v);

    const nextSelection = new Set([...selection].map(rebase));
    selection.clear();
    for (const r of nextSelection) selection.add(r);

    dirtyRelSet = new Set([...dirtyRelSet].map(rebase));
    if (focusedRel) focusedRel = rebase(focusedRel);
    if (host.dataset.active) host.dataset.active = rebase(host.dataset.active);

    // 目标父目录未展开：移除旧节点并展开目标父目录（其 listDir 会带出移动后的项）
    if (newParent !== rootRel && !state.get(newParent)?.expanded) {
      if (node) { cleanupSubtree(node); node.remove(); }
      await expandDir(newParent);
      applyDirty();
      return;
    }

    const destBox = getChildrenBox(newParent);
    if (!node || !destBox || (newParent !== rootRel && destBox.dataset.loaded !== "1")) {
      // 退化为按父目录重取
      if (node) { cleanupSubtree(node); node.remove(); }
      await applyChanges([{ rel: oldRel, kind: "remove" }, { rel: newRel, kind: "create" }]);
      if (newParent !== rootRel) await expandDir(newParent);
      return;
    }

    // 更新被移动子树内所有节点的 rel 与缩进
    const allNodes = [node, ...node.querySelectorAll(".tree-node[data-rel]")];
    for (const n of allNodes) {
      const prevRel = n.dataset.rel;
      if (prevRel && nodeMap.get(prevRel) === n) nodeMap.delete(prevRel);
    }
    for (const n of allNodes) {
      const prevRel = n.dataset.rel;
      const updatedRel = rebase(prevRel);
      const updatedEntry = { ...n._entry, rel: updatedRel, name: n === node ? newName : n._entry.name };
      syncNodeVisual(n, updatedEntry, depthOf(updatedRel));
      nodeMap.set(updatedRel, n);
    }

    insertNodeSorted(destBox, node);
    applyDirty();
    if (window.__refreshAgentDiffDots) window.__refreshAgentDiffDots();
  }

  function insertNodeSorted(box, nodeEl) {
    const entry = nodeEl._entry;
    const siblings = [...box.querySelectorAll(":scope > .tree-node")].filter((el) => el !== nodeEl);
    let refNode = null;
    for (const sib of siblings) {
      const sEntry = sib._entry;
      if (!sEntry) continue;
      if (!!entry.is_dir !== !!sEntry.is_dir) {
        if (entry.is_dir && !sEntry.is_dir) { refNode = sib; break; }
        continue;
      }
      if (collator.compare(entry.name || "", sEntry.name || "") < 0) { refNode = sib; break; }
    }
    box.insertBefore(nodeEl, refNode);
  }

  function removeNode(relRaw) {
    const rel = normRel(relRaw);
    const node = nodeMap.get(rel);
    if (node) { cleanupSubtree(node); node.remove(); }
    for (const r of [...state.keys()]) if (r === rel || r.startsWith(rel + "/")) state.delete(r);
    for (const r of [...selection]) if (r === rel || r.startsWith(rel + "/")) selection.delete(r);
    for (const r of [...dirtyRelSet]) if (r === rel || r.startsWith(rel + "/")) dirtyRelSet.delete(r);
    applyDirty();
  }

  // —— 全量刷新：并发预取展开目录 + 单帧原地 Diff，不清空 DOM ——
  let refreshToken = 0;
  async function refresh() {
    const token = ++refreshToken;
    const scrollTop = host.scrollTop;

    // 根容器：移除历史残留的 search-hit / inline-create
    for (const c of [...host.children]) {
      if (c.classList.contains("search-hit")) c.remove();
    }

    if (opts.onRootChange) opts.onRootChange(rootRel);

    const dirsToFetch = [rootRel];
    const expList = [...state.entries()]
      .filter(([k, v]) => v && v.expanded && (rootRel === "" || k === rootRel || k.startsWith(rootRel + "/")))
      .map(([k]) => k)
      .sort((a, b) => a.split("/").length - b.split("/").length);
    for (const d of expList) if (!dirsToFetch.includes(d)) dirsToFetch.push(d);

    const results = await Promise.allSettled(
      dirsToFetch.map((dir) => listDir(projectId, dir).then((items) => ({ dir, items })))
    );
    if (token !== refreshToken) return;

    for (const res of results) {
      if (res.status !== "fulfilled") continue;
      const { dir, items } = res.value;
      const box = getChildrenBox(dir);
      if (!box) { state.delete(dir); continue; }
      const depth = dir === rootRel ? 0 : depthOf(dir) + 1;
      reconcileContainer(box, items, depth);
      if (dir !== rootRel) {
        box.dataset.loaded = "1";
        box.classList.remove("hidden");
      }
    }

    host.scrollTop = scrollTop;
    applyDirty();
    if (window.__refreshAgentDiffDots) window.__refreshAgentDiffDots();
  }

  // —— 增量 applyChanges（幂等守卫）——
  async function applyChanges(changes) {
    if (!Array.isArray(changes) || !changes.length) return;
    const dirtyParents = new Set();
    for (const c of changes) {
      const rel = normRel(c.rel);
      if (!rel) continue;
      const existsInDom = nodeMap.has(rel);
      if (c.kind === "create" && existsInDom) continue;
      if (c.kind === "remove" && !existsInDom) continue;
      dirtyParents.add(parentOf(rel));
    }
    if (!dirtyParents.size) return;

    const sortedParents = [...dirtyParents].sort((a, b) => a.split("/").length - b.split("/").length);
    for (const parent of sortedParents) {
      const box = getChildrenBox(parent);
      if (!box) continue;
      if (parent !== rootRel && !state.get(parent)?.expanded) continue;
      try {
        const entries = await listDir(projectId, parent);
        const depth = parent === rootRel ? 0 : depthOf(parent) + 1;
        reconcileContainer(box, entries, depth);
        if (parent !== rootRel) box.dataset.loaded = "1";
      } catch {}
    }
    applyDirty();
    if (window.__refreshAgentDiffDots) window.__refreshAgentDiffDots();
  }

  // —— 逐级展开并选中（保持 reveal 签名）——
  async function reveal(rel) {
    const target = normRel(rel);
    if (!target) return;
    const parts = target.split("/");
    let cur = "";
    for (let i = 0; i < parts.length - 1; i++) {
      cur = cur ? cur + "/" + parts[i] : parts[i];
      if (rootRel && cur !== rootRel && !cur.startsWith(rootRel + "/")) continue;
      if (!state.get(cur)?.expanded) await expandDir(cur);
    }
    selectSingle(target);
    const node = nodeMap.get(target);
    const row = node?.firstElementChild;
    if (row) row.scrollIntoView({ block: "nearest" });
  }

  // —— 内联重命名（调 opts.onRename(rel, dir, newName) 返回 bool；由宿主落盘与刷新）——
  function startRename(relRaw) {
    const rel = normRel(relRaw);
    const node = nodeMap.get(rel);
    if (!node) return;
    const row = node.firstElementChild;
    const label = row?.querySelector(".tree-label");
    if (!label || label.querySelector("input")) return;

    const oldName = node._entry.name;
    const isDir = !!node._entry.is_dir;
    label.textContent = "";

    const input = document.createElement("input");
    input.className = "rename-input";
    input.value = oldName;
    label.appendChild(input);
    input.focus();
    const dot = oldName.lastIndexOf(".");
    if (!isDir && dot > 0) input.setSelectionRange(0, dot); else input.select();

    let done = false;
    const commit = async () => {
      if (done) return;
      done = true;
      const newName = input.value.trim();
      if (!newName || newName === oldName) { label.textContent = oldName; return; }
      label.textContent = oldName;
      const dir = parentOf(rel);
      if (opts.onRename) {
        try { await opts.onRename(rel, dir, newName); } catch {}
      }
    };
    const cancel = () => { if (done) return; done = true; label.textContent = oldName; };

    input.addEventListener("keydown", (e) => {
      e.stopPropagation();
      if (e.key === "Enter") { e.preventDefault(); commit(); }
      else if (e.key === "Escape") { e.preventDefault(); cancel(); }
    });
    input.addEventListener("blur", commit);
    input.addEventListener("click", (e) => e.stopPropagation());
    input.addEventListener("mousedown", (e) => e.stopPropagation());
  }

  // —— 键盘导航 ——
  host.addEventListener("keydown", async (ev) => {
    if (ev.target && ev.target.tagName === "INPUT") return;
    const visible = getVisibleRels();
    if (!visible.length) return;
    const cur = focusedRel || [...selection][0] || visible[0];
    const idx = visible.indexOf(cur);

    if (ev.key === "ArrowDown") {
      ev.preventDefault();
      const next = visible[Math.min(visible.length - 1, idx + 1)] || visible[0];
      if (ev.shiftKey) selectRange(next); else selectSingle(next);
      nodeMap.get(next)?.firstElementChild?.scrollIntoView({ block: "nearest" });
    } else if (ev.key === "ArrowUp") {
      ev.preventDefault();
      const prev = visible[Math.max(0, idx - 1)] || visible[0];
      if (ev.shiftKey) selectRange(prev); else selectSingle(prev);
      nodeMap.get(prev)?.firstElementChild?.scrollIntoView({ block: "nearest" });
    } else if (ev.key === "ArrowRight") {
      ev.preventDefault();
      const node = nodeMap.get(cur);
      if (node?._entry?.is_dir) {
        if (!state.get(cur)?.expanded) await expandDir(cur);
        else { const next = visible[idx + 1]; if (next && next.startsWith(cur + "/")) selectSingle(next); }
      }
    } else if (ev.key === "ArrowLeft") {
      ev.preventDefault();
      const node = nodeMap.get(cur);
      if (node?._entry?.is_dir && state.get(cur)?.expanded) collapseDir(cur);
      else { const p = parentOf(cur); if (p && nodeMap.has(p)) selectSingle(p); }
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      const node = nodeMap.get(cur);
      if (!node) return;
      if (node._entry.is_dir) await toggleDir(cur);
      else onOpenFile(cur, node._entry.name);
    } else if (ev.key === "F2") {
      ev.preventDefault();
      if (cur) startRename(cur);
    }
  });

  // —— 上一层 / 根操作 / 搜索 ——
  async function goUp() {
    if (restrict) return false;
    const up = normRel(rootRel + "/..");
    if (up === rootRel) return false;
    rootRel = up;
    await refresh();
    return true;
  }

  async function setRoot(rel) {
    rootRel = normRel(rel || "");
    await refresh();
    return rootRel;
  }

  async function search(query) {
    const q = (query || "").trim().toLowerCase();
    if (!q) {
      // 退出搜索：清空残留，恢复常规树
      nodeMap.clear();
      state.clear();
      selection.clear();
      await refresh();
      return;
    }
    const hits = [];
    async function walk(dir, depth) {
      if (depth > 8 || hits.length >= 200) return;
      let list;
      try { list = await listDir(projectId, dir); } catch { return; }
      for (const item of sortEntries(list)) {
        if (item.name.toLowerCase().includes(q)) hits.push(item);
        if (item.is_dir && hits.length < 200) await walk(item.rel, depth + 1);
      }
    }
    await walk(rootRel, 0);

    // 搜索结果用独立行，不进入 nodeMap
    host.innerHTML = "";
    for (const entry of hits) {
      const row = document.createElement("div");
      row.className = "tree-row search-hit";
      row.dataset.rel = entry.rel;
      const ic = iconFor(entry, false);
      row.innerHTML = '<span class="tree-twisty empty"></span><span class="tree-icon" style="color:' + ic.color + '">' + ic.svg + '</span><span class="tree-label" title="' + entry.rel + '">' + entry.rel + '</span>';
      row.addEventListener("click", () => { if (!entry.is_dir) onOpenFile(entry.rel, entry.name); });
      host.appendChild(row);
    }
    if (!hits.length) {
      const empty = document.createElement("div");
      empty.className = "tree-error";
      empty.textContent = "无匹配文件";
      host.appendChild(empty);
    }
  }

  return {
    refresh, goUp, search, setRoot, getRoot: () => rootRel,
    getSelection, clearSelection, startRename, selectSingle, setDirty,
    applyChanges, reveal, moveNode, removeNode,
  };
}
