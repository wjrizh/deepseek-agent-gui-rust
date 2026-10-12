// 左侧文件树：懒加载目录、展开/折叠、点击文件回调、搜索、上一层。
// 图标用内联 SVG（VSCode 风），按类型区分。

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

function iconFor(entry, expanded) {
  if (entry.is_dir) {
    return { svg: expanded ? SVG_FOLDER_OPEN : SVG_FOLDER, color: COLOR_FOLDER };
  }
  const n = entry.name.toLowerCase();
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
  const parts = (rel || "").split("/").filter((p) => p && p !== ".");
  const out = [];
  for (const p of parts) {
    if (p === "..") {
      if (out.length) out.pop();
      else out.push("..");
    } else out.push(p);
  }
  return out.join("/");
}

export function createTree(host, projectId, onOpenFile, restrict, opts = {}) {
  const state = new Map(); // rel -> { expanded }
  let rootRel = "";        // 当前显示根（相对工作目录）
  const activePath = () => host.dataset.active || "";
  const onContextMenu = opts.onContextMenu || null;
  const selection = new Set(); // 当前选中的 rel 集合（支持 Ctrl 多选）

  function applySelection() {
    host.querySelectorAll(".tree-row").forEach((r) => {
      r.classList.toggle("selected", selection.has(r.dataset.rel));
    });
  }
  function selectSingle(rel) {
    selection.clear();
    selection.add(rel);
    applySelection();
  }
  function toggleSelect(rel) {
    if (selection.has(rel)) selection.delete(rel);
    else selection.add(rel);
    applySelection();
  }
  function clearSelection() {
    selection.clear();
    applySelection();
  }
  function getSelection() {
    return [...selection].map((rel) => {
      const row = host.querySelector('.tree-row[data-rel="' + CSS.escape(rel) + '"]');
      return { rel, isDir: row ? row.dataset.isDir === "1" : false };
    });
  }

  function makeRow(entry, depth, container) {
    const row = document.createElement("div");
    row.className = "tree-row";
    row.style.paddingLeft = 6 + depth * 14 + "px";
    row.dataset.rel = entry.rel;
    row.dataset.isDir = entry.is_dir ? "1" : "0";
    row._entry = entry;
    if (!entry.is_dir && entry.rel === activePath()) row.classList.add("active");

    row.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      // 若右键的行不在已选集合中，则单选它；否则保留多选集合。
      if (!selection.has(entry.rel)) selectSingle(entry.rel);
      if (onContextMenu) onContextMenu(entry, ev.clientX, ev.clientY);
    });

    // —— 拖拽移动 ——
    row.draggable = true;
    row.addEventListener("dragstart", (ev) => {
      ev.dataTransfer.effectAllowed = "move";
      ev.dataTransfer.setData("text/plain", entry.rel);
      // 供“拖到外部应用”使用（若后端插件可用，会在 drop 外部时接管）。
      try {
        ev.dataTransfer.setData("text/uri-list", host.dataset.abs ? host.dataset.abs + "/" + entry.rel : entry.rel);
      } catch {}
      host.dataset.dragSrc = entry.rel;
      host.dataset.dragIsDir = entry.is_dir ? "1" : "0";
    });
    row.addEventListener("dragend", () => {
      host.querySelectorAll(".tree-row.drop-target, .tree-row.drop-before, .tree-row.drop-after")
        .forEach((r) => r.classList.remove("drop-target", "drop-before", "drop-after"));
      delete host.dataset.dragSrc;
      delete host.dataset.dragIsDir;
      delete host.dataset.dropRel;
      delete host.dataset.dropZone;
    });

    // 拖拽落在本行的哪个区域：
    //   上 25% / 下 25% → 排序（仅调整视图顺序，不落盘）
    //   中间 50%       → 目录则 move 进该目录；文件则不处理
    function dropZone(ev) {
      const r = row.getBoundingClientRect();
      const y = ev.clientY - r.top;
      if (y <r.height * 0.25) return "before";
      if (y > r.height * 0.75) return "after";
      return "middle";
    }

    row.addEventListener("dragover", (ev) => {
      const src = host.dataset.dragSrc;
      if (!src || src === entry.rel || entry.rel.startsWith(src + "/")) return;
      ev.preventDefault();
      ev.stopPropagation();
      const z = dropZone(ev);
      row.classList.remove("drop-target", "drop-before", "drop-after");
      if (z === "middle") {
        if (entry.is_dir) { ev.dataTransfer.dropEffect = "move"; row.classList.add("drop-target"); }
        else { ev.dataTransfer.dropEffect = "none"; }
      } else {
        ev.dataTransfer.dropEffect = "move";
        row.classList.add(z === "before" ? "drop-before" : "drop-after");
        host.dataset.dropRel = entry.rel;
        host.dataset.dropZone = z;
      }
    });
    row.addEventListener("dragleave", () => {
      row.classList.remove("drop-target", "drop-before", "drop-after");
    });
    row.addEventListener("drop", (ev) => {
      const src = host.dataset.dragSrc;
      if (!src || src === entry.rel || entry.rel.startsWith(src + "/")) return;
      ev.preventDefault();
      ev.stopPropagation();
      const z = dropZone(ev);
      row.classList.remove("drop-target", "drop-before", "drop-after");
      if (z === "middle") {
        if (entry.is_dir && opts.onMove) opts.onMove(src, entry.rel);
        return;
      }
      const srcDir = src.includes("/") ? src.slice(0, src.lastIndexOf("/")) : "";
      const destDir = entry.rel.includes("/") ? entry.rel.slice(0, entry.rel.lastIndexOf("/")) : "";
      if (srcDir !== destDir) {
        if (opts.onMove) opts.onMove(src, destDir);
        return;
      }
      reorderRow(src, row, z);
    });

    const twisty = document.createElement("span");
    twisty.className = "tree-twisty" + (entry.is_dir ? "" : " empty");
    twisty.innerHTML = entry.is_dir
      ? '<svg viewBox="0 0 24 24" width="14" height="14" fill="none"><path d="M9 6l6 6-6 6" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>'
      : "";

    const icon = document.createElement("span");
    icon.className = "tree-icon";
    const ic = iconFor(entry, false);
    icon.innerHTML = ic.svg;
    icon.style.color = ic.color;

    const label = document.createElement("span");
    label.className = "tree-label";
    label.textContent = entry.name;

    row.append(twisty, icon, label);
    const unread = window.__hasUnreadAgentDiff
      ? (entry.is_dir
          ? (window.__hasUnreadAgentDiffUnder && window.__hasUnreadAgentDiffUnder(entry.rel))
          : window.__hasUnreadAgentDiff(entry.rel))
      : false;
    if (unread) {
      const dot = document.createElement("span");
      dot.className = "agent-diff-dot";
      row.append(dot);
    }
    container.appendChild(row);

    if (entry.is_dir) {
      const childBox = document.createElement("div");
      childBox.className = "tree-children hidden";
      container.appendChild(childBox);
      row.addEventListener("click", async (ev) => {
        if (ev.ctrlKey || ev.metaKey) { toggleSelect(entry.rel); return; }
        selectSingle(entry.rel);
        const expanded = state.get(entry.rel)?.expanded;
        if (expanded) {
          childBox.classList.add("hidden");
          twisty.classList.remove("open");
          const c = iconFor(entry, false);
          icon.innerHTML = c.svg;
          icon.style.color = c.color;
          state.set(entry.rel, { expanded: false });
        } else {
          if (!childBox.dataset.loaded) {
            await renderDir(childBox, entry.rel, depth + 1);
            childBox.dataset.loaded = "1";
          }
          childBox.classList.remove("hidden");
          twisty.classList.add("open");
          const c = iconFor(entry, true);
          icon.innerHTML = c.svg;
          icon.style.color = c.color;
          state.set(entry.rel, { expanded: true });
        }
      });
    } else {
      row.addEventListener("click", (ev) => {
        if (ev.ctrlKey || ev.metaKey) { toggleSelect(entry.rel); return; }
        selectSingle(entry.rel);
        host.querySelectorAll(".tree-row.active").forEach((r) => r.classList.remove("active"));
        row.classList.add("active");
        host.dataset.active = entry.rel;
        onOpenFile(entry.rel, entry.name);
      });
    }
  }

  async function renderDir(container, relPath, depth) {
    let entries;
    try {
      entries = await listDir(projectId, relPath);
    } catch (e) {
      const err = document.createElement("div");
      err.className = "tree-error";
      err.textContent = "读取失败：" + e;
      container.appendChild(err);
      return;
    }
    for (const entry of entries) makeRow(entry, depth, container);
  }

  let refreshToken = 0;
  async function refresh() {
    const token = ++refreshToken;
    const expanded = new Set(
      [...state.entries()].filter(([, v]) => v && v.expanded).map(([k]) => k)
    );
    const scrollTop = host.scrollTop;

    host.innerHTML = "";
    state.clear();
    if (opts.onRootChange) opts.onRootChange(rootRel);
    await renderDir(host, rootRel, 0);
    if (token !== refreshToken) return;

    const rels = [...expanded].sort(
      (a, b) => a.split("/").length - b.split("/").length
    );
    for (const rel of rels) {
      if (token !== refreshToken) return;
      const row = host.querySelector('.tree-row[data-rel="' + CSS.escape(rel) + '"]');
      if (!row) continue;
      if (state.get(rel) && state.get(rel).expanded) continue;
      row.click();
      await waitDirLoaded(rel);
    }
    host.scrollTop = scrollTop;
    applyDirty();
  }

  async function waitDirLoaded(rel) {
    for (let i = 0; i < 50; i++) {
      const row = host.querySelector('.tree-row[data-rel="' + CSS.escape(rel) + '"]');
      const box = row && row.nextElementSibling;
      if (box && box.classList.contains("tree-children") && box.dataset.loaded) return;
      await new Promise((r) => setTimeout(r, 10));
    }
  }

  function parentDepth(rel) {
    if (rel === "" || rel === rootRel) return 0;
    const trimmed = rel === rootRel ? "" : rel.startsWith(rootRel + "/") ? rel.slice(rootRel.length + 1) : rel;
    return trimmed === "" ? 0 : trimmed.split("/").length;
  }

  async function applyChanges(changes) {
    const byParent = new Map();
    for (const c of changes) {
      const parent = c.rel.includes("/") ? c.rel.slice(0, c.rel.lastIndexOf("/")) : "";
      if (!byParent.has(parent)) byParent.set(parent, []);
      byParent.get(parent).push(c);
    }

    for (const [parent] of byParent) {
      let box;
      if (parent === rootRel) {
        box = host;
      } else {
        const pRow = host.querySelector('.tree-row[data-rel="' + CSS.escape(parent) + '"]');
        const pBox = pRow && pRow.nextElementSibling;
        box = (pBox && pBox.classList.contains("tree-children") && !pBox.classList.contains("hidden"))
          ? pBox : null;
      }
      if (!box) continue;
      const depth = parentDepth(parent) + 1;

      let entries;
      try { entries = await listDir(projectId, parent); }
      catch { continue; }

      const expandedChildren = [];
      for (const [rel, v] of state.entries()) {
        if (!v || !v.expanded) continue;
        const prefix = parent === "" ? "" : parent + "/";
        if (!rel.startsWith(prefix)) continue;
        const rest = rel.slice(prefix.length);
        if (!rest.includes("/")) expandedChildren.push(rel);
      }

      box.innerHTML = "";
      for (const entry of entries) makeRow(entry, depth, box);

      for (const rel of expandedChildren) {
        const row = box.querySelector('.tree-row[data-rel="' + CSS.escape(rel) + '"]');
        if (row && !(state.get(rel) && state.get(rel).expanded)) {
          row.click();
          await waitDirLoaded(rel);
        }
      }
    }
    applyDirty();
    if (window.__refreshAgentDiffDots) window.__refreshAgentDiffDots();
  }

  // 仅调整视图顺序：把 src 行（目录则连同其子容器）移到目标行前/后。不落盘。
  function reorderRow(srcRel, targetRow, zone) {
    const srcRow = host.querySelector('.tree-row[data-rel="' + CSS.escape(srcRel) + '"]');
    if (!srcRow || srcRow === targetRow) return;
    // 收集要移动的节点：行本身 + （目录的）紧跟其后的 children 容器。
    const moving = [srcRow];
    if (srcRow.dataset.isDir === "1") {
      const cb = srcRow.nextElementSibling;
      if (cb && cb.classList.contains("tree-children")) moving.push(cb);
    }
    // 目标节点所在父容器。
    const parent = targetRow.parentElement;
    if (zone === "before") {
      for (const n of moving) parent.insertBefore(n, targetRow);
    } else {
      const anchor = targetRow.nextElementSibling;
      for (const n of moving) parent.insertBefore(n, anchor);
    }
  }

  // 进入上一层（受 restrict 限制）。
  function goUp() {
    if (restrict) return false;
    const up = normRel(rootRel + "/..");
    if (up === rootRel) return false;
    // 已到文件系统根（up 仍含前导 .. 且无更多可去）
    rootRel = up;
    refresh();
    return true;
  }

  // 简单文件搜索：从 rootRel 递归扫描（限深度与数量），过滤文件名，展开命中路径。
  async function search(query) {
    const q = (query || "").trim().toLowerCase();
    if (!q) { refresh(); return; }
    host.innerHTML = "";
    state.clear();
    const results = document.createElement("div");
    results.className = "search-results";
    host.appendChild(results);
    let count = 0;
    const MAX = 200;

    async function walk(relPath, depth) {
      if (count >= MAX || depth > 8) return;
      let entries;
      try { entries = await listDir(projectId, relPath); } catch { return; }
      for (const entry of entries) {
        if (count >= MAX) return;
        if (entry.name.toLowerCase().includes(q)) {
          const row = document.createElement("div");
          row.className = "tree-row search-hit";
          const icon = document.createElement("span");
          icon.className = "tree-icon";
          const ic = iconFor(entry, false);
          icon.innerHTML = ic.svg;
          icon.style.color = ic.color;
          const label = document.createElement("span");
          label.className = "tree-label";
          label.textContent = entry.rel;
          row.append(icon, label);
          if (!entry.is_dir) {
            row.addEventListener("click", () => {
              results.querySelectorAll(".active").forEach((r) => r.classList.remove("active"));
              row.classList.add("active");
              host.dataset.active = entry.rel;
              onOpenFile(entry.rel, entry.name);
            });
          }
          results.appendChild(row);
          count++;
        }
        if (entry.is_dir) await walk(entry.rel, depth + 1);
      }
    }
    await walk(rootRel, 0);
    if (count === 0) {
      const empty = document.createElement("div");
      empty.className = "tree-error";
      empty.textContent = "无匹配文件";
      results.appendChild(empty);
    }
  }

  // 直接设置显示根（用于“设为项目文件夹”后聚焦到该层）。
  async function setRoot(rel) {
    rootRel = normRel(rel || "");
    await refresh();
    return rootRel;
  }

  // 内联重命名：把行的 label 换成输入框，回车提交（同目录改名），Esc 取消。
  function startRename(rel) {
    const row = host.querySelector('.tree-row[data-rel="' + CSS.escape(rel) + '"]');
    if (!row) return;
    const label = row.querySelector(".tree-label");
    if (!label || row.querySelector(".rename-input")) return;
    const oldName = label.textContent;
    const input = document.createElement("input");
    input.className = "rename-input";
    input.value = oldName;
    label.replaceWith(input);
    input.focus();
    const dot = oldName.lastIndexOf(".");
    if (dot > 0) input.setSelectionRange(0, dot); else input.select();

    let done = false;
    const commit = async () => {
      if (done) return;
      done = true;
      const newName = input.value.trim();
      if (!newName || newName === oldName) {
        input.replaceWith(label);
        return;
      }
      const dir = rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
      if (opts.onRename) {
        const ok = await opts.onRename(rel, dir, newName);
        if (!ok) input.replaceWith(label);
      } else input.replaceWith(label);
    };
    const cancel = () => { if (done) return; done = true; input.replaceWith(label); };

    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); commit(); }
      else if (e.key === "Escape") { e.preventDefault(); cancel(); }
    });
    input.addEventListener("blur", commit);
    input.addEventListener("click", (e) => e.stopPropagation());
    input.addEventListener("mousedown", (e) => e.stopPropagation());
  }

  // 脏标记：接收脏文件 rel 集合，向所有祖先目录冒泡，给已渲染的行加 .dirty。
  let dirtyRelSet = new Set();
  function applyDirty() {
    const dirs = new Set();
    for (const rel of dirtyRelSet) {
      const parts = rel.split("/");
      for (let i = 1; i <parts.length; i++) {
        dirs.add(parts.slice(0, i).join("/"));
      }
    }
    host.querySelectorAll(".tree-row").forEach((row) => {
      const rel = row.dataset.rel;
      const isDir = row.dataset.isDir === "1";
      const dirty = isDir ? dirs.has(rel) : dirtyRelSet.has(rel);
      row.classList.toggle("dirty", dirty);
    });
    if (window.__refreshGitDecorations) window.__refreshGitDecorations();
  }
  function setDirty(relSet) {
    dirtyRelSet = new Set(relSet);
    applyDirty();
  }

  return {
    refresh, goUp, search, setRoot, getRoot: () => rootRel,
    getSelection, clearSelection, startRename, selectSingle, setDirty,
    applyChanges,
  };
}