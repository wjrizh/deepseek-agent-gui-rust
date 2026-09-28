#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    elevate_if_needed();
    deepseek_agent_gui_rust_lib::run();
}

/// 首次启动若未提权，则用 UAC 以管理员身份重启自身并退出。
/// 这样 GUI 进程全程是管理员：execute_command 无需再提权，
/// 也不会再弹出 PowerShell/UAC 窗口。用户取消 UAC 时按原权限继续运行。
#[cfg(windows)]
fn elevate_if_needed() {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    extern "system" {
        fn IsUserAnAdmin() -> i32;
    }
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> *mut std::ffi::c_void;
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    unsafe {
        if IsUserAnAdmin() != 0 {
            return;
        }
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let params = args
        .iter()
        .map(|a| {
            if a.contains(' ') || a.contains('"') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let op = wide(OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let params_w = wide(OsStr::new(&params));
    let dir = exe
        .parent()
        .map(|d| wide(d.as_os_str()))
        .unwrap_or_else(|| wide(OsStr::new("")));

    const SW_SHOWNORMAL: i32 = 1;
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            if params.is_empty() {
                std::ptr::null()
            } else {
                params_w.as_ptr()
            },
            dir.as_ptr(),
            SW_SHOWNORMAL,
        )
    };
    // 返回值 <= 32 表示失败（含用户取消 UAC）。失败则按原权限继续。
    if result as isize > 32 {
        std::process::exit(0);
    }
}