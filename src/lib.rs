//! DeepSeek Agent GUI —— Tauri 后端。
//!
//! 复用 deepseek-agent 库（path 依赖），不修改原项目。
//! 账号体系与原 CLI 共用 ~/.deepseek-agent。

use tauri::Manager;

pub mod bootstrap;
pub mod commands;
pub mod env_probe;
pub mod logging;
pub mod notify;
#[cfg(windows)]
pub mod notify_win;
pub mod projects;
pub mod prompt_gui;
pub mod qq;
pub mod runner;
pub mod shell;
pub mod state;
pub mod tools;

pub fn run() {
    #[cfg(windows)]
    {
        logging::init();
    }
    bootstrap::release_seed_accounts();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|_app| {
            #[cfg(windows)]
            {
                notify_win::set_app_id();
                std::thread::spawn(|| {
                    notify_win::ensure_shortcut();
                });
            }
            Ok(())
        })
        .manage(state::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::account_status,
            commands::log_frontend,
            commands::list_accounts,
            commands::login,
            commands::list_projects,
            commands::create_project,
            commands::rename_project,
            commands::delete_project,
            commands::open_project,
            commands::list_conversations,
            commands::new_conversation,
            commands::switch_conversation,
            commands::delete_conversation,
            commands::load_conversation_messages,
            commands::load_project_messages,
            commands::get_mode,
            commands::set_mode,
            commands::get_thinking,
            commands::set_thinking,
            commands::get_search,
            commands::set_search,
            commands::send_message,
            commands::interrupt,
            commands::approve_command,
            commands::busy_projects,
            commands::window_minimize,
            commands::window_close,
            commands::window_start_drag,
            commands::upload_files,
            commands::set_pending_files,
            commands::clear_pending_files,
            commands::get_workdir,
            commands::set_workdir,
            commands::get_restrict,
            commands::set_restrict,
            commands::get_compact_threshold,
            commands::set_compact_threshold,
            commands::remove_file,
            commands::list_web_files,
            commands::login_browser,
            commands::delete_account,
            commands::list_account_sessions,
            commands::delete_account_sessions,
            commands::clear_local_history,
            commands::detach_conversation,
            commands::list_project_account_sessions,
            commands::attach_account_session,
            commands::qq_status,
            commands::qq_toggle,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let state = window.state::<state::AppState>();
                let inner = state.inner.clone();
                tauri::async_runtime::block_on(async move {
                    let bridges: Vec<_> = {
                        let guard = inner.lock().await;
                        guard.bridges.values().cloned().collect()
                    };
                    for b in bridges {
                        b.shutdown().await;
                    }
                    deepseek_agent::agent::browser::shutdown().await;
                    let mut guard = inner.lock().await;
                    guard.bridges.clear();
                    guard.running.clear();
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
#[cfg(test)]
mod asset_tests {
    #[test]
    fn frontend_assets_embedded_and_current() {
        let ctx: tauri::Context<tauri::Wry> = tauri::generate_context!();
        let assets = ctx.assets();
        let key: tauri::utils::assets::AssetKey = "/app.js".into();
        let data = assets.get(&key).expect("app.js 未内嵌");
        let text = String::from_utf8_lossy(&data);
        assert!(
            text.contains("scheduleRender"),
            "app.js 缺少 scheduleRender（前端未更新）"
        );
        assert!(
            text.contains("function streamText"),
            "app.js 缺少 streamText"
        );
        assert!(
            text.contains("scheduleToolRender"),
            "app.js 缺少 scheduleToolRender（工具输出节流未内嵌）"
        );
        assert!(
            text.contains("TOOL_OUT_PREVIEW_TAIL"),
            "app.js 缺少工具输出截断常量"
        );
    }

    #[test]
    fn stylesheet_allows_text_selection() {
        let ctx: tauri::Context<tauri::Wry> = tauri::generate_context!();
        let assets = ctx.assets();
        let key: tauri::utils::assets::AssetKey = "/style.css".into();
        let data = assets.get(&key).expect("style.css 未内嵌");
        let css = String::from_utf8_lossy(&data);
        assert!(
            css.contains("user-select: text"),
            "style.css 缺少文本可选中声明"
        );
        assert!(
            css.contains("-webkit-user-drag: none"),
            "style.css 缺少禁用拖拽声明"
        );
    }
}
