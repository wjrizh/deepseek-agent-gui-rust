//! 工作结束通知 + 点击聚焦。
//!
//! 发送通知后后台阻塞等待用户点击，点击（body 或按钮）时聚焦主窗口。
//! Windows 需要 AUMID（见 notify_win）；Linux 走 D-Bus action。

use notify_rust::{Notification, NotificationResponse};
use tauri::{AppHandle, Manager};

#[cfg(windows)]
const APP_ID: &str = "com.ligong.deepseek-agent-gui-rust";

pub fn notify_work_done(app: &AppHandle, project_name: &str) {
    let body = format!("项目「{project_name}」工作结束");

    let mut notification = Notification::new();
    notification.summary("力工Studio").body(&body);
    notification.action("open", "打开");

    #[cfg(windows)]
    notification.app_id(APP_ID);

    let handle = match notification.show() {
        Ok(h) => h,
        Err(_) => {
            return;
        }
    };

    let app = app.clone();
    std::thread::spawn(move || {
        let _ = handle.wait_for_response(move |response: &NotificationResponse| {
            match response {
                NotificationResponse::Default | NotificationResponse::Action(_) => {
                    focus_main(&app);
                }
                _ => {}
            }
        });
    });
}

fn focus_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}