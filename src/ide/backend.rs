//! 远程文件操作后端（POSIX 远程）。
//!
//! 阶段 2：命令入口按项目类型分派——本地项目走原 `fs.rs` 逻辑（零改动），
//! 远程项目走本模块（经 `ssh::exec` 在远端执行 POSIX 命令）。
//! 目标远端为 Linux/macOS；Windows 远端文件浏览暂不支持（返回明确错误）。
//!
//! 安全：远端路径拒绝 NUL / `..` 逃逸；restrict 时前缀必须落在 root 内。

use std::time::Duration;

use crate::ide::fs::DirEntry;
use crate::ide::ssh;
use crate::projects::RemoteSpec;

/// 读取文本文件的大小上限（与本地 `fs.rs` 保持一致）。
pub const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// 列目录/读写的默认超时。
const OP_TIMEOUT: Duration = Duration::from_secs(30);

/// 取项目的远程规格（None=本地）。
pub async fn remote_spec(
    state: &tauri::State<'_, crate::state::AppState>,
    project_id: &str,
) -> Option<RemoteSpec> {
    let inner = state.inner.lock().await;
    inner.projects.remote_of(project_id)
}

/// POSIX 规范化：合并 `.`/`..`，去掉多余 `/`，返回以 `/` 开头的绝对形式。
pub fn normalize_posix(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

/// 把远程 root 与相对路径拼成规范化绝对路径，并按 restrict 校验越界。
pub fn posix_resolve(root: &str, rel: &str, restrict: bool) -> Result<String, String> {
    if rel.contains('\0') {
        return Err("路径含非法字符".into());
    }
    let root_norm = normalize_posix(root);
    let rel_trim = rel.trim_start_matches('/');
    let full = normalize_posix(&format!("{root_norm}/{rel_trim}"));
    if restrict {
        if full != root_norm && !full.starts_with(&format!("{root_norm}/")) {
            return Err(format!("路径越界：{rel}"));
        }
    }
    Ok(full)
}

/// 单引号 shell 转义（防注入）。
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Windows 路径规范化：统一反斜杠为 `/`，合并 `.`/`..`，保留盘符或 UNC 前缀。
pub fn normalize_win(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let is_unc = unified.starts_with("//");
    let mut out: Vec<&str> = Vec::new();
    for seg in unified.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if is_unc {
        format!("//{joined}")
    } else {
        joined
    }
}

/// 把 Windows 远程 root 与相对路径拼成规范化绝对路径，并按 restrict 校验越界（大小写不敏感）。
pub fn win_resolve(root: &str, rel: &str, restrict: bool) -> Result<String, String> {
    if rel.contains('\0') {
        return Err("路径含非法字符".into());
    }
    let root_norm = normalize_win(root);
    let rel_clean = rel.replace('\\', "/");
    let rel_trim = rel_clean.trim_start_matches('/');
    let full = normalize_win(&format!("{root_norm}/{rel_trim}"));
    if restrict {
        let f = full.to_lowercase();
        let r = root_norm.to_lowercase();
        if f != r && !f.starts_with(&format!("{r}/")) {
            return Err(format!("路径越界：{rel}"));
        }
    }
    Ok(full)
}

/// PowerShell 单引号字面量转义（用于 `-LiteralPath` / 字符串）。
pub fn ps_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 简单判定是否为 Windows 风格路径（盘符如 `C:` 或含反斜杠）。
pub fn is_windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 2 && b[1] == b':') || p.contains('\\')
}

/// 把远程绝对路径转成相对 `spec.root` 的 rel（与 win_resolve/posix_resolve 同规则）。
/// 越界或不在 root 下返回 Err。rel 为空表示 root 本身。
pub fn to_rel(spec: &RemoteSpec, abs: &str) -> Result<String, String> {
    if is_windows_path(abs) {
        let root = normalize_win(&spec.root);
        let target = normalize_win(abs);
        let r = root.to_lowercase();
        let t = target.to_lowercase();
        if t == r {
            return Ok(String::new());
        }
        let prefix = format!("{r}/");
        if !t.starts_with(&prefix) {
            return Err(format!("路径不在项目根目录下：{abs}"));
        }
        Ok(target[r.len() + 1..].to_string())
    } else {
        let root = normalize_posix(&spec.root);
        let target = normalize_posix(abs);
        if target == root {
            return Ok(String::new());
        }
        let prefix = format!("{root}/");
        if !target.starts_with(&prefix) {
            return Err(format!("路径不在项目根目录下：{abs}"));
        }
        Ok(target[prefix.len()..].to_string())
    }
}

/// 列出一个远程目录（不递归）。`rel` 为空表示 root。
pub async fn list_dir(spec: &RemoteSpec, rel: &str) -> Result<Vec<DirEntry>, String> {
    if is_windows_remote(spec).await {
        return super::backend_win::list_dir(spec, rel).await;
    }
    let abs = posix_resolve(&spec.root, rel, spec.restrict)?;
    // %y=类型(d/f/l) %s=大小 %f=文件名；LC_ALL=C 稳定排序与数字。
    let cmd = format!(
        "LC_ALL=C find {} -mindepth 1 -maxdepth 1 -printf '%y\\t%s\\t%f\\n' 2>/dev/null",
        shell_quote(&abs)
    );
    let out = ssh::exec_timeout(spec, &cmd, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程列目录失败：{}", out.stderr.trim()));
    }

    let base = rel.trim_matches('/');
    let mut entries = Vec::new();
    for line in out.stdout.lines() {
        let mut it = line.splitn(3, '\t');
        let ty = it.next().unwrap_or("");
        let size = it.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let name = match it.next() {
            Some(n) => n,
            None => continue,
        };
        if name.is_empty() {
            continue;
        }
        let is_dir = ty == "d";
        let is_symlink = ty == "l";
        let rel_path = if base.is_empty() {
            name.to_string()
        } else {
            format!("{base}/{name}")
        };
        entries.push(DirEntry {
            rel: rel_path,
            name: name.to_string(),
            is_dir,
            size: if is_dir { 0 } else { size },
            is_symlink,
        });
    }
    // 目录优先，名称自然序（借用本地排序逻辑：不区分大小写）。
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// 读取远程文本文件。
pub async fn read_file(spec: &RemoteSpec, rel: &str) -> Result<String, String> {
    if is_windows_remote(spec).await {
        return super::backend_win::read_text(spec, rel).await;
    }
    let abs = posix_resolve(&spec.root, rel, spec.restrict)?;

    // 大小检查
    let sz_cmd = format!("wc -c < {} 2>/dev/null", shell_quote(&abs));
    let sz_out = ssh::exec_timeout(spec, &sz_cmd, OP_TIMEOUT).await?;
    if !sz_out.ok() {
        return Err(format!("远程读取失败：{}", sz_out.stderr.trim()));
    }
    let size: u64 = sz_out.stdout.trim().parse().unwrap_or(0);
    if size > MAX_READ_BYTES {
        return Err(format!(
            "文件过大（{size} 字节，上限 {MAX_READ_BYTES} 字节），IDE 不加载"
        ));
    }

    let cat_cmd = format!("cat -- {} 2>/dev/null", shell_quote(&abs));
    let out = ssh::exec_timeout(spec, &cat_cmd, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程读取失败：{}", out.stderr.trim()));
    }
    if out.stdout.as_bytes().iter().take(4096).any(|&b| b == 0) {
        return Err("二进制文件，无法以文本打开".to_string());
    }
    Ok(out.stdout)
}

/// 远程二进制读取上限（与本地 preview 一致）。
pub const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

/// 读取远程文件为 base64（图片/PDF 预览）。
pub async fn read_binary(spec: &RemoteSpec, rel: &str) -> Result<(Vec<u8>, u64), String> {
    if is_windows_remote(spec).await {
        return super::backend_win::read_binary(spec, rel).await;
    }
    let abs = posix_resolve(&spec.root, rel, spec.restrict)?;
    let sz_cmd = format!("wc -c < {} 2>/dev/null", shell_quote(&abs));
    let sz_out = ssh::exec_timeout(spec, &sz_cmd, OP_TIMEOUT).await?;
    if !sz_out.ok() {
        return Err(format!("远程读取失败：{}", sz_out.stderr.trim()));
    }
    let size: u64 = sz_out.stdout.trim().parse().unwrap_or(0);
    if size > MAX_BINARY_BYTES {
        return Err(format!(
            "文件过大（{size} 字节，上限 {MAX_BINARY_BYTES} 字节），IDE 不预览"
        ));
    }
    let cmd = format!("base64 -w0 -- {} 2>/dev/null", shell_quote(&abs));
    let out = ssh::exec_timeout(spec, &cmd, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程读取失败：{}", out.stderr.trim()));
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(out.stdout.trim().as_bytes())
        .map_err(|e| format!("base64 解码失败: {e}"))?;
    Ok((bytes, size))
}

/// 写入远程文本文件（临时文件 + 原子 rename；父目录自动创建）。
pub async fn write_file(spec: &RemoteSpec, rel: &str, content: &str) -> Result<(), String> {
    if is_windows_remote(spec).await {
        return super::backend_win::write_file(spec, rel, content).await;
    }
    let abs = posix_resolve(&spec.root, rel, spec.restrict)?;
    use base64::Engine;
    // .ps1/.psm1/.psd1 加 UTF-8 BOM：与 Windows 路径一致，万一被 PowerShell 5.1 执行也能识别。
    let lower = rel.to_lowercase();
    let need_bom = lower.ends_with(".ps1") || lower.ends_with(".psm1") || lower.ends_with(".psd1");
    let bytes: Vec<u8> = if need_bom {
        let mut v = vec![0xEF, 0xBB, 0xBF];
        v.extend_from_slice(content.as_bytes());
        v
    } else {
        content.as_bytes().to_vec()
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let parent = match abs.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => abs[..i].to_string(),
    };
    let tmp = format!("{abs}.ligong-tmp");
    let cmd = format!(
        "mkdir -p {parent} && printf %s {b64} | base64 -d > {tmp} && mv -f {tmp} {abs}",
        parent = shell_quote(&parent),
        b64 = shell_quote(&b64),
        tmp = shell_quote(&tmp),
        abs = shell_quote(&abs),
    );
    let out = ssh::exec_timeout(spec, &cmd, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程保存失败：{}", out.stderr.trim()));
    }
    Ok(())
}

/// 远程新建文件或目录。
pub async fn create(spec: &RemoteSpec, rel: &str, is_dir: bool) -> Result<(), String> {
    if is_windows_remote(spec).await {
        return super::backend_win::create(spec, rel, is_dir).await;
    }
    let abs = posix_resolve(&spec.root, rel, spec.restrict)?;
    let cmd = if is_dir {
        format!("mkdir -p {}", shell_quote(&abs))
    } else {
        format!("mkdir -p \"$(dirname {})\" && : > {}", shell_quote(&abs), shell_quote(&abs))
    };
    let out = ssh::exec_timeout(spec, &cmd, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程新建失败：{}", out.stderr.trim()));
    }
    Ok(())
}

/// 远程删除（永久删除；回收站后置）。拒绝删除 root 本身。
pub async fn delete(spec: &RemoteSpec, rel: &str) -> Result<(), String> {
    if is_windows_remote(spec).await {
        return super::backend_win::delete(spec, rel).await;
    }
    let abs = posix_resolve(&spec.root, rel, spec.restrict)?;
    let root_norm = normalize_posix(&spec.root);
    if abs == root_norm {
        return Err("不能删除工作目录根".to_string());
    }
    let cmd = format!("rm -rf -- {}", shell_quote(&abs));
    let out = ssh::exec_timeout(spec, &cmd, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程删除失败：{}", out.stderr.trim()));
    }
    Ok(())
}

/// 远程改名/移动。`dest_rel` 为目标绝对路径（相对 root 已解析）。
pub async fn rename(spec: &RemoteSpec, from_rel: &str, to_rel: &str) -> Result<(), String> {
    if is_windows_remote(spec).await {
        return super::backend_win::rename(spec, from_rel, to_rel).await;
    }
    let from = posix_resolve(&spec.root, from_rel, spec.restrict)?;
    let to = posix_resolve(&spec.root, to_rel, spec.restrict)?;
    let parent = match to.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => to[..i].to_string(),
    };
    let cmd = format!(
        "mkdir -p {} && mv -f {} {}",
        shell_quote(&parent),
        shell_quote(&from),
        shell_quote(&to),
    );
    let out = ssh::exec_timeout(spec, &cmd, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程移动失败：{}", out.stderr.trim()));
    }
    Ok(())
}
/// 探测远端是否为 Windows（uname 失败或非 unix 视为 Windows）。
pub async fn is_windows_remote(spec: &RemoteSpec) -> bool {
    match ssh::exec_timeout(spec, "uname -s", Duration::from_secs(15)).await {
        Ok(o) => {
            let s = o.stdout.to_lowercase();
            !(s.contains("linux") || s.contains("darwin") || s.contains("bsd"))
        }
        Err(_) => true,
    }
}