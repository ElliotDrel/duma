//! The floating status pill. It must never take focus from the app the user is typing in:
//! a non-activating NSPanel on macOS, a WS_EX_NOACTIVATE window shown with SW_SHOWNOACTIVATE on Windows.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use duma_core::engine::UiEvent;
use tauri::{AppHandle, Manager, WebviewUrl};

pub const LABEL: &str = "overlay";
const WIDTH: f64 = 460.0;
const HEIGHT: f64 = 120.0;
const BOTTOM_MARGIN: f64 = 72.0;

/// Bumped on every show; a delayed hide only runs if nothing was shown since.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
tauri_nspanel::tauri_panel! {
    panel!(DumaOverlayPanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

fn position(app: &AppHandle) -> Option<(f64, f64)> {
    let monitor = app.primary_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let size = monitor.work_area().size.to_logical::<f64>(scale);
    let origin = monitor.work_area().position.to_logical::<f64>(scale);
    Some((
        origin.x + (size.width - WIDTH) / 2.0,
        origin.y + size.height - HEIGHT - BOTTOM_MARGIN,
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn create(app: &AppHandle) {
    let (x, y) = position(app).unwrap_or((200.0, 200.0));
    let built =
        tauri::WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay.html".into()))
            .title("Duma")
            .inner_size(WIDTH, HEIGHT)
            .position(x, y)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focusable(false)
            .focused(false)
            .visible(false)
            .build();
    if let Err(error) = built {
        log::error!("creating the overlay failed: {error}");
    }
}

#[cfg(target_os = "macos")]
pub fn create(app: &AppHandle) {
    use tauri_nspanel::{CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};
    let (x, y) = position(app).unwrap_or((200.0, 200.0));
    let built = PanelBuilder::<_, DumaOverlayPanel>::new(app, LABEL)
        .url(WebviewUrl::App("overlay.html".into()))
        .title("Duma")
        .position(tauri::Position::Logical(tauri::LogicalPosition { x, y }))
        .size(tauri::Size::Logical(tauri::LogicalSize {
            width: WIDTH,
            height: HEIGHT,
        }))
        .level(PanelLevel::Status)
        .has_shadow(false)
        .transparent(true)
        .no_activate(true)
        .corner_radius(0.0)
        .style_mask(StyleMask::empty().borderless().nonactivating_panel())
        .with_window(|w| w.decorations(false).transparent(true).focusable(false))
        .collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary(),
        )
        .build();
    match built {
        Ok(panel) => panel.hide(),
        Err(error) => log::error!("creating the overlay panel failed: {error}"),
    }
}

fn show(app: &AppHandle) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(LABEL) else {
            return;
        };
        #[cfg(windows)]
        if let Ok(hwnd) = window.hwnd() {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                SetWindowPos, ShowWindow,
            };
            unsafe {
                let hwnd = HWND(hwnd.0);
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            return;
        }
        #[cfg(target_os = "macos")]
        {
            use tauri_nspanel::ManagerExt;
            if let Ok(panel) = handle.get_webview_panel(LABEL) {
                panel.show();
                return;
            }
        }
        let _ = window.show();
    });
}

fn hide_after(app: &AppHandle, delay: Duration) {
    let generation = GENERATION.load(Ordering::SeqCst);
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let inner = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            #[cfg(target_os = "macos")]
            {
                use tauri_nspanel::ManagerExt;
                if let Ok(panel) = inner.get_webview_panel(LABEL) {
                    panel.hide();
                    return;
                }
            }
            if let Some(window) = inner.get_webview_window(LABEL) {
                let _ = window.hide();
            }
        });
    });
}

/// Shows or hides the pill for an engine event. The pill's content is rendered by `overlay.html`.
pub fn on_event(app: &AppHandle, event: &UiEvent) {
    match event {
        UiEvent::Listening { .. } | UiEvent::Working { .. } => show(app),
        UiEvent::Partial { .. } | UiEvent::ThreadsChanged => {}
        UiEvent::Done { .. } => hide_after(app, Duration::from_millis(900)),
        UiEvent::Message { .. } => {
            show(app);
            hide_after(app, Duration::from_millis(4000));
        }
        UiEvent::Idle => hide_after(app, Duration::from_millis(150)),
    }
}
