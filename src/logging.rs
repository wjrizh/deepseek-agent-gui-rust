//! 运行日志：把 stderr 重定向到文件，便于 Windows（无控制台）排查闪退。
//!
//! - 日志路径：~/.ligong/logs/app.log（追加写）。
//! - Unix：dup2 覆盖 fd 2。
//! - Windows：SetStdHandle 覆盖 STD_ERROR_HANDLE。
//! - 同时安装 panic hook，把崩溃信息与位置写入同一文件。

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn log_path() -> Option<&'static PathBuf> {
    LOG_PATH.get()
}

fn open_log_file() -> Option<std::fs::File> {
    let home = dirs::home_dir()?;
    let dir = home.join(".ligong").join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("app.log");
    let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    let _ = LOG_PATH.set(path);
    Some(file)
}

#[cfg(unix)]
fn redirect_stderr(file: std::fs::File) {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    unsafe {
        libc::dup2(fd, 2);
    }
    std::mem::forget(file);
}

#[cfg(windows)]
fn redirect_stderr(file: std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    let handle = HANDLE(file.as_raw_handle() as *mut core::ffi::c_void);
    unsafe {
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
    }
    std::mem::forget(file);
}

pub fn init() {
    let Some(file) = open_log_file() else {
        return;
    };
    redirect_stderr(file);

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<非字符串 panic>".into());
        eprintln!("[panic] {} | 位置: {}", msg, loc.as_deref().unwrap_or("<未知>"));
        default_hook(info);
    }));

}

