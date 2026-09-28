use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// 可执行文件所在目录；打包后即安装根目录。
pub fn install_root() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// 随包分发的运行时目录（node / playwright / chromium / bridge 脚本）。
pub fn runtime_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LIGONG_RUNTIME") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let root = install_root()?;
    for cand in [root.join("runtime"), root.join("../lib/ligong/runtime")] {
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

pub fn node_bin() -> String {
    if let Ok(p) = std::env::var("LIGONG_NODE") {
        if PathBuf::from(&p).exists() {
            return p;
        }
    }
    let names: &[&str] = if cfg!(windows) {
        &["node.exe", "node"]
    } else {
        &["node"]
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(rt) = runtime_dir() {
        dirs.push(rt.clone());
        dirs.push(rt.join("bin"));
    }
    if let Some(root) = install_root() {
        dirs.push(root);
    }
    for dir in dirs {
        for name in names {
            let cand = dir.join(name);
            if cand.exists() {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    "node".to_string()
}

pub fn playwright_dir() -> PathBuf {
    if let Ok(p) = std::env::var("LIGONG_PLAYWRIGHT") {
        return PathBuf::from(p);
    }
    if let Some(rt) = runtime_dir() {
        let cand = rt.join("node_modules").join("playwright");
        if cand.exists() {
            return cand;
        }
    }
    home().join(".ligong-mcp/node_modules/playwright")
}

pub fn chromium_exec() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LIGONG_CHROMIUM") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return Some(pb);
        }
    }
    if let Some(rt) = runtime_dir() {
        if let Some(found) = scan_chromium(&rt.join("browsers")) {
            return Some(found);
        }
    }
    let cache = if cfg!(windows) {
        std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join("AppData/Local"))
            .join("ms-playwright")
    } else {
        home().join(".cache/ms-playwright")
    };
    scan_chromium(&cache)
}

fn scan_chromium(base: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(base).ok()?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("chromium-") {
            continue;
        }
        for c in [
            e.path().join("chrome-linux64/chrome"),
            e.path().join("chrome-linux/chrome"),
            e.path().join("chrome-win64/chrome.exe"),
            e.path().join("chrome-win/chrome.exe"),
        ] {
            if c.exists() {
                return Some(c);
            }
        }
    }
    None
}

pub fn bridge_dir() -> PathBuf {
    if let Ok(p) = std::env::var("LIGONG_BRIDGE_DIR") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return pb;
        }
    }
    if let Some(rt) = runtime_dir() {
        let cand = rt.join("bridge");
        if cand.exists() {
            return cand;
        }
    }
    if let Some(root) = install_root() {
        let cand = root.join("bridge");
        if cand.exists() {
            return cand;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bridge")
}

pub fn bridge_server_script() -> PathBuf {
    bridge_dir().join("bridge_server.mjs")
}

pub fn login_script_path() -> PathBuf {
    bridge_dir().join("browser_login.mjs")
}
