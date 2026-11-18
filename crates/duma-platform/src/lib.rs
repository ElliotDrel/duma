//! Native OS layer. Everything here talks straight to the OS:
//! UI Automation, SendInput and the Win32 clipboard on Windows;
//! Accessibility (AX), CGEvent and NSPasteboard on macOS.
//!
//! Public API is identical on both platforms; `imp` holds the per-OS code.

#[cfg(feature = "audio")]
pub mod audio;
pub mod hotkeys;
mod insert;

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;

#[cfg(not(any(windows, target_os = "macos")))]
#[path = "unsupported.rs"]
mod imp;

use serde::Serialize;

pub use imp::{
    ClipboardSnapshot, FieldHandle, accessibility_trusted, request_accessibility,
    secure_input_active,
};
pub use insert::{InsertReport, capture_selection_via_clipboard, insert_text};

/// Stamped on every key event Duma injects, so its own Ctrl/Cmd+V can be told apart from the user's keys.
pub const INJECTED_MARKER: usize = 0x0D0A_D0A0;

/// The focused app and window, captured at the moment an action starts.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AppContext {
    pub app_name: Option<String>,
    /// Executable path on Windows, bundle identifier on macOS.
    pub app_id: Option<String>,
    pub pid: Option<i32>,
    pub window_title: Option<String>,
    /// The target runs elevated and Duma does not, so Windows silently drops injected keys (UIPI).
    pub is_elevated: bool,
}

/// What a text field reported at one moment.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct FieldState {
    /// Full field text, when the OS exposes it.
    pub text: Option<String>,
    /// Selection as (start, length) in UTF-16 code units, when exposed.
    pub selection: Option<(usize, usize)>,
    pub selected_text: Option<String>,
}

/// Static facts about the focused field.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FieldInfo {
    /// UI Automation control type or AX role.
    pub role: Option<String>,
    pub is_secure: bool,
}

/// Everything known about where the user is typing.
#[derive(Debug, Default)]
pub struct Focus {
    pub app: AppContext,
    pub field: Option<FieldHandle>,
    pub info: FieldInfo,
    pub state: FieldState,
}

/// Captures the focused app, window and text field.
pub fn focus() -> Focus {
    imp::focus()
}

/// Physical modifier keys currently held (Ctrl, Alt/Option, Shift, Win/Cmd).
pub fn modifiers_down() -> bool {
    imp::modifiers_down()
}

/// Waits until no modifier is physically held, so injected shortcuts are not combined with them.
pub fn wait_modifiers_released(timeout: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    while modifiers_down() {
        if started.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    true
}

/// Sends the platform paste shortcut (Ctrl+V / Cmd+V).
pub fn send_paste() -> anyhow::Result<()> {
    imp::send_shortcut('v')
}

/// Sends the platform copy shortcut (Ctrl+C / Cmd+C).
pub fn send_copy() -> anyhow::Result<()> {
    imp::send_shortcut('c')
}

/// Changes every time anything writes the clipboard.
pub fn clipboard_sequence() -> u64 {
    imp::clipboard_sequence()
}

pub fn clipboard_text() -> Option<String> {
    imp::clipboard_text()
}

pub fn set_clipboard_text(text: &str) -> anyhow::Result<()> {
    imp::set_clipboard_text(text)
}

/// Counts characters the way the field APIs do (UTF-16 code units on both platforms).
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Converts a UTF-16 offset into a byte offset in `s`, clamped to its length.
pub fn utf16_to_byte(s: &str, utf16_offset: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in s.char_indices() {
        if units >= utf16_offset {
            return byte;
        }
        units += ch.len_utf16();
    }
    s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets() {
        let s = "a😀b";
        assert_eq!(utf16_len(s), 4);
        assert_eq!(utf16_to_byte(s, 0), 0);
        assert_eq!(utf16_to_byte(s, 1), 1);
        assert_eq!(utf16_to_byte(s, 3), 5);
        assert_eq!(utf16_to_byte(s, 99), s.len());
    }
}
