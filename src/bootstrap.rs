//! 首次运行时把随包分发的种子账号释放到用户目录。
//!
//! 只在目标文件不存在时复制，绝不覆盖用户已有数据。

use std::path::{Path, PathBuf};

fn copy_dir_if_absent(src: &Path, dst: &Path) {
    if !src.is_dir() {
        return;
    }
    let _ = std::fs::create_dir_all(dst);
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_if_absent(&from, &to);
        } else if !to.exists() {
            let _ = std::fs::copy(&from, &to);
        }
    }
}

fn seed_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LIGONG_SEED") {
        let pb = PathBuf::from(&p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let root = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let cand = root.join("seed");
    if cand.exists() {
        Some(cand)
    } else {
        None
    }
}

pub fn release_seed_accounts() {
    let Some(seed) = seed_root() else {
        return;
    };
    let Some(home) = dirs::home_dir() else {
        return;
    };

    let target_profiles = home.join(".deepseek-agent").join("accounts");
    let seed_profiles = seed.join("accounts");
    if seed_profiles.is_dir() {
        let _ = std::fs::create_dir_all(&target_profiles);
        if let Ok(entries) = std::fs::read_dir(&seed_profiles) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                copy_dir_if_absent(&entry.path(), &target_profiles.join(&name));
            }
        }
    }

    let target_states = home.join(".ligong").join("accounts");
    let seed_states = seed.join("ligong-accounts");
    if seed_states.is_dir() {
        let _ = std::fs::create_dir_all(&target_states);
        if let Ok(entries) = std::fs::read_dir(&seed_states) {
            for entry in entries.flatten() {
                copy_dir_if_absent(&entry.path(), &target_states.join(&entry.file_name()));
            }
        }
    }

    ensure_accounts_registered(&home);
}

fn ensure_accounts_registered(home: &Path) {
    let accounts_dir = home.join(".deepseek-agent").join("accounts");
    let Ok(entries) = std::fs::read_dir(&accounts_dir) else {
        return;
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    if names.is_empty() {
        return;
    }
    let mut mgr = match deepseek_agent::accounts::AccountManager::load_default() {
        Ok(m) => m,
        Err(_) => return,
    };
    for name in names {
        if !mgr.exists(&name) {
            let _ = mgr.add(&name);
        }
    }
}
