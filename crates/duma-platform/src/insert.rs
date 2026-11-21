//! Cross-platform insertion and selection capture on top of the per-OS clipboard and input primitives.
//!
//! Rules carried over from Universal Spell Check and Coda:
//! - wait for the user's physical modifiers to be released before injecting a shortcut, and refuse if they never are;
//! - back up every clipboard format first and restore it on every path, but only if nothing newer was copied;
//! - restore only once the paste is seen in the field (or after a grace period when the field is unreadable).

use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::{
    ClipboardSnapshot, FieldHandle, clipboard_sequence, clipboard_text, send_copy, send_paste,
    set_clipboard_text,
};

/// Time for the clipboard write to become visible to other processes.
const PUBLISH_SETTLE: Duration = Duration::from_millis(30);
/// Longest wait for the paste to appear in a readable field before restoring anyway.
const PASTE_CONFIRM_TIMEOUT: Duration = Duration::from_millis(1500);
/// Grace period when the field cannot be read.
const PASTE_GRACE: Duration = Duration::from_millis(400);
const COPY_TIMEOUT: Duration = Duration::from_millis(400);
const MODIFIER_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, Serialize)]
pub struct InsertReport {
    pub method: &'static str,
    pub modifier_wait_ms: u64,
    pub clipboard_formats_backed_up: usize,
    pub clipboard_backup_error: Option<String>,
    pub restored_clipboard: bool,
    pub restore_error: Option<String>,
    /// Something else wrote the clipboard during the paste window, so the backup was not restored.
    pub clipboard_changed_by_other: bool,
    pub paste_sent_ms: u64,
    /// When the field was first seen containing the change, if it could be read.
    pub paste_observed_ms: Option<u64>,
    pub total_ms: u64,
}

/// Restores the user's clipboard when dropped, unless something newer than our own write is on it.
struct ClipboardRestore {
    backup: Option<ClipboardSnapshot>,
    ours: Option<u64>,
}

impl ClipboardRestore {
    fn finish(mut self, report: &mut InsertReport) {
        let Some(ours) = self.ours else { return };
        if clipboard_sequence() != ours {
            report.clipboard_changed_by_other = true;
            self.backup = None;
            return;
        }
        if let Some(backup) = self.backup.take() {
            match backup.restore() {
                Ok(()) => report.restored_clipboard = true,
                Err(error) => report.restore_error = Some(error.to_string()),
            }
        }
    }
}

impl Drop for ClipboardRestore {
    fn drop(&mut self) {
        if let (Some(backup), Some(ours)) = (self.backup.take(), self.ours)
            && clipboard_sequence() == ours
        {
            let _ = backup.restore();
        }
    }
}

fn wait_modifiers(report_ms: &mut u64) -> Result<()> {
    let started = Instant::now();
    let released = crate::wait_modifiers_released(MODIFIER_TIMEOUT);
    *report_ms = started.elapsed().as_millis() as u64;
    if !released {
        bail!("a modifier key is still held; not pasting so it cannot trigger another shortcut");
    }
    Ok(())
}

/// Pastes `text` into the focused field and restores the user's clipboard.
/// `field`, when readable, is used to confirm the paste landed before the clipboard is restored.
pub fn insert_text(text: &str, field: Option<&FieldHandle>) -> Result<InsertReport> {
    let started = Instant::now();
    let mut report = InsertReport {
        method: "clipboard_paste",
        ..Default::default()
    };
    wait_modifiers(&mut report.modifier_wait_ms)?;

    let before = field.and_then(|f| f.read().ok()).and_then(|s| s.text);
    let backup = match ClipboardSnapshot::capture() {
        Ok(snapshot) => {
            report.clipboard_formats_backed_up = snapshot.format_count();
            Some(snapshot)
        }
        Err(error) => {
            report.clipboard_backup_error = Some(error.to_string());
            None
        }
    };
    let mut restore = ClipboardRestore { backup, ours: None };

    set_clipboard_text(text)?;
    restore.ours = Some(clipboard_sequence());
    sleep(PUBLISH_SETTLE);
    send_paste()?;
    report.paste_sent_ms = started.elapsed().as_millis() as u64;

    match (field, before) {
        (Some(field), Some(before)) => {
            let deadline = Instant::now() + PASTE_CONFIRM_TIMEOUT;
            while Instant::now() < deadline {
                sleep(Duration::from_millis(15));
                if field
                    .read()
                    .ok()
                    .and_then(|s| s.text)
                    .is_some_and(|now| now != before)
                {
                    report.paste_observed_ms = Some(started.elapsed().as_millis() as u64);
                    // Some apps read the clipboard more than once; give them a moment.
                    sleep(Duration::from_millis(60));
                    break;
                }
            }
        }
        _ => sleep(PASTE_GRACE),
    }

    restore.finish(&mut report);
    report.total_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Reads the selection by sending Ctrl/Cmd+C, for fields that do not expose text to accessibility APIs.
/// Returns `None` when nothing was copied. The user's clipboard is always restored.
/// Do not use in terminals (Ctrl+C interrupts) or editors that copy the whole line when nothing is selected.
pub fn capture_selection_via_clipboard() -> Result<Option<String>> {
    let mut wait_ms = 0;
    wait_modifiers(&mut wait_ms)?;
    let backup = ClipboardSnapshot::capture().ok();
    let before = clipboard_sequence();
    send_copy()?;
    let started = Instant::now();
    let mut copied = None;
    let mut changed = false;
    while started.elapsed() < COPY_TIMEOUT {
        if clipboard_sequence() != before {
            changed = true;
            sleep(Duration::from_millis(10));
            copied = clipboard_text();
            break;
        }
        sleep(Duration::from_millis(5));
    }
    if changed && let Some(backup) = backup {
        let _ = backup.restore();
    }
    Ok(copied.filter(|t| !t.is_empty()))
}
