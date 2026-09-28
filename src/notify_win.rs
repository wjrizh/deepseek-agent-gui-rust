//! Windows 绿色版通知注册：创建带 AppUserModelID 的开始菜单快捷方式，
//! 并设置进程 AUMID，使未安装（便携）的 exe 也能弹出系统通知。
//!
//! Windows 要求 toast 通知来源必须有一个被系统识别的 AUMID：
//! 要么是 MSIX 打包应用，要么是「开始菜单快捷方式 + System.AppUserModel.ID」。
//! 便携 exe 两者都没有，导致通知静默失败（HRESULT 0x80070490）。
//! 本模块在启动时自动完成注册，无需用户手动操作。

#![cfg(windows)]

use std::io::Write;
use std::path::PathBuf;
use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SHGetFolderPathW, SetCurrentProcessExplicitAppUserModelID, ShellLink, CSIDL_PROGRAMS};

const APP_ID: &str = "com.ligong.deepseek-agent-gui-rust";
const LNK_NAME: &str = "力工Studio.lnk";

fn log(msg: &str) {
    let path = std::env::temp_dir().join("ligong_notify.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{msg}");
    }
}

pub fn set_app_id() {
    log("set_app_id: ENTER");
    let app_id: HSTRING = APP_ID.into();
    if let Err(err) = unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(app_id.as_ptr())) } {
        log(&format!("SetCurrentProcessExplicitAppUserModelID 失败: {err}"));
    } else {
        log("SetCurrentProcessExplicitAppUserModelID ok");
    }
    log("set_app_id: done");
}

pub fn ensure_shortcut() {
    log("ensure_shortcut: ENTER");
    let result = std::panic::catch_unwind(|| {
        log("ensure_shortcut: inside catch_unwind");
        match create_start_menu_shortcut() {
            Ok(()) => log("create_start_menu_shortcut ok"),
            Err(err) => log(&format!("创建开始菜单快捷方式失败: {err}")),
        }
        log("ensure_shortcut: after match");
    });
    log("ensure_shortcut: catch_unwind returned");
    if result.is_err() {
        log("ensure_shortcut: panic caught");
    }
    log("ensure_shortcut: done");
}

fn start_menu_dir() -> Result<PathBuf, String> {
    log("start_menu_dir: enter");
    let mut buf = [0u16; MAX_PATH as usize];
    unsafe { SHGetFolderPathW(None, CSIDL_PROGRAMS as i32, None, 0, &mut buf) }
        .map_err(|e| format!("SHGetFolderPathW: {e}"))?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let p = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    log(&format!("start_menu_dir: exit {:?}", p));
    Ok(p)
}

fn exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("current_exe: {e}"))
}

fn create_start_menu_shortcut() -> Result<(), String> {
    log("create: ENTER");
    let dir = start_menu_dir()?;
    log("create: got dir");
    let lnk_path = dir.join(LNK_NAME);
    let exe = exe_path()?;
    log(&format!("create: exe={:?}", exe));

    unsafe {
        log("create: CoInitializeEx");
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        log(&format!("create: CoInitializeEx hr={hr:?}"));

        log("create: CoCreateInstance");
        let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("CoCreateInstance(ShellLink): {e}"))?;
        log("create: CoCreateInstance ok");

        log("create: SetPath");
        let exe_wide = to_wide(exe.to_string_lossy().as_ref());
        shell_link
            .SetPath(PCWSTR(exe_wide.as_ptr()))
            .map_err(|e| format!("SetPath: {e}"))?;
        log("create: SetPath ok");

        if let Some(parent) = exe.parent() {
            let parent_wide = to_wide(parent.to_string_lossy().as_ref());
            let _ = shell_link.SetWorkingDirectory(PCWSTR(parent_wide.as_ptr()));
            log("create: SetWorkingDirectory done");
        }

        log("create: cast IPropertyStore");
        let store: IPropertyStore = shell_link
            .cast()
            .map_err(|e| format!("cast IPropertyStore: {e}"))?;
        log("create: cast IPropertyStore ok");

        log("create: build PROPVARIANT");
        let app_id_wide = to_wide(APP_ID);
        let variant = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: core::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_LPWSTR,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        pwszVal: PWSTR(app_id_wide.as_ptr() as *mut u16),
                    },
                }),
            },
        };
        log("create: PROPVARIANT built");
        log("create: SetValue");
        store
            .SetValue(&PKEY_AppUserModel_ID, &variant)
            .map_err(|e| format!("SetValue(PKEY_AppUserModel_ID): {e}"))?;
        log("create: SetValue ok");
        store.Commit().map_err(|e| format!("Commit: {e}"))?;
        log("create: Commit ok");

        std::mem::forget(variant);
        log("create: variant forgotten");

        log("create: cast IPersistFile");
        let persist: IPersistFile = shell_link
            .cast()
            .map_err(|e| format!("cast IPersistFile: {e}"))?;
        log("create: cast IPersistFile ok");
        let lnk_wide = to_wide(lnk_path.to_string_lossy().as_ref());
        log("create: before Save");
        persist
            .Save(PCWSTR(lnk_wide.as_ptr()), true)
            .map_err(|e| format!("Save: {e}"))?;
        log("create: after Save");

        log("create: dropping persist");
        drop(persist);
        log("create: dropping store");
        drop(store);
        log("create: dropping shell_link");
        drop(shell_link);
        log("create: all COM dropped");
    }

    log("create: returning Ok(())");
    Ok(())
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}