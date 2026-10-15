# 资源管理器（IDE 文件树）优化 —— 完整上下文与改造约束（供外部 AI）

> 目标：优化 `deepseek-agent-gui-rust` 的 IDE 左侧「资源管理器」文件树。
> 主诉痛点：**拖动文件/文件夹到新位置后，整个树被全量重建，之前展开的目录全部折叠，滚动/选中丢失、界面闪烁**。
> 本文档给出**现有全部相关源码**、**现有对外 API 契约**、**已核对的差异点**、以及**外部 AI 必须遵守的约束**。请基于现有代码做**增量、安全**的改造，不要凭空替换整文件。

## 0. 项目与技术栈

- Tauri 2 桌面应用；Rust 后端（`#[tauri::command]`），前端原生 ES Module（无框架，无打包器）。
- 前端通过 `window.__TAURI__.core.invoke` 调后端；事件用 `window.__TAURI__.event.listen`。
- 文件树：懒加载（展开目录才 `listDir`），节点由后端 `read_dir` 给出。
- 文件监听：Rust `notify` + `notify-debouncer-mini`（防抖 180ms）推送 `ide://fs-changes`。
- 平台：Linux x86_64（webkit2gtk）。路径统一用正斜杠相对工作目录。

## 1. 涉及文件

| 文件 | 作用 |
|---|---|
| `src/ide/fs.rs` | 文件系统命令：`ide_list_dir`/`ide_read_file`/`ide_write_file`/`ide_delete`/`ide_restore`/`ide_purge_trash`/`ide_copy`/`ide_move`/`ide_create` |
| `src/ide/watch.rs` | 文件监听，推送 `ide://fs-changes`；含 `suppress_path`（实例方法） |
| `src/ide/path.rs` | `resolve(root, rel, restrict, must_exist)`、`to_rel(root, abs)` 等 |
| `src/lib.rs` | 命令注册、`IdeWatcher` 初始化与 `manage` |
| `ui/ide/tree.js` | 文件树全部逻辑 |
| `ui/ide/ide.js` | 宿主：创建树、右键菜单、`onMove`/`newEntry`/`removeEntry`/`pasteInto`、文件监听、Tab 管理、历史撤销 |
| `ui/ide/api.js` | Tauri 命令封装 |
| `ui/ide/index.html` | 资源管理器 DOM 骨架 |
| `ui/ide/ide.css` | 树样式 |


## 2. 后端源码

### 2.1 `src/ide/fs.rs`（关键部分）

```rust
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub rel: String,      // 相对工作目录，正斜杠
    pub name: String,
    pub is_dir: bool,
    pub size: u64,        // 目录为 0
    pub is_symlink: bool,
}

const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;
pub const TRASH_DIR: &str = ".ligong-ide-trash";

async fn project_root(state: &State<'_, AppState>, project_id: &str) -> Result<PathBuf, String> {
    let inner = state.inner.lock().await;
    let dir = inner.projects.workdir_of(project_id).ok_or_else(|| "项目未设置工作目录".to_string())?;
    let root = std::path::PathBuf::from(dir);
    if !root.is_dir() { return Err(format!("工作目录不存在：{}", root.display())); }
    Ok(root)
}
async fn project_restrict(state: &State<'_, AppState>, project_id: &str) -> bool {
    let inner = state.inner.lock().await;
    inner.projects.restrict_of(project_id)
}

#[tauri::command]
pub async fn ide_list_dir(
    project_id: String,
    rel_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<DirEntry>, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let rel = rel_path.unwrap_or_default();
    let dir = path::resolve(&root, &rel, restrict, false)?;
    if !dir.is_dir() { return Err(format!("不是目录：{}", dir.display())); }

    let mut out: Vec<DirEntry> = Vec::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("读取目录失败: {e}"))?;
    for ent in rd {
        let ent = match ent { Ok(e) => e, Err(_) => continue };
        let name = ent.file_name().to_string_lossy().to_string();
        if name == TRASH_DIR { continue; }
        let full = ent.path();
        let meta = match std::fs::symlink_metadata(&full) { Ok(m) => m, Err(_) => continue };
        let is_symlink = meta.file_type().is_symlink();
        let is_dir = if is_symlink {
            std::fs::metadata(&full).map(|m| m.is_dir()).unwrap_or(false)
        } else { meta.is_dir() };
        let size = if is_dir { 0 } else { meta.len() };
        out.push(DirEntry { rel: path::to_rel(&root, &full), name, is_dir, size, is_symlink });
    }

    // 目录优先，其次名称（不区分大小写）排序。
    out.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}
```

`ide_move` 全文：

```rust
#[tauri::command]
pub async fn ide_move(
    project_id: String,
    rel_path: String,
    dest_dir: String,
    new_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let root = project_root(&state, &project_id).await?;
    let restrict = project_restrict(&state, &project_id).await;
    let src = path::resolve(&root, &rel_path, restrict, false)?;
    let dest = path::resolve(&root, &dest_dir, restrict, true)?;
    if !src.exists() { return Err(format!("源不存在：{}", src.display())); }
    if !dest.is_dir() { return Err(format!("目标目录不存在：{}", dest.display())); }
    let name = match new_name {
        Some(n) if !n.trim().is_empty() => {
            let n = n.trim().to_string();
            if n.contains('/') || n.contains('\\') { return Err("名称不能包含路径分隔符".to_string()); }
            std::ffi::OsString::from(n)
        }
        _ => src.file_name().ok_or_else(|| "无法取得源名称".to_string())?.to_os_string(),
    };
    let target = dest.join(&name);
    if target == src { return Ok(path::to_rel(&root, &src)); }
    if target.exists() { return Err(format!("目标已存在：{}", path::to_rel(&root, &target))); }
    std::fs::rename(&src, &target).map_err(|e| format!("移动失败: {e}"))?;
    Ok(path::to_rel(&root, &target))
}
```

其余命令（`ide_read_file`/`ide_write_file` 原子写、`ide_delete` 移入 `.ligong-ide-trash/<nanos>/` 并写 `.orig`、`ide_restore`、`ide_purge_trash`、`ide_copy` 递归复制、`ide_create`）略。


### 2.2 `src/ide/watch.rs`（关键部分）

```rust
//! IDE 文件系统监听：外部改动时通知对应 IDE 窗口刷新。
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, DebouncedEvent};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, serde::Serialize)]
pub struct FsChange { pub project_id: String, pub rel: String, pub kind: String, pub is_dir: bool }

type RootMap = Arc<Mutex<HashMap<PathBuf, String>>>;
type SuppressionMap = Arc<Mutex<HashMap<PathBuf, Instant>>>;
type SeenSet = Arc<Mutex<HashSet<PathBuf>>>;

pub struct IdeWatcher {
    watcher: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    roots: RootMap,
    suppressions: SuppressionMap,
}

fn is_ignored(rel: &str) -> bool {
    rel.split('/').any(|seg| {
        if matches!(seg, ".git"|".svn"|".hg"|".ligong-ide-trash"|"node_modules"|"dist"|"build"|"out"|"bin"|"pkg"
                |"vendor"|"Pods"|"site-packages"|"__pycache__"|".playwright-mcp"|".turbo"|".next"|".venv"|".cache") { return true; }
        seg.starts_with("target") || seg.starts_with(".fingerprint") || seg.ends_with(".tmp") || seg.contains(".tmp_") || seg.ends_with('~')
    })
}

impl IdeWatcher {
    pub fn new(app: AppHandle) -> Result<Self, String> {
        let roots: RootMap = Arc::new(Mutex::new(HashMap::new()));
        let suppressions: SuppressionMap = Arc::new(Mutex::new(HashMap::new()));
        let seen: SeenSet = Arc::new(Mutex::new(HashSet::new()));
        let roots_cb = roots.clone(); let supp_cb = suppressions.clone(); let seen_cb = seen.clone();
        let debouncer = new_debouncer(Duration::from_millis(180), move |res: DebounceEventResult| {
            let Ok(events) = res else { return };
            emit_batch(&app, &roots_cb, &supp_cb, &seen_cb, events);
        }).map_err(|e| format!("创建文件监听失败: {e}"))?;
        Ok(Self { watcher: debouncer, roots, suppressions })
    }

    /// IDE 内部写盘前调用：抑制该路径未来 800ms 的事件。
    pub fn suppress_path(&self, path: &Path) {
        self.suppressions.lock().unwrap().insert(path.to_path_buf(), Instant::now() + Duration::from_millis(800));
    }

    pub fn add(&mut self, project_id: &str, root: &Path) -> Result<(), String> { /* watch Recursive，维护 roots */ }
    pub fn remove(&mut self, project_id: &str) { /* unwatch，移除 roots */ }
    pub fn project_ids(&self) -> Vec<String> { self.roots.lock().unwrap().values().cloned().collect() }
}

fn emit_batch(app: &AppHandle, roots: &RootMap, suppressions: &SuppressionMap, seen: &SeenSet, events: Vec<DebouncedEvent>) {
    let now = Instant::now();
    { let mut supp = suppressions.lock().unwrap(); supp.retain(|_, expiry| *expiry > now); }
    let snapshot: Vec<(PathBuf, String)> = { let map = roots.lock().unwrap(); map.iter().map(|(r,p)|(r.clone(),p.clone())).collect() };
    let mut batched: HashMap<String, Vec<FsChange>> = HashMap::new();
    for ev in events {
        let path = ev.path;
        { let supp = suppressions.lock().unwrap(); if supp.contains_key(&path) { continue; } }
        for (root, project_id) in &snapshot {
            if !path.starts_with(root) { continue; }
            let rel = path.strip_prefix(root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            if rel.is_empty() || is_ignored(&rel) { break; }
            let meta = std::fs::metadata(&path);
            let exists = meta.is_ok();
            let is_dir = meta.map(|m| m.is_dir()).unwrap_or(false);
            let kind = {
                let mut seen = seen.lock().unwrap();
                if !exists { seen.remove(&path); "remove" }
                else if seen.insert(path.clone()) { "create" }
                else { "modify" }
            };
            batched.entry(project_id.clone()).or_default().push(FsChange {
                project_id: project_id.clone(), rel, kind: kind.to_string(), is_dir,
            });
            break;
        }
    }
    for (project_id, changes) in batched {
        let label = format!("ide-{project_id}");
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.emit("ide://fs-changes", &changes);
        }
    }
}
```

> **关键事实**：
> - `suppress_path` 是 **`IdeWatcher` 实例方法**，不是模块级函数。`IdeWatcher` 存于 `AppState.inner.ide_watcher: Option<IdeWatcher>`（见 `src/state.rs`）。
> - fs 命令目前**只锁 `inner` 拿 projects**，未使用 watcher；若要在 `ide_move` 等里调 `suppress_path`，需在同一把锁内取 `inner.ide_watcher`。
> - watcher 视角**没有"移动"语义**：移动 = 旧路径 `remove` + 新路径 `create`。
> - `ide_move` 内部直接 `std::fs::rename`，**未调 suppress** → watcher 会推送 → 前端再刷新一次（与前端 `onMove` 的 `tree.refresh()` 叠加成双刷）。

### 2.3 `src/state.rs`（相关字段）

```rust
pub struct AppState { pub inner: tokio::sync::Mutex<Inner>, /* ... */ }
pub struct Inner {
    pub projects: /* ... */,
    pub ide_watcher: Option<crate::ide::watch::IdeWatcher>,
    // ...
}
```

### 2.4 `src/ide/path.rs`（接口）

```rust
pub fn resolve(root: &Path, rel: &str, restrict: bool, must_exist: bool) -> Result<PathBuf, String>;
pub fn to_rel(root: &Path, abs: &Path) -> String;   // 转正斜杠相对路径
```


## 3. 前端源码

### 3.1 `ui/ide/api.js`（fs 相关）

```js
const invoke = window.__TAURI__.core.invoke;
export function listDir(projectId, relPath) { return invoke("ide_list_dir", { projectId, relPath: relPath || "" }); }
export function readFile(projectId, relPath) { return invoke("ide_read_file", { projectId, relPath }); }
export function writeFile(projectId, relPath, content) { return invoke("ide_write_file", { projectId, relPath, content }); }
export function deletePath(projectId, relPath) { return invoke("ide_delete", { projectId, relPath }); }
export function restorePath(projectId, trashRel, destRel) { return invoke("ide_restore", { projectId, trashRel, destRel: destRel || null }); }
export function purgeTrash(projectId) { return invoke("ide_purge_trash", { projectId }); }
export function copyPath(projectId, relPath, destDir) { return invoke("ide_copy", { projectId, relPath, destDir }); }
export function movePath(projectId, relPath, destDir, newName) { return invoke("ide_move", { projectId, relPath, destDir, newName: newName || null }); }
export function createPath(projectId, parentDir, name, isDir) { return invoke("ide_create", { projectId, parentDir, name, isDir }); }
export function readBinary(projectId, relPath) { return invoke("ide_read_binary", { projectId, relPath }); }
export function openExternal(projectId, relPath) { return invoke("ide_open_external", { projectId, relPath }); }
// 另有 git*/term*/getWorkdir/setWorkdir/getRestrict/setRestrict 等，与本任务无关
```

### 3.2 `ui/ide/tree.js`（现有实现要点，请以此为基础增量改造）

```js
// 左侧文件树：懒加载目录、展开/折叠、点击文件回调、搜索、上一层。
import { listDir } from "./api.js";

// SVG 常量：SVG_FOLDER / SVG_FOLDER_OPEN / SVG_FILE / SVG_FILE_CODE / SVG_FILE_MARKDOWN / SVG_FILE_JSON / SVG_FILE_IMAGE
const COLOR_FOLDER = "#7fa8ff"; const COLOR_CODE = "#4ec9b0"; const COLOR_MARKDOWN = "#9aa4b2";
const COLOR_JSON = "#e2c08d"; const COLOR_IMAGE = "#c586c0"; const COLOR_PLAIN = "#8a94a6";
function iconFor(entry, expanded) {
  if (entry.is_dir) return { svg: expanded ? SVG_FOLDER_OPEN : SVG_FOLDER, color: COLOR_FOLDER };
  const n = entry.name.toLowerCase();
  if (/\.(js|mjs|cjs|jsx|ts|tsx|rs|go|py|java|c|h|cpp|cc|hpp|cs|rb|php|sh|bash|zsh|lua|swift|kt)$/.test(n)) return { svg: SVG_FILE_CODE, color: COLOR_CODE };
  if (/\.(md|markdown)$/.test(n)) return { svg: SVG_FILE_MARKDOWN, color: COLOR_MARKDOWN };
  if (/\.(json|jsonc|json5)$/.test(n)) return { svg: SVG_FILE_JSON, color: COLOR_JSON };
  if (/\.(png|jpg|jpeg|gif|svg|webp|bmp|ico)$/.test(n)) return { svg: SVG_FILE_IMAGE, color: COLOR_IMAGE };
  return { svg: SVG_FILE, color: COLOR_PLAIN };
}
function normRel(rel) { /* 折叠 .. 和 . */ }
export function fileIcon(name) { return iconFor({ name, is_dir: false }, false); }

export function createTree(host, projectId, onOpenFile, restrict, opts = {}) {
  const state = new Map();   // rel -> { expanded }
  let rootRel = "";
  const activePath = () => host.dataset.active || "";
  const onContextMenu = opts.onContextMenu || null;
  const selection = new Set();
  // applySelection / selectSingle / toggleSelect / clearSelection / getSelection

  function makeRow(entry, depth, container) {
    // 创建 .tree-row，dataset.rel/isDir，_entry
    // contextmenu：若未选中则单选，再 onContextMenu(entry, x, y)
    // draggable=true：
    //   dragstart：setData text/plain=rel；text/uri-list=abs/rel；host.dataset.dragSrc/dragIsDir
    //   dragend：清理 drop-* 与 host.dataset
    //   dropZone：上25%=before，下25%=after，中50%=middle
    //   dragover：middle 时目录=drop-target、文件=none；before/after=drop-before/after
    //   drop：middle 且目录 → opts.onMove(src, entry.rel)；
    //         before/after：srcDir!==destDir → opts.onMove(src, destDir)，否则 reorderRow(仅视图排序，不落盘)
    // twisty / icon / label 组装
    // 目录：childBox(.tree-children.hidden)，click 切换展开，首次 renderDir 懒加载
    // 文件：click 设 active 并 onOpenFile(rel, name)
    // agent-diff-dot：按 window.__hasUnreadAgentDiff[Under] 加角标
  }

  async function renderDir(container, relPath, depth) {
    let entries; try { entries = await listDir(projectId, relPath); } catch (e) { /* .tree-error */ return; }
    for (const entry of entries) makeRow(entry, depth, container);
  }

  // ★ 全量刷新：innerHTML="" + state.clear()，再串行 row.click()+轮询重建展开
  let refreshToken = 0;
  async function refresh() {
    const token = ++refreshToken;
    const expanded = new Set([...state.entries()].filter(([,v]) => v && v.expanded).map(([k]) => k));
    const scrollTop = host.scrollTop;
    host.innerHTML = "";
    state.clear();
    if (opts.onRootChange) opts.onRootChange(rootRel);
    await renderDir(host, rootRel, 0);
    if (token !== refreshToken) return;
    const rels = [...expanded].sort((a,b) => a.split("/").length - b.split("/").length);
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
  async function waitDirLoaded(rel) { /* 轮询 50×10ms 等 childBox.dataset.loaded */ }
  function parentDepth(rel) { /* 相对 rootRel 深度 */ }

  // 增量：仅按「变更文件的父目录」重建该层，但仍 box.innerHTML=""
  async function applyChanges(changes) {
    const byParent = new Map();
    for (const c of changes) { const parent = c.rel.includes("/") ? c.rel.slice(0, c.rel.lastIndexOf("/")) : ""; /* 分组 */ }
    for (const [parent] of byParent) {
      // 找 parent 容器 box（根=host；否则 parentRow 的未 hidden childBox）
      // listDir(parent) → box.innerHTML="" → makeRow → 恢复该层已展开子目录
    }
    applyDirty();
    if (window.__refreshAgentDiffDots) window.__refreshAgentDiffDots();
  }

  function reorderRow(srcRel, targetRow, zone) { /* insertBefore 移动 DOM，仅视图排序，不落盘 */ }
  function goUp() { /* restrict 判断；rootRel = normRel(rootRel + "/..")；refresh() */ }
  async function search(query) { /* 递归扫描 深度≤8 数量≤200，生成 .search-hit 行 */ }
  async function setRoot(rel) { rootRel = normRel(rel || ""); await refresh(); return rootRel; }

  // 内联重命名：label 换 input，回车提交；回调 opts.onRename(rel, dir, newName) 返回 bool
  function startRename(rel) {
    // input.className="rename-input"; 选中主名（跳过扩展名）
    // commit：newName===oldName 则还原；否则 await opts.onRename(rel, dir, newName)，返回 false 则还原
  }

  // 脏标记：接收脏文件 rel 集合，向祖先目录冒泡加 .dirty
  let dirtyRelSet = new Set();
  function applyDirty() { /* 计算所有祖先目录 dirs，.tree-row 加/去 .dirty；调 window.__refreshGitDecorations */ }
  function setDirty(relSet) { dirtyRelSet = new Set(relSet); applyDirty(); }

  // 逐级展开到 rel 并选中（用于"跳转到文件"）
  async function reveal(rel) { /* 逐级 row.click()+waitDirLoaded；selectSingle；scrollIntoView */ }

  return {
    refresh, goUp, search, setRoot, getRoot: () => rootRel,
    getSelection, clearSelection, startRename, selectSingle, setDirty,
    applyChanges, reveal,
  };
}
```


### 3.3 `ui/ide/ide.js`（资源管理器相关，节选）

```js
import { createTree, fileIcon } from "./tree.js";
let tree = null;
const openDocs = new Map(); // rel -> { model, dirty, savedValue, kind, ... }  ★ 变量名是 openDocs，不是 openTabs
let activeRel = null;

function setGlobalDirty() {
  const dirtyRels = [];
  for (const [rel, doc] of openDocs) if (doc.dirty) dirtyRels.push(rel);
  els.dirtyFlag.classList.toggle("hidden", dirtyRels.length === 0);
  if (tree) tree.setDirty(dirtyRels);   // ★ 调用 tree.setDirty
}

function closeTabsUnder(rel) {
  const prefix = rel + "/";
  for (const key of [...openDocs.keys()]) if (key === rel || key.startsWith(prefix)) closeTab(key);
}

// —— 创建树（boot 内）——
const tree = createTree(document.getElementById("file-tree"), projectId, openFile, restrict, {
  onRootChange: (rel) => { els.sidebarPath.textContent = "/" + (rel || ""); },
  onContextMenu: (entry, x, y) => { /* 弹菜单：打开/新建/重命名/删除/复制/剪切/粘贴/在文件管理器显示/设为项目文件夹... */ },
  // 注意：现有 createTree 的 opts 里【没有 onRename / onDelete】；
  //       实际 ide.js 通过 menuItem("重命名", () => tree.startRename(entry.rel)) 调用，未传回调。
  onMove: async (src, destDir) => {
    try {
      const rel = await movePath(projectId, src, destDir);
      els.statusPath.textContent = "已移动：" + rel;
      closeTabsUnder(src);
      await tree.refresh();            // ★ 移动后全量刷新 → 展开丢失主因
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
els.upBtn.onclick = () => goUpAndSync();  // 内部用 tree.getRoot()/tree.setRoot()

// 新建文件/文件夹（模态框输入名，创建后 tree.refresh()）
async function newEntry(parentDir, isDir) { /* showModal → createPath → await tree.refresh() → openFile/pushHistory */ }

// 删除（批量）：deletePath → closeTabsUnder → tree.clearSelection() → await tree.refresh()
async function removeEntry(entry) { /* ... await tree.refresh() ... */ }

// 粘贴：cut=movePath（后 tree.refresh()）；copy=copyPath（后 tree.refresh()）
async function pasteInto(destDir) { /* ... await tree.refresh() ... */ }

// 右键"重命名"：tree.startRename(entry.rel)（无回调）

// —— 文件监听 ——
function installFsWatch() {
  const tauriEvent = window.__TAURI__ && window.__TAURI__.event;
  if (!tauriEvent) return;
  let timer = null, flushing = false;
  const pendingChanges = new Map(), pendingDocs = new Map();
  tauriEvent.listen("ide://fs-changes", (ev) => {
    const list = Array.isArray(ev.payload) ? ev.payload : [ev.payload || {}];
    for (const p of list) {
      if (p.project_id && p.project_id !== projectId) continue;
      if (p.kind === "create" || p.kind === "remove") pendingChanges.set(p.rel, { rel: p.rel, kind: p.kind, is_dir: !!p.is_dir });
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
      const changes = [...pendingChanges.values()]; pendingChanges.clear();
      const docsToSync = new Map(pendingDocs); pendingDocs.clear();
      if (changes.length && tree && tree.applyChanges) { try { await tree.applyChanges(changes); } catch {} }
      else if (changes.length) { try { await tree.refresh(); } catch {} }
      // 再同步已打开文档内容（readFile → model.setValue，保留本地未保存修改）
      for (const [rel, kind] of docsToSync) { /* ... */ }
      if ((changes.length || docsToSync.size) && gitState && gitState.is_repo) refreshGit().catch(() => {});
    } finally {
      flushing = false;
      if ((pendingChanges.size || pendingDocs.size) && !timer) timer = setTimeout(flushFsChanges, 250);
    }
  }
}
```

> `ide.js` 中所有"操作后刷新"处（新建/删除/粘贴/重命名/移动、git checkout/reset/pull 等）目前统一是 `await tree.refresh()`（全量）。`reveal(rel)` 已在 `openFile` 相关流程里被调用（`if (tree && tree.reveal) await tree.reveal(rel);`）。

### 3.4 `ui/ide/index.html`（资源管理器骨架）

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

### 3.5 `ui/ide/ide.css`（树相关）

```css
:root { --tree-hover: rgba(255,255,255,0.05); --tree-active: rgba(91,140,255,0.18); }
.file-tree { flex: 1; overflow: auto; padding: 4px 0 12px; }
.tree-row { /* flex 行、gap、padding、cursor、white-space 等 */ }
.tree-row:hover { background: var(--tree-hover); }
.tree-row.active { background: var(--tree-active); }
.tree-row.selected { background: var(--tree-active); }
.tree-twisty { /* 旋转箭头容器 */ }
.tree-twisty.empty { visibility: hidden; }
.tree-twisty.open { transform: rotate(90deg); }
.tree-twisty svg { display: block; }
.tree-icon { /* 图标列 */ }
.tree-icon svg { display: block; }
.tree-label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.tree-children.hidden { display: none; }
.tree-error { padding: 6px 14px; color: #ff6b6b; font-size: 12px; }
.tree-row.drop-target { /* 拖入目录高亮 */ }
.tree-row.drop-before { box-shadow: inset 0 2px 0 var(--accent); }
.tree-row.drop-after { box-shadow: inset 0 -2px 0 var(--accent); }
.tree-row[draggable="true"] { cursor: grab; }
.tree-row.dirty .tree-label { color: #e2b341; }
.tree-row.dirty::after { /* 脏点 */ }
.search-hit .tree-label { color: var(--fg-dim); font-size: 12px; }
.agent-diff-dot { /* 未读 Agent 改动角标 */ }
/* 另有 .tree-row[data-git-code="M|A|D|R|U|C"] .git-badge 着色 */
```

## 4. 现有对外 API 契约（**必须保留**）

`createTree(host, projectId, onOpenFile, restrict, opts)` 返回对象，**被 `ide.js` 实际调用的方法**：

| 方法 | 调用处 | 说明 |
|---|---|---|
| `refresh()` | 多处 | 全量刷新 |
| `applyChanges(changes)` | `flushFsChanges` | `changes: [{rel, kind:"create"|"remove", is_dir}]` |
| `setDirty(relSet)` | `setGlobalDirty` | 脏文件集合，向祖先冒泡 |
| `getRoot()` | goUpAndSync/右键根 | 返回当前 `rootRel` |
| `setRoot(rel)` | goUpAndSync/设为项目文件夹 | 设根并刷新 |
| `getSelection()` | 右键菜单 | 返回 `[{rel, isDir}]` |
| `clearSelection()` | 复制/剪切后 | |
| `selectSingle(rel)` | 内部/外部 | |
| `reveal(rel)` | openFile 后 | 逐级展开并选中定位 |
| `startRename(rel)` | 右键"重命名" | 内联重命名 |
| `search(query)` | 搜索框 | 递归搜索 |
| `goUp()` | 上一层 | |
| `fileIcon(name)`（具名导出） | SCM 面板 | 按名取图标 |

`opts` 实际传入：`onRootChange(rel)`、`onContextMenu(entry, x, y)`、`onMove(src, destDir)`。

**宿主全局钩子**（`tree.js` 内部会调用，必须继续支持）：
- `window.__hasUnreadAgentDiff(rel)` / `window.__hasUnreadAgentDiffUnder(dirRel)` → 角标
- `window.__refreshAgentDiffDots()` → 刷新角标
- `window.__refreshGitDecorations()` → 由 `applyDirty` 调用

**宿主变量名**：打开文档集合是 **`openDocs`**（不是 `openTabs`）。

## 5. 已核对的差异点（外部方案不能照抄）

外部方案（`explorer_full_optimization_plan.md`）与现状不符之处：

1. **`suppress_path` 不是模块级函数**，是 `IdeWatcher` 实例方法；fs 命令当前无 watcher 句柄，需在同一把 `inner` 锁内取 `inner.ide_watcher`。
2. **`ide_list_dir` 已排序**（目录优先 + 小写名比较）；仅"自然序 file2<file10"未实现。
3. **`rebaseOpenTabs` 里的 `openTabs` 不存在**，实际是 `openDocs`；照抄会静默失效。
4. **丢失/改名现有 API**：方案丢了 `setDirty`/`getRoot`/`reveal`（改名 revealPath）/`search` 语义；`applyDirty` 被改成读 `window.__dirtyFiles`（错，现有用内部 `dirtyRelSet` + 冒泡 + `__refreshGitDecorations`）。
5. **`startRename` 回调签名**：现有 `opts.onRename(rel, dir, newName)->bool`；方案是 `opts.onRename(oldRel, newName)`。
6. **`opts` 无 `onDelete`**；方案键盘 Delete 分支引用 `opts.onDelete`，需新增或改走宿主。
7. **agent-diff-dot 角标逻辑**（`__hasUnreadAgentDiff*`）在方案中被丢失。
8. **search 复用 `.tree-node`/`nodeMap` 并 `nodeMap.clear()`** 会与正常树状态串扰；现有用独立 `.search-hit`。
9. **多选拖拽**：方案 for 循环多次调 `opts.onMove` → 会多次 `pushHistory`。
10. 拖拽 **before/after 排序不落盘**（现有即如此）；方案建议"废除假排序"。

## 6. 外部 AI 必须遵守的约束

1. **不整文件替换**。基于现有 `tree.js` 做增量改造，**保留第 4 节全部导出方法与签名**（尤其 `setDirty`/`getRoot`/`reveal`/`startRename` 现有签名）。
2. **前端不新增依赖**（无 React/Vue/vdom 库），除非明确论证并由人工决定。
3. **变量名对齐**：打开文档集合 `openDocs`；文档对象含 `kind`（"text"/"preview"）、`model`、`dirty`、`savedValue`。
4. **后端改动**需说明具体锁与句柄获取方式（`AppState.inner.ide_watcher`），不得虚构模块级函数。
5. **脏标记语义**：保留"向祖先目录冒泡 + `window.__refreshGitDecorations`"，不要换成 `window.__dirtyFiles`。
6. **agent-diff 角标**：保留 `window.__hasUnreadAgentDiff`/`__hasUnreadAgentDiffUnder`/`__refreshAgentDiffDots` 调用点。
7. **路径语义**：rel 为正斜杠相对工作目录；`rootRel` 由 `goUp`/`setRoot` 维护，可含 `..`（现有 `goUp` 用 `normRel(rootRel + "/..")`）。改造须与现有 `activePath`（`host.dataset.active`）一致。
8. 输出应给出：**改动的具体文件 + 精确 diff 或替换片段 + 新增/修改的导出签名 + 后端改动 + 需人工确认点**。

## 7. 请外部 AI 回答

1. 在「原生 JS、无框架」约束下，如何把 `tree.js` 改为**状态驱动 + Keyed DOM 原地 diff**，实现拖拽移动后**不重建、不丢失展开/滚动/选中**，且**保留第 4 节全部 API 契约**？
2. 如何消除「前端 `onMove` 全量 refresh」与「后端 watcher 推送 `create/remove` 再刷」的**双刷**？给出前端幂等守卫 + 后端 `suppress_path` 接入的具体代码（含锁/句柄）。
3. 如何正确处理**移动目录**时保留其展开子树？
4. `reveal`/`setDirty`/`applyDirty`/agent-diff 角标 在新内核下如何保持兼容？
5. 是否应引入「前端持有树模型 + 后端只给 dir 列表」的统一 diff？给出最小改动落地方案。
6. 拖拽 before/after 视图排序不落盘，是否应保留/禁止？多选拖拽如何组织历史（避免多次 pushHistory）？
