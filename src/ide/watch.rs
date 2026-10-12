//! IDE 文件系统监听：外部改动时通知对应 IDE 窗口刷新。
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, DebouncedEvent};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, serde::Serialize)]
pub struct FsChange {
    pub project_id: String,
    pub rel: String,
    pub kind: String,
    pub is_dir: bool,
}

type RootMap = Arc<Mutex<HashMap<PathBuf, String>>>;
type SuppressionMap = Arc<Mutex<HashMap<PathBuf, Instant>>>;
type SeenSet = Arc<Mutex<HashSet<PathBuf>>>;

pub struct IdeWatcher {
    watcher: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    roots: RootMap,
    suppressions: SuppressionMap,
}

/// 高频/无关目录，命中任一目录段即忽略，避免事件暴洪。
fn is_ignored(rel: &str) -> bool {
    rel.split('/').any(|seg| {
        if matches!(
            seg,
            ".git" | ".svn" | ".hg" | ".ligong-ide-trash" | "node_modules"
                | "dist" | "build" | "out" | "bin" | "pkg"
                | "vendor" | "Pods" | "site-packages" | "__pycache__"
                | ".playwright-mcp" | ".turbo" | ".next" | ".venv" | ".cache"
        ) {
            return true;
        }
        seg.starts_with("target")
            || seg.starts_with(".fingerprint")
            || seg.ends_with(".tmp")
            || seg.contains(".tmp_")
            || seg.ends_with('~')
    })
}

impl IdeWatcher {
    pub fn new(app: AppHandle) -> Result<Self, String> {
        let roots: RootMap = Arc::new(Mutex::new(HashMap::new()));
        let suppressions: SuppressionMap = Arc::new(Mutex::new(HashMap::new()));
        let seen: SeenSet = Arc::new(Mutex::new(HashSet::new()));
        let roots_cb = roots.clone();
        let supp_cb = suppressions.clone();
        let seen_cb = seen.clone();

        let debouncer = new_debouncer(
            Duration::from_millis(180),
            move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                emit_batch(&app, &roots_cb, &supp_cb, &seen_cb, events);
            },
        )
        .map_err(|e| format!("创建文件监听失败: {e}"))?;

        Ok(Self {
            watcher: debouncer,
            roots,
            suppressions,
        })
    }

    /// IDE 内部写盘前调用：抑制该路径未来 800ms 的事件。
    pub fn suppress_path(&self, path: &Path) {
        self.suppressions
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), Instant::now() + Duration::from_millis(800));
    }

    pub fn add(&mut self, project_id: &str, root: &Path) -> Result<(), String> {
        {
            let map = self.roots.lock().unwrap();
            if map.get(root).map(|p| p == project_id).unwrap_or(false) {
                return Ok(());
            }
        }

        let stale: Vec<PathBuf> = {
            let map = self.roots.lock().unwrap();
            map.iter()
                .filter(|(_, p)| p.as_str() == project_id)
                .map(|(r, _)| r.clone())
                .collect()
        };
        for r in stale {
            let _ = self.watcher.watcher().unwatch(&r);
            self.roots.lock().unwrap().remove(&r);
        }

        self.watcher
            .watcher()
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| format!("监听目录失败: {e}"))?;
        self.roots
            .lock()
            .unwrap()
            .insert(root.to_path_buf(), project_id.to_string());
        Ok(())
    }

    pub fn remove(&mut self, project_id: &str) {
        let stale: Vec<PathBuf> = {
            let map = self.roots.lock().unwrap();
            map.iter()
                .filter(|(_, p)| p.as_str() == project_id)
                .map(|(r, _)| r.clone())
                .collect()
        };
        for r in stale {
            let _ = self.watcher.watcher().unwatch(&r);
            self.roots.lock().unwrap().remove(&r);
        }
    }

    pub fn project_ids(&self) -> Vec<String> {
        self.roots.lock().unwrap().values().cloned().collect()
    }
}

fn emit_batch(
    app: &AppHandle,
    roots: &RootMap,
    suppressions: &SuppressionMap,
    seen: &SeenSet,
    events: Vec<DebouncedEvent>,
) {
    let now = Instant::now();
    {
        let mut supp = suppressions.lock().unwrap();
        supp.retain(|_, expiry| *expiry > now);
    }

    let snapshot: Vec<(PathBuf, String)> = {
        let map = roots.lock().unwrap();
        map.iter().map(|(r, p)| (r.clone(), p.clone())).collect()
    };

    let mut batched: HashMap<String, Vec<FsChange>> = HashMap::new();

    for ev in events {
        let path = ev.path;
        {
            let supp = suppressions.lock().unwrap();
            if supp.contains_key(&path) {
                continue;
            }
        }

        for (root, project_id) in &snapshot {
            if !path.starts_with(root) {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel.is_empty() || is_ignored(&rel) {
                break;
            }

            let meta = std::fs::metadata(&path);
            let exists = meta.is_ok();
            let is_dir = meta.map(|m| m.is_dir()).unwrap_or(false);

            let kind = {
                let mut seen = seen.lock().unwrap();
                if !exists {
                    seen.remove(&path);
                    "remove"
                } else if seen.insert(path.clone()) {
                    "create"
                } else {
                    "modify"
                }
            };

            batched
                .entry(project_id.clone())
                .or_default()
                .push(FsChange {
                    project_id: project_id.clone(),
                    rel,
                    kind: kind.to_string(),
                    is_dir,
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
/// Agent 工具改动后的只读 diff 快照，定向发给 ide-<project_id>。
#[derive(Clone, serde::Serialize)]
pub struct AgentDiffPayload {
    pub project_id: String,
    pub rel: String,
    pub old_content: String,
    pub new_content: String,
    pub tool_call_id: String,
    pub timestamp: u64,
}
