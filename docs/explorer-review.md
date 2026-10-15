# 资源管理器（IDE 文件树）问题汇总 —— 供外部方案评审

> 项目：`deepseek-agent-gui-rust`（Tauri 2，Rust 后端 + 原生 JS 前端，无框架）
> 目标：优化左侧「资源管理器」文件树。核心痛点：**拖动文件到新文件夹后，整个树被全量重建，之前展开的文件夹全部折叠、滚动位置/选中态丢失**。另有多处操作体验问题。
> 本文档汇总所有相关源码与「更改变动 → 刷新」的完整链路，供外部 AI 评估是否有更高效、更稳定的方案。

---

## 0. 技术栈与约束

- 前端：原生 ES Module，无 React/Vue。DOM 手动增删。Tauri `withGlobalTauri` 暴露 `window.__TAURI__`。
- 后端：Rust，Tauri 2 命令（`#[tauri::command]`），文件系统用 `std::fs`，监听用 `notify` + `notify-debouncer-mini`。
- 文件树是**懒加载**（点开目录才 `listDir`），节点顺序由后端 `read_dir` 给出（未排序？见后端）。
- 已有能力：展开/折叠、单击打开文件、Ctrl 多选、右键菜单（新建/重命名/删除/复制/粘贴/在文件管理器中显示等）、拖拽移动/排序、搜索、上一层、每个项目独立 IDE 窗口。
- 刷新来源有三：① 前端主动调用 `tree.refresh()`（用户操作后）；② 用户拖拽 move；③ 后端 watcher 推送 `ide://fs-changes` → 前端 `flushFsChanges` → `tree.applyChanges()` 或 `tree.refresh()`。

---

## 1. 涉及文件清单

| 文件 | 作用 |
|---|---|
| `ui/ide/tree.js` | 文件树全部逻辑：渲染、懒加载、展开/折叠、选择、右键、拖拽移动/排序、搜索、`refresh()`、`applyChanges()` |
| `ui/ide/ide.js` | 宿主：创建树、绑定右键菜单动作（onMove/onCreate/onDelete/…）、文件监听 `installFsWatch`/`flushFsChanges`、历史撤销栈 |
| `ui/ide/api.js` | Tauri 命令封装（`ide_list_dir`、`ide_move`、`ide_create`、`ide_delete` 等） |
| `ui/ide/ide.css` | 树相关样式（`.tree-row`、`.tree-children`、`.drop-target` 等） |
| `ui/ide/index.html` | 资源管理器 DOM 骨架（`#file-tree`、`#refresh-btn`、`#up-btn`、`#file-search`） |
| `src/ide/watch.rs` | 后端文件监听，防抖 180ms，推送 `ide://fs-changes` |
| `src/ide/fs.rs`（或 fs 命令所在） | `ide_list_dir`/`ide_move`/`ide_create`/`ide_delete`/`ide_copy` 等后端实现 |
| `src/ide/window.rs` | 打开 IDE 窗口时注册监听根目录 |

> 注：`fs.rs` 文件名需确认（下节给出命令注册处）。

---

## 2. 关键代码

### 2.1 `ui/ide/tree.js`（核心，全文）

```js
// 左侧文件树：懒加载目录、展开/折叠、点击文件回调、搜索、上一层。
// 图标用内联 SVG（VSCode 风），按类型区分。

import { listDir } from "./api.js";

// —— SVG 图标（省略长 SVG 常量，实际存在 SVG_FOLDER / SVG_FOLDER_OPEN / SVG_FILE / SVG_FILE_CODE / SVG_FILE_MARKDOWN / SVG_FILE_JSON / SVG_FILE_IMAGE）——
// 颜色常量：COLOR_FOLDER=#7fa8ff CODE=#4ec9b0 MD=#9aa4b2 JSON=#e2c08d IMAGE=#c586c0 PLAIN=#8a94a6

// iconFor(entry, expanded) -> {svg, color}：按 is_dir 与扩展名返回图标。
// normRel(rel)：折叠 `..` 与 `.`。
// fileIcon(name)：对外暴露，按文件名取图标（SCM 面板用）。

export function createTree(host, projectId, onOpenFile, restrict, opts = {}) {
  const state = new Map(); // rel -> { expanded }
  let rootRel = "";        // 当前显示根（相对工作目录）
  const activePath = () => host.dataset.active || "";
  const onContextMenu = opts.onContextMenu || null;
  const selection = new Set(); // 当前选中的 rel 集合（支持 Ctrl 多选）

  function applySelection() { /* 遍历 .tree-row，toggle selected */ }
  function selectSingle(rel) { /* 清空后加一个 */ }
  function toggleSelect(rel) { /* 加/删一个 */ }
  function clearSelection() { /* 清空 */ }
  function getSelection() { /* 返回 [{rel,isDir}]，isDir 从 row.dataset 读 */ }

  function makeRow(entry, depth, container) {
    const row = document.createElement("div");
    row.className = "tree-row";
    row.style.paddingLeft = 6 + depth * 14 + "px";
    row.dataset.rel = entry.rel;
    row.dataset.isDir = entry.is_dir ? "1" : "0";
    row._entry = entry;
    if (!entry.is_dir && entry.rel === activePath()) row.classList.add("active");

    // 右键菜单：若不在已选集合则单选；否则保留多选
    row.addEventListener("contextmenu", (ev) => { ... onContextMenu(entry, ev.clientX, ev.clientY) });

    // —— 拖拽移动 ——
    row.draggable = true;
    row.addEventListener("dragstart", (ev) => {
      ev.dataTransfer.effectAllowed = "move";
      ev.dataTransfer.setData("text/plain", entry.rel);
      try { ev.dataTransfer.setData("text/uri-list", host.dataset.abs ? host.dataset.abs + "/" + entry.rel : entry.rel); } catch {}
      host.dataset.dragSrc = entry.rel;
      host.dataset.dragIsDir = entry.is_dir ? "1" : "0";
    });
    row.addEventListener("dragend", () => { /* 清理 drop-target/before/after 与 host.dataset */ });

    // dropZone(ev): 上 25% = "before"，下 25% = "after"，中间 50% = "middle"
    row.addEventListener("dragover", (ev) => {
      const src = host.dataset.dragSrc;
      if (!src || src === entry.rel || entry.rel.startsWith(src + "/")) return;
      ev.preventDefault(); ev.stopPropagation();
      const z = dropZone(ev);
      row.classList.remove("drop-target","drop-before","drop-after");
      if (z === "middle") {
        if (entry.is_dir) { ev.dataTransfer.dropEffect = "move"; row.classList.add("drop-target"); }
        else { ev.dataTransfer.dropEffect = "none"; }
      } else {
        ev.dataTransfer.dropEffect = "move";
        row.classList.add(z === "before" ? "drop-before" : "drop-after");
        host.dataset.dropRel = entry.rel; host.dataset.dropZone = z;
      }
    });
    row.addEventListener("dragleave", () => row.classList.remove("drop-target","drop-before","drop-after"));
    row.addEventListener("drop", (ev) => {
      const src = host.dataset.dragSrc;
      if (!src || src === entry.rel || entry.rel.startsWith(src + "/")) return;
      ev.preventDefault(); ev.stopPropagation();
      const z = dropZone(ev);
      row.classList.remove("drop-target","drop-before","drop-after");
      if (z === "middle") {
        if (entry.is_dir && opts.onMove) opts.onMove(src, entry.rel);   // 移进目录
        return;
      }
      // before/after：跨目录则 move 到目标目录，否则仅视图排序（不落盘）
      const srcDir = src.includes("/") ? src.slice(0, src.lastIndexOf("/")) : "";
      const destDir = entry.rel.includes("/") ? entry.rel.slice(0, entry.rel.lastIndexOf("/")) : "";
      if (srcDir !== destDir) { if (opts.onMove) opts.onMove(src, destDir); return; }
      reorderRow(src, row, z);
    });

    // twisty（展开箭头）、icon、label 组装到 row
    // 目录：创建 childBox(.tree-children.hidden)，点击切换展开/折叠，首次展开时 renderDir 懒加载
    // 文件：点击设为 active 并 onOpenFile(entry.rel, entry.name)
    // 目录/文件还可能有 agent-diff-dot 角标
  }

  async function renderDir(container, relPath, depth) {
    let entries;
    try { entries = await listDir(projectId, relPath); }
    catch (e) { /* 追加 .tree-error */ return; }
    for (const entry of entries) makeRow(entry, depth, container);
  }

  // ============ 全量刷新（问题核心之一）============
  let refreshToken = 0;
  async function refresh() {
    const token = ++refreshToken;
    // 记录当前展开集合
    const expanded = new Set([...state.entries()].filter(([,v]) => v && v.expanded).map(([k]) => k));
    const scrollTop = host.scrollTop;

    host.innerHTML = "";      // ★ 全量清空 DOM
    state.clear();            // ★ 清空展开状态
    if (opts.onRootChange) opts.onRootChange(rootRel);
    await renderDir(host, rootRel, 0);
    if (token !== refreshToken) return;

    // 按层级从浅到深重新展开
    const rels = [...expanded].sort((a,b) => a.split("/").length - b.split("/").length);
    for (const rel of rels) {
      if (token !== refreshToken) return;
      const row = host.querySelector('.tree-row[data-rel="' + CSS.escape(rel) + '"]');
      if (!row) continue;
      if (state.get(rel) && state.get(rel).expanded) continue;
      row.click();            // 触发懒加载
      await waitDirLoaded(rel);
    }
    host.scrollTop = scrollTop;
    applyDirty();
  }

  async function waitDirLoaded(rel) { /* 轮询最多 50×10ms，等 childBox.dataset.loaded */ }
  function parentDepth(rel) { /* 相对 rootRel 的深度 */ }

  // ============ 增量刷新（本意是避免全量重建）============
  async function applyChanges(changes) {
    const byParent = new Map();
    for (const c of changes) {
      const parent = c.rel.includes("/") ? c.rel.slice(0, c.rel.lastIndexOf("/")) : "";
      if (!byParent.has(parent)) byParent.set(parent, []);
      byParent.get(parent).push(c);
    }
    for (const [parent] of byParent) {
      // 找到 parent 对应的容器 box（根=host，否则找 parentRow 的 childBox 且未 hidden）
      let box;
      if (parent === rootRel) box = host;
      else { const pRow = host.querySelector(...); const pBox = pRow && pRow.nextElementSibling; box = (pBox && ... && !pBox.classList.contains("hidden")) ? pBox : null; }
      if (!box) continue;
      const depth = parentDepth(parent) + 1;
      let entries; try { entries = await listDir(projectId, parent); } catch { continue; }
      // 记录该 parent 下已展开的子目录
      const expandedChildren = [...];
      box.innerHTML = "";                       // ★ 只重建该目录层，但仍是全清
      for (const entry of entries) makeRow(entry, depth, box);
      // 恢复子目录展开
      for (const rel of expandedChildren) { const row = box.querySelector(...); if (row && !expanded) { row.click(); await waitDirLoaded(rel); } }
    }
    applyDirty();
    if (window.__refreshAgentDiffDots) window.__refreshAgentDiffDots();
  }

  // 仅调整视图顺序（不落盘）：把 src 行（目录连子容器）移到目标行前/后
  function reorderRow(srcRel, targetRow, zone) { /* insertBefore 移动 DOM */ }

  function goUp() { /* restrict 判断，rootRel = normRel(rootRel+"/..")，refresh() */ }
  async function search(query) { /* 递归扫描（深度≤8，数量≤200），列搜索结果 */ }
  async function setRoot(rel) { rootRel = normRel(rel||""); await refresh(); return rootRel; }
  function startRename(rel) { /* label 换 input，回车提交 */ }

  return {
    refresh, applyChanges, setRoot, goUp, search, startRename, getSelection,
    // ...（还有 clearSelection/selectSingle 等，视实际导出）
  };
}
```

> **`refresh()` 的问题**：每次调用都 `host.innerHTML = ""` + `state.clear()`，然后异步逐个 `row.click()` 重建展开目录。虽然「技术上」会恢复展开集合，但因为：
> 1. 恢复是**串行异步 + 轮询等待**（`waitDirLoaded` 最多 50×10ms/目录），目录多时肉眼可见闪烁、顺序重建；
> 2. 恢复过程中用户若正在交互（或又触发一次 refresh），`refreshToken` 变化会**中途 return**，导致部分展开丢失；
> 3. 选中态 `selection`、`dataset.active`、滚动位置恢复不完整；
> 4. `row.click()` 依赖 DOM 已渲染 + 事件绑定，重建期间节点是「死」的。
>
> **`applyChanges()` 的问题**：只按「变更文件的父目录」重建该层，但该层仍 `box.innerHTML = ""` 全清；并且**只处理父目录当前可见（未折叠）的情况**——如果拖拽导致源目录/目标目录层重建，其他层不动，但源目录消失了文件、目标目录多了文件，通常两个 parent 都会重建，体验尚可；然而**移动目录**时，被移动目录的展开状态会被清掉，且父级顺序可能变化。

### 2.2 `ui/ide/ide.js`（宿主相关片段）

```js
// 创建树（boot 内，节选）
const tree = createTree(document.getElementById("file-tree"), projectId, openFile, restrict, {
  onRootChange: (rel) => { els.sidebarPath.textContent = "/" + (rel || ""); },
  onContextMenu: (entry, x, y) => { ... 弹出菜单，动作含：打开/新建文件/新建文件夹/重命名/删除/复制/剪切/粘贴/在文件管理器显示/设为项目文件夹 ... },
  onMove: async (src, destDir) => {
    try {
      const rel = await movePath(projectId, src, destDir);
      els.statusPath.textContent = "已移动：" + rel;
      closeTabsUnder(src);
      await tree.refresh();               // ★ 移动后全量刷新 → 展开状态丢失的主因
      pushHistory({
        label: "移动 " + src,
        undo: async () => { await movePath(projectId, rel, parentOf(src)); await tree.refresh(); },
        redo: async () => { await movePath(projectId, src, destDir); await tree.refresh(); },
      });
    } catch (e) { els.statusPath.textContent = "移动失败：" + e; }
  },
});
await tree.refresh();

els.refreshBtn.onclick = () => tree.refresh();
els.upBtn.onclick = () => goUpAndSync();

// 其它动作（新建/删除/粘贴/重命名/复制）也普遍是「操作后 await tree.refresh()」，
// 见 ide.js 中 newEntry/removeEntry/pasteInto 等，均触发全量 refresh。
```

```js
// 文件监听（外部/其它来源改动）—— installFsWatch / flushFsChanges
function installFsWatch() {
  const tauriEvent = window.__TAURI__ && window.__TAURI__.event;
  if (!tauriEvent) return;
  let timer = null, flushing = false;
  const pendingChanges = new Map();   // rel -> {rel, kind, is_dir}
  const pendingDocs = new Map();      // rel -> kind

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
    timer = setTimeout(flushFsChanges, 250);   // 250ms 聚合
  });

  async function flushFsChanges() {
    if (flushing) return;
    flushing = true;
    try {
      const changes = [...pendingChanges.values()]; pendingChanges.clear();
      const docsToSync = new Map(pendingDocs); pendingDocs.clear();

      if (changes.length && tree && tree.applyChanges) {
        try { await tree.applyChanges(changes); } catch {}   // 优先增量
      } else if (changes.length) {
        try { await tree.refresh(); } catch {}                // 退化全量
      }
      // ...再同步已打开文档内容（readFile → model.setValue，保留本地未保存修改）...
      if ((changes.length || docsToSync.size) && gitState && gitState.is_repo) refreshGit().catch(() => {});
    } finally {
      flushing = false;
      if ((pendingChanges.size || pendingDocs.size) && !timer) timer = setTimeout(flushFsChanges, 250);
    }
  }
}
```

> **双重刷新**：用户拖拽 move 时，`onMove` 先 `await tree.refresh()`（全量），同时后端 watcher 也会因文件移动推送 `ide://fs-changes` → 250ms 后 `applyChanges()`（又一次重建）。两次刷新叠加，加剧闪烁与展开丢失。

### 2.3 `ui/ide/api.js`（fs 命令封装）

```js
export function listDir(projectId, relPath) { return invoke("ide_list_dir", { projectId, relPath: relPath || "" }); }
export function readFile(projectId, relPath) { return invoke("ide_read_file", { projectId, relPath }); }
export function writeFile(projectId, relPath, content) { return invoke("ide_write_file", { projectId, relPath, content }); }
export function deletePath(projectId, relPath) { return invoke("ide_delete", { projectId, relPath }); }
export function restorePath(projectId, trashRel, destRel) { return invoke("ide_restore", { projectId, trashRel, destRel: destRel || null }); }
export function copyPath(projectId, relPath, destDir) { return invoke("ide_copy", { projectId, relPath, destDir }); }
export function movePath(projectId, relPath, destDir, newName) { return invoke("ide_move", { projectId, relPath, destDir, newName: newName || null }); }
export function createPath(projectId, parentDir, name, isDir) { return invoke("ide_create", { projectId, parentDir, name, isDir }); }
export function readBinary(projectId, relPath) { return invoke("ide_read_binary", { projectId, relPath }); }
export function openExternal(projectId, relPath) { return invoke("ide_open_external", { projectId, relPath }); }
```

### 2.4 `src/ide/watch.rs`（后端监听，防抖 180ms）

```rust
// 关键点：
// - Debouncer 180ms，回调 emit_batch。
// - is_ignored(rel)：跳过 .git/node_modules/target*/dist/build/... 等。
// - 维护 roots: Map<PathBuf(根), project_id>，suppressions: Map<PathBuf, 过期 Instant>，seen: HashSet<PathBuf>。
// - 每个事件：计算 rel，若不存在→"remove"，首次见→"create"，否则→"modify"。只发 create/remove 之外的都叫 modify。
//   ★ 注意：文件「移动」在 watcher 视角是「旧路径 remove + 新路径 create」，不会带「移动」语义。
//   ★ 目的端新路径首次见→"create"；源端旧路径消失→"remove"。
// - 定向 emit 到 label = "ide-<project_id>" 的窗口。
// - suppress_path：IDE 内部写盘前可抑制 800ms（避免自己触发的事件回灌）。
```

```rust
fn emit_batch(app, roots, suppressions, seen, events) {
  // 清理过期 suppression
  // 对每个事件：先查 suppression 跳过；再匹配 root 求 rel；is_ignored 跳过；
  // 判断 exists / is_dir；kind = !exists ? remove : (seen.insert ? create : modify)
  // 按 project_id 聚合，emit "ide://fs-changes"
}
```

### 2.5 `ui/ide/index.html`（资源管理器骨架）

```html
<button class="act-item active" data-view="explorer" title="资源管理器">…</button>
<div class="sidebar-pane explorer-pane" id="explorer-pane">
  <div class="sidebar-head">
    <span class="sidebar-title">资源管理器</span>
    <div class="sidebar-actions">
      <button id="up-btn" class="icon-btn" title="上一层">↑</button>
      <button id="refresh-btn" class="icon-btn" title="刷新">⟳</button>
    </div>
  </div>
  <div id="sidebar-path" class="sidebar-path" title="当前文件夹">/</div>
  <div class="search-wrap"><input id="file-search" class="file-search" type="text" placeholder="搜索文件…" /></div>
  <div id="file-tree" class="file-tree"></div>
</div>
```

### 2.6 `ui/ide/ide.css`（树样式，节选）

```css
/* 树行、缩进、选中、hover、拖拽高亮、children 容器等 */
/* .tree-row[draggable="true"] { cursor: grab; } */
/* .tree-children.hidden { display: none; } */
/* .drop-target / .drop-before / .drop-after 用于拖拽视觉反馈 */
```

---

## 3. 问题清单

### P0：拖拽移动 → 全量重建 → 展开状态丢失（用户主诉）
- `onMove` 成功后无条件 `await tree.refresh()`；`refresh()` 是 `innerHTML="" + state.clear()` 的全量重建，再靠串行 `row.click()` + 轮询重建展开。
- 同时后端 watcher 又推送 `create/remove`，`flushFsChanges` 再 `applyChanges()` 重建一次。
- 结果：展开的目录折叠、滚动/选中/active 丢失、可见闪烁。
- 期望：局部更新（把源父目录层删掉被移项、目标父目录层插入被移项），其余层和展开状态完全不动；或基于「虚拟 DOM/diff」的最小 patch。

### P1：`applyChanges` 的局限
- 仍对被变更父目录 `box.innerHTML=""` 全清（不是真增量）。
- 只处理「父目录当前可见」；源/目标目录若折叠或不在视图内，可能不更新。
- 移动**目录**时，被移动目录的展开子树丢失。
- 无法区分「移动」与「新建+删除」，可能造成两次无关重建。

### P1：`refresh()` 的并发安全
- `refreshToken` 中途 return 会导致展开恢复不完整。
- `waitDirLoaded` 轮询式等待，目录多时慢且脆弱（依赖 `dataset.loaded` 时序）。

### P2：其他体验问题（用户提到「好多不好操作的地方」，需列举）
- 拖拽 before/after 排序**不落盘**（仅视图顺序），刷新后又变回后端顺序——用户会困惑。
- 后端 `list_dir` 是否排序？若未排序，视图顺序不稳定。
- 多选拖拽？目前 `dragstart` 只带单个 `entry.rel`，多选拖动未实现。
- 双击 vs 单击打开？目前单击即打开。
- 搜索是每次递归扫描（深度≤8，数量≤200），大项目慢；无缓存。
- 无「刷新时保持展开」的稳定保证；无「定位到当前文件」。
- 拖到空白区域=移动到根目录？未实现（只处理落在行上）。
- 展开/折叠图标与 hover 区域较小。

---

## 4. 期望的改进方向（供外部评估）

1. **真正的增量/差异更新**：保留节点实例与展开状态，只对受影响的父目录做「移除旧子项/插入新子项」的最小 DOM 操作；或引入轻量虚拟 DOM/diff（如小型自研 vdom，或引入 `preact`/`solid-js`/`lit`/`uhtml` 之类极小库——需评估是否可接受新增依赖）。
2. **状态外置**：把 `expanded`/`selection`/`active`/`scrollTop` 提升为显式、可序列化的视图状态，渲染只读该状态，避免「重建后靠 click 恢复」。
3. **去重复刷新**：`onMove` 后不要既全量 refresh 又让 watcher 再刷；或让后端在自身写操作时 `suppress_path` 抑制 watcher（已有 `suppress_path` 能力，但前端调用 move 时未使用）。
4. **区分移动语义**：前端 move 后直接本地 patch（从 src 父层移除、向 dest 父层插入），不依赖 watcher。
5. **拖拽排序落盘**（若确需）：需要后端支持「重排序」持久化（如写出索引/或依赖名称排序）。
6. **多选拖拽**、**拖到空白=移根**、**刷新保持展开+滚动+选中**。

---

## 5. 请外部 AI 重点回答

- 在「原生 JS、无框架」约束下，做**稳定的增量文件树更新**最务实的方案是什么？（自研 patch vs 引入极小 vdom vs 其它）
- 如何彻底消除「拖拽后全量重建」与「watcher 双刷」？
- 如何处理「移动目录时保留其展开子树」？
- 展开/选中/滚动/active 的状态管理与恢复的最佳实践？
- 是否应把文件树的数据源改为「前端持有的树模型 + 后端只给 dir 列表」并由前端统一 diff？
- 拖拽排序若不落盘，是否应直接禁止，避免误导？