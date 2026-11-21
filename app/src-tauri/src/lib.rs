//! Tauri shell: tray, overlay, main window and the commands the UI calls. All real work is in `duma-core`.

mod overlay;

use std::sync::Arc;

use duma_core::engine::{Engine, Status, UiEvent, UiSink};
use duma_core::paths::{Channel, Paths};
use duma_core::settings::Settings;
use duma_core::store::{ThreadSummary, Version};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

type CommandResult<T> = Result<T, String>;

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[tauri::command]
fn status(engine: State<'_, Engine>) -> Status {
    engine.status()
}

#[tauri::command]
fn get_settings(engine: State<'_, Engine>) -> Settings {
    engine.settings()
}

#[tauri::command]
fn save_settings(engine: State<'_, Engine>, settings: Settings) -> CommandResult<()> {
    engine.set_settings(settings).map_err(err)
}

#[tauri::command]
fn set_api_key(engine: State<'_, Engine>, key: String) -> CommandResult<()> {
    engine.set_api_key(&key).map_err(err)
}

#[tauri::command]
fn list_threads(
    engine: State<'_, Engine>,
    limit: usize,
    offset: usize,
) -> CommandResult<Vec<ThreadSummary>> {
    engine.threads(limit, offset).map_err(err)
}

#[tauri::command]
fn get_thread(engine: State<'_, Engine>, id: String) -> CommandResult<Vec<Version>> {
    engine.thread(&id).map_err(err)
}

#[tauri::command]
fn request_accessibility() {
    duma_core::platform::request_accessibility();
}

#[tauri::command]
fn default_settings() -> Settings {
    Settings::default()
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Duma", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Duma", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let channel = Channel::current();
    TrayIconBuilder::with_id("duma")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip(if channel == Channel::Dev {
            "Duma (dev)"
        } else {
            "Duma"
        })
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .plugin(tauri_plugin_opener::init());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .setup(|app| {
            let handle = app.handle().clone();
            overlay::create(&handle);
            let ui_handle = handle.clone();
            let ui: UiSink = Arc::new(move |event: UiEvent| {
                overlay::on_event(&ui_handle, &event);
                let _ = ui_handle.emit("duma://ui", &event);
            });
            let engine = Engine::start(Paths::for_channel(Channel::current()), ui)?;
            let status = engine.status();
            app.manage(engine);
            // The main window is `create: false` in the config so its page can't call a command
            // before the engine above is managed.
            let main_config = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .expect("main window config");
            tauri::WebviewWindowBuilder::from_config(&handle, main_config)?.build()?;
            build_tray(&handle)?;
            if status.api_key.is_none() || !status.accessibility_trusted {
                show_main(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window keeps Duma running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            status,
            get_settings,
            save_settings,
            set_api_key,
            list_threads,
            get_thread,
            request_accessibility,
            default_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Duma");
}
