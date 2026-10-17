//! Windows 远端文件操作实现（pwsh 7 + EncodedCommand）。
//!
//! PowerShell 脚本以 UTF-16LE base64 经 `pwsh -EncodedCommand` 传入，避免引号/中文/换行转义问题；
//! 大块内容（写入的文件）走 stdin，避免命令行长度限制。

use std::time::Duration;

use crate::ide::fs::DirEntry;
use crate::ide::ssh;
use crate::projects::RemoteSpec;

use super::backend::{
    is_windows_remote, normalize_win, ps_lit, win_resolve, MAX_BINARY_BYTES, MAX_READ_BYTES,
};

const OP_TIMEOUT: Duration = Duration::from_secs(30);

async fn read_raw(spec: &RemoteSpec, abs: &str) -> Result<(u64, Vec<u8>), String> {
    let head = r#"
$ErrorActionPreference='Stop'
$p=__P__
if (-not (Test-Path -LiteralPath $p)) { [Console]::Error.Write('路径不存在'); exit 2 }
(Get-Item -LiteralPath $p).Length
"#
        .replace("__P__", &ps_lit(abs));
    let sz_out = ssh::exec_ps(spec, &head, OP_TIMEOUT).await?;
    if !sz_out.ok() {
        return Err(format!("远程读取失败：{}", sz_out.stderr.trim()));
    }
    let size: u64 = sz_out.stdout.trim().parse().unwrap_or(0);
    if size > MAX_BINARY_BYTES {
        return Err(format!(
            "文件过大（{size} 字节，上限 {MAX_BINARY_BYTES} 字节），IDE 不加载"
        ));
    }

    let script = r#"
$ErrorActionPreference='Stop'
$p=__P__
$fs=[IO.File]::OpenRead($p)
try {
  $buf=New-Object byte[] (1MB)
  while (($n=$fs.Read($buf,0,$buf.Length)) -gt 0) {
    $seg=New-Object byte[] $n
    [Array]::Copy($buf,$seg,$n)
    Write-Output ('CHUNK:' + [Convert]::ToBase64String($seg))
  }
} finally { $fs.Close() }
"#
        .replace("__P__", &ps_lit(abs));
    let out = ssh::exec_ps(spec, &script, Duration::from_secs(120)).await?;
    if !out.ok() {
        return Err(format!("远程读取失败：{}", out.stderr.trim()));
    }
    use base64::Engine;
    let mut bytes: Vec<u8> = Vec::with_capacity(size as usize);
    for line in out.stdout.lines() {
        if let Some(v) = line.strip_prefix("CHUNK:") {
            let seg = base64::engine::general_purpose::STANDARD
                .decode(v.trim().as_bytes())
                .map_err(|e| format!("base64 解码失败: {e}"))?;
            bytes.extend_from_slice(&seg);
        }
    }
    Ok((size, bytes))
}

/// Windows 读取文本文件。
pub async fn read_text(spec: &RemoteSpec, rel: &str) -> Result<String, String> {
    let abs = win_resolve(&spec.root, rel, spec.restrict)?;
    let (size, bytes) = read_raw(spec, &abs).await?;
    if size > MAX_READ_BYTES {
        return Err(format!(
            "文件过大（{size} 字节，上限 {MAX_READ_BYTES} 字节），IDE 不加载"
        ));
    }
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err("二进制文件，无法以文本打开".to_string());
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Windows 读取二进制文件。
pub async fn read_binary(spec: &RemoteSpec, rel: &str) -> Result<(Vec<u8>, u64), String> {
    let abs = win_resolve(&spec.root, rel, spec.restrict)?;
    let (size, bytes) = read_raw(spec, &abs).await?;
    if size > MAX_BINARY_BYTES {
        return Err(format!(
            "文件过大（{size} 字节，上限 {MAX_BINARY_BYTES} 字节），IDE 不预览"
        ));
    }
    Ok((bytes, size))
}

/// Windows 列目录。
pub async fn list_dir(spec: &RemoteSpec, rel: &str) -> Result<Vec<DirEntry>, String> {
    let abs = win_resolve(&spec.root, rel, spec.restrict)?;
    let script = r#"
$ErrorActionPreference='Stop'
$p=__P__
if (-not (Test-Path -LiteralPath $p)) { [Console]::Error.Write('路径不存在'); exit 2 }
Get-ChildItem -LiteralPath $p -Force | ForEach-Object {
  $t = if ($_.PSIsContainer) { 'd' } elseif ($_.LinkType) { 'l' } else { 'f' }
  $s = if ($_.PSIsContainer) { 0 } else { $_.Length }
  "$t`t$s`t$($_.Name)"
}
"#
    .replace("__P__", &ps_lit(&abs));
    let out = ssh::exec_ps(spec, &script, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程列目录失败：{}", out.stderr.trim()));
    }
    let base = rel.trim_matches(['/', '\\']);
    let mut entries = Vec::new();
    for line in out.stdout.lines() {
        if line.is_empty() {
            continue;
        }
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
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Windows 写文本文件（base64 走 stdin）。
pub async fn write_file(spec: &RemoteSpec, rel: &str, content: &str) -> Result<(), String> {
    let abs = win_resolve(&spec.root, rel, spec.restrict)?;
    let script = r#"
$ErrorActionPreference='Stop'
$p=__P__
$parent=Split-Path -Parent $p
if ($parent -and -not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$b64=[Console]::In.ReadToEnd()
$bytes=[Convert]::FromBase64String($b64.Trim())
[IO.File]::WriteAllBytes($p,$bytes)
"#
    .replace("__P__", &ps_lit(&abs));
    use base64::Engine;
    // .ps1/.psm1/.psd1 加 UTF-8 BOM：万一被 PowerShell 5.1 执行，5.1 才认 UTF-8。
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
    let out = ssh::exec_ps_stdin(spec, &script, b64.as_bytes(), OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程保存失败：{}", out.stderr.trim()));
    }
    Ok(())
}

/// Windows 新建文件或目录。
pub async fn create(spec: &RemoteSpec, rel: &str, is_dir: bool) -> Result<(), String> {
    let abs = win_resolve(&spec.root, rel, spec.restrict)?;
    let script = if is_dir {
        r#"$ErrorActionPreference='Stop'; $p=__P__; New-Item -ItemType Directory -Force -Path $p | Out-Null"#
    } else {
        r#"$ErrorActionPreference='Stop'; $p=__P__; $parent=Split-Path -Parent $p; if ($parent -and -not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }; if (-not (Test-Path -LiteralPath $p)) { New-Item -ItemType File -Path $p | Out-Null }"#
    }
    .replace("__P__", &ps_lit(&abs));
    let out = ssh::exec_ps(spec, &script, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程新建失败：{}", out.stderr.trim()));
    }
    Ok(())
}

/// Windows 删除。
pub async fn delete(spec: &RemoteSpec, rel: &str) -> Result<(), String> {
    let abs = win_resolve(&spec.root, rel, spec.restrict)?;
    let root_norm = normalize_win(&spec.root);
    if abs.to_lowercase() == root_norm.to_lowercase() {
        return Err("不能删除工作目录根".to_string());
    }
    let script = r#"$ErrorActionPreference='Stop'; $p=__P__; if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }"#
        .replace("__P__", &ps_lit(&abs));
    let out = ssh::exec_ps(spec, &script, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程删除失败：{}", out.stderr.trim()));
    }
    Ok(())
}

/// Windows 改名/移动。
pub async fn rename(spec: &RemoteSpec, from_rel: &str, to_rel: &str) -> Result<(), String> {
    let from = win_resolve(&spec.root, from_rel, spec.restrict)?;
    let to = win_resolve(&spec.root, to_rel, spec.restrict)?;
    let script = r#"$ErrorActionPreference='Stop'; $f=__F__; $t=__T__; $parent=Split-Path -Parent $t; if ($parent -and -not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }; Move-Item -LiteralPath $f -Destination $t -Force"#
        .replace("__F__", &ps_lit(&from))
        .replace("__T__", &ps_lit(&to));
    let out = ssh::exec_ps(spec, &script, OP_TIMEOUT).await?;
    if !out.ok() {
        return Err(format!("远程移动失败：{}", out.stderr.trim()));
    }
    Ok(())
}

/// Windows 探测（供调用方避免重复 uname）。
pub async fn probe(spec: &RemoteSpec) -> bool {
    is_windows_remote(spec).await
}
#[cfg(test)]
mod tests {
    use super::*;

    fn gongwei() -> RemoteSpec {
        RemoteSpec {
            host: "gongwei".into(),
            user: None,
            port: None,
            root: "C:/Users/z".into(),
            ssh_alias: Some("gongwei".into()),
            proxy_jump: None,
            restrict: true,
        }
    }

    #[tokio::test]
    #[ignore]
    async fn win_roundtrip() {
        let spec = gongwei();
        assert!(is_windows_remote(&spec).await);
        let entries = list_dir(&spec, "").await.unwrap();
        assert!(!entries.is_empty());
        let content = "hello 世界\n<tag> Result<X>\n";
        write_file(&spec, "ligong-test/中文 目录/a.txt", content).await.unwrap();
        let got = read_text(&spec, "ligong-test/中文 目录/a.txt").await.unwrap();
        assert_eq!(got, content);
        create(&spec, "ligong-test/newdir", true).await.unwrap();
        rename(&spec, "ligong-test/中文 目录/a.txt", "ligong-test/newdir/b.txt").await.unwrap();
        let got2 = read_text(&spec, "ligong-test/newdir/b.txt").await.unwrap();
        assert_eq!(got2, content);
        delete(&spec, "ligong-test").await.unwrap();
    }
}