//! Stubs so the workspace builds on platforms Duma does not support yet (Linux).

use anyhow::{Result, bail};

use crate::{FieldState, Focus};

#[derive(Clone, Debug)]
pub struct FieldHandle;

impl FieldHandle {
    pub fn read(&self) -> Result<FieldState> {
        bail!("unsupported platform")
    }
    pub fn select(&self, _start: usize, _expected: &str) -> Result<()> {
        bail!("unsupported platform")
    }
    pub fn is_focused(&self) -> bool {
        false
    }
}

pub struct ClipboardSnapshot;

impl ClipboardSnapshot {
    pub fn capture() -> Result<Self> {
        bail!("unsupported platform")
    }
    pub fn format_count(&self) -> usize {
        0
    }
    pub fn restore(&self) -> Result<()> {
        bail!("unsupported platform")
    }
}

pub fn accessibility_trusted() -> bool {
    false
}
pub fn request_accessibility() {}
pub fn secure_input_active() -> bool {
    false
}
pub fn focus() -> Focus {
    Focus::default()
}
pub fn modifiers_down() -> bool {
    false
}
pub fn send_shortcut(_letter: char) -> Result<()> {
    bail!("unsupported platform")
}
pub fn clipboard_sequence() -> u64 {
    0
}
pub fn clipboard_text() -> Option<String> {
    None
}
pub fn set_clipboard_text(_text: &str) -> Result<()> {
    bail!("unsupported platform")
}
