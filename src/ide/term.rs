//! IDE 内置终端：基于 PTY 的交互式 shell 会话。
//!
//! 仅用于 IDE 窗口的底部终端；Agent 主流程不经过此处（仍走 `shell.rs` 的一次性执行）。
//! 直接启动用户 shell（不套 `sh -c`、不 sudo），保证 heredoc/管道等语法原样工作。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

/// 终端会话（master 用于读写与调整尺寸）。
struct TermSession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// 全局终端会话表。
#[derive(Default)]
pub struct TermState {
    sessions: Mutex<HashMap<String, TermSession>>,
}

/// 回传给前端的事件。
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TermEvent {
    /// 终端输出（原始字节按 UTF-8 lossy 转字符串）。
    Data { data: String },
    /// 进程退出。
    Exit { code: i32 },
}

/// 默认 shell：Unix 取 $SHELL，回退 /bin/bash；Windows 取 COMSPEC，回退 cmd。
fn default_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "/bin/bash".to_string())
    }
}

/// 打开一个终端会话，返回会话 id。
#[tauri::command]
pub async fn term_open(
    id: String,
    cwd: Option<String>,
    shell: Option<String>,
    cols: u16,
    rows: u16,
    on_event: Channel<TermEvent>,
    state: State<'_, TermState>,
) -> Result<(), String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("创建 PTY 失败：{e}"))?;

    let shell_path = shell
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(default_shell);

    let mut cmd = CommandBuilder::new(&shell_path);
    // 交互式登录 shell：与用户在真实终端里敲命令一致。
    cmd.arg("-i");
    if let Some(dir) = cwd.filter(|d| !d.trim().is_empty()) {
        if std::path::Path::new(&dir).is_dir() {
            cmd.cwd(dir);
        }
    }
    cmd.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("启动 shell 失败：{e}"))?;
    // slave 需要在此关闭，避免子进程退出后 read 不返回 EOF。
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("获取 PTY reader 失败：{e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("获取 PTY writer 失败：{e}"))?;

    // 读线程：持续把输出经 Channel 推给前端。
    let ev = on_event.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    if ev.send(TermEvent::Data { data }).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = ev.send(TermEvent::Exit { code: 0 });
    });

    let session = TermSession {
        master: pair.master,
        writer,
        child,
    };
    state
        .sessions
        .lock()
        .map_err(|_| "终端状态锁失败".to_string())?
        .insert(id, session);
    Ok(())
}

/// 向终端写入输入（键盘输入、粘贴）。
#[tauri::command]
pub async fn term_write(
    id: String,
    data: String,
    state: State<'_, TermState>,
) -> Result<(), String> {
    let mut sessions = state
        .sessions
        .lock()
        .map_err(|_| "终端状态锁失败".to_string())?;
    let s = sessions
        .get_mut(&id)
        .ok_or_else(|| "终端会话不存在".to_string())?;
    s.writer
        .write_all(data.as_bytes())
        .map_err(|e| format!("写入终端失败：{e}"))?;
    s.writer.flush().ok();
    Ok(())
}

/// 调整终端尺寸。
#[tauri::command]
pub async fn term_resize(
    id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TermState>,
) -> Result<(), String> {
    let sessions = state
        .sessions
        .lock()
        .map_err(|_| "终端状态锁失败".to_string())?;
    let s = sessions
        .get(&id)
        .ok_or_else(|| "终端会话不存在".to_string())?;
    s.master
        .resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("调整终端尺寸失败：{e}"))?;
    Ok(())
}

/// 关闭并回收终端会话。
#[tauri::command]
pub async fn term_close(id: String, state: State<'_, TermState>) -> Result<(), String> {
    let mut sessions = state
        .sessions
        .lock()
        .map_err(|_| "终端状态锁失败".to_string())?;
    if let Some(mut s) = sessions.remove(&id) {
        let _ = s.child.kill();
        let _ = s.child.wait();
    }
    Ok(())
}