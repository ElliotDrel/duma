//! Windows: UI Automation for field text and selection, Win32 clipboard, SendInput.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoIncrementMTAUsage};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};
use windows::core::{BSTR, Interface, PWSTR, w};

use crate::{AppContext, FieldInfo, FieldState, Focus};

/// UIA calls into a hung app block for up to 20 s by default.
const UIA_TIMEOUT_MS: u32 = 500;
/// Forcing delayed-rendered clipboard formats can take seconds (large Excel copies).
const CAPTURE_TIME_BUDGET: Duration = Duration::from_millis(250);
const CAPTURE_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// Pins the process-wide MTA so every thread that has not initialized COM is an implicit MTA member
/// and UIA objects can move between worker threads. Never call UIA from the UI (STA) thread.
fn ensure_com() {
    static MTA: OnceLock<bool> = OnceLock::new();
    MTA.get_or_init(|| unsafe { CoIncrementMTAUsage().is_ok() });
}

fn automation() -> Result<IUIAutomation> {
    ensure_com();
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };
    if let Ok(timeouts) = automation.cast::<IUIAutomation2>() {
        unsafe {
            let _ = timeouts.SetConnectionTimeout(UIA_TIMEOUT_MS);
            let _ = timeouts.SetTransactionTimeout(UIA_TIMEOUT_MS);
        }
    }
    Ok(automation)
}

pub fn accessibility_trusted() -> bool {
    true
}

pub fn request_accessibility() {}

/// Windows has no secure-input mode equivalent; password fields are detected per element.
pub fn secure_input_active() -> bool {
    false
}

// ---------------------------------------------------------------------------------------------
// Focus and fields
// ---------------------------------------------------------------------------------------------

/// The focused UI Automation element. UIA client objects are free-threaded within the MTA, so the
/// handle can move to the edit-watch thread.
#[derive(Clone)]
pub struct FieldHandle {
    element: IUIAutomationElement,
    automation: IUIAutomation,
}

unsafe impl Send for FieldHandle {}
unsafe impl Sync for FieldHandle {}

impl std::fmt::Debug for FieldHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FieldHandle(uia)")
    }
}

pub fn focus() -> Focus {
    let mut focus = Focus {
        app: app_context(),
        ..Default::default()
    };
    let Ok(automation) = automation() else {
        return focus;
    };
    let Ok(element) = (unsafe { automation.GetFocusedElement() }) else {
        return focus;
    };
    let field = FieldHandle {
        element,
        automation,
    };
    focus.info = field.info();
    if !focus.info.is_secure {
        focus.state = field.read().unwrap_or_default();
    }
    focus.field = Some(field);
    focus
}

impl FieldHandle {
    fn info(&self) -> FieldInfo {
        unsafe {
            let role = self
                .element
                .CurrentControlType()
                .ok()
                .map(|t| control_type_name(t.0));
            let is_secure = self
                .element
                .CurrentIsPassword()
                .map(|b| b.as_bool())
                .unwrap_or(false);
            FieldInfo { role, is_secure }
        }
    }

    fn text_pattern(&self) -> Option<IUIAutomationTextPattern> {
        unsafe {
            self.element
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                .ok()
        }
    }

    /// Reads the field text and selection.
    pub fn read(&self) -> Result<FieldState> {
        unsafe {
            if self
                .element
                .CurrentIsPassword()
                .map(|b| b.as_bool())
                .unwrap_or(false)
            {
                bail!("secure field");
            }
            if let Some(pattern) = self.text_pattern() {
                let document = pattern.DocumentRange()?;
                let text = document.GetText(-1)?.to_string();
                let mut state = FieldState {
                    text: Some(text),
                    ..Default::default()
                };
                if let Ok(ranges) = pattern.GetSelection() {
                    if ranges.Length().unwrap_or(0) > 0 {
                        let selection = ranges.GetElement(0)?;
                        let selected = selection.GetText(-1)?.to_string();
                        let prefix = document.Clone()?;
                        prefix.MoveEndpointByRange(
                            TextPatternRangeEndpoint_End,
                            &selection,
                            TextPatternRangeEndpoint_Start,
                        )?;
                        let start = crate::utf16_len(&prefix.GetText(-1)?.to_string());
                        state.selection = Some((start, crate::utf16_len(&selected)));
                        state.selected_text = Some(selected);
                    }
                }
                return Ok(state);
            }
            if let Ok(value) = self
                .element
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            {
                return Ok(FieldState {
                    text: Some(value.CurrentValue()?.to_string()),
                    ..Default::default()
                });
            }
            Ok(FieldState::default())
        }
    }

    /// Selects `expected`, which should sit at UTF-16 offset `start`, so the next paste replaces it.
    /// UIA "characters" are provider-defined (CRLF and surrogate pairs vary), so the selection is
    /// verified, falling back to the exact match nearest `start`. Never selects anything else.
    pub fn select(&self, start: usize, expected: &str) -> Result<()> {
        unsafe {
            let pattern = self
                .text_pattern()
                .ok_or_else(|| anyhow!("field has no text pattern"))?;
            let document = pattern.DocumentRange()?;
            let range = document.Clone()?;
            range.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &range,
                TextPatternRangeEndpoint_Start,
            )?;
            range.Move(TextUnit_Character, start as i32)?;
            range.MoveEndpointByUnit(
                TextPatternRangeEndpoint_End,
                TextUnit_Character,
                crate::utf16_len(expected) as i32,
            )?;
            if range.GetText(-1)?.to_string() == expected {
                range.Select()?;
                return Ok(());
            }
            let needle = BSTR::from(expected);
            let mut best: Option<(usize, IUIAutomationTextRange)> = None;
            let mut search = document.Clone()?;
            for _ in 0..50 {
                let Ok(found) = search.FindText(&needle, false, false) else {
                    break;
                };
                let prefix = document.Clone()?;
                prefix.MoveEndpointByRange(
                    TextPatternRangeEndpoint_End,
                    &found,
                    TextPatternRangeEndpoint_Start,
                )?;
                let distance = crate::utf16_len(&prefix.GetText(-1)?.to_string()).abs_diff(start);
                if best.as_ref().is_none_or(|(d, _)| distance < *d) {
                    best = Some((distance, found.clone()));
                }
                let rest = document.Clone()?;
                rest.MoveEndpointByRange(
                    TextPatternRangeEndpoint_Start,
                    &found,
                    TextPatternRangeEndpoint_End,
                )?;
                if rest.GetText(1)?.is_empty() {
                    break;
                }
                search = rest;
            }
            let (_, found) =
                best.ok_or_else(|| anyhow!("the text to replace is no longer in the field"))?;
            found.Select()?;
            Ok(())
        }
    }

    /// Whether this element still has keyboard focus.
    pub fn is_focused(&self) -> bool {
        unsafe {
            self.automation
                .GetFocusedElement()
                .and_then(|focused| self.automation.CompareElements(&focused, &self.element))
                .map(|same| same.as_bool())
                .unwrap_or(false)
        }
    }
}

fn app_context() -> AppContext {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return AppContext::default();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut title = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title);
        let exe = process_path(pid);
        let target_elevated = process_elevated(pid).unwrap_or(true);
        let self_elevated = process_elevated(std::process::id()).unwrap_or(false);
        AppContext {
            app_name: exe.as_deref().and_then(|p| {
                std::path::Path::new(p)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            }),
            app_id: exe,
            pid: Some(pid as i32),
            window_title: Some(String::from_utf16_lossy(&title[..len.max(0) as usize])),
            is_elevated: target_elevated && !self_elevated,
        }
    }
}

/// Whether a process runs elevated. `None` (access denied) usually means it does.
fn process_elevated(pid: u32) -> Option<bool> {
    unsafe {
        let own = pid == std::process::id();
        let process = if own {
            GetCurrentProcess()
        } else {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        if !own {
            let _ = CloseHandle(process);
        }
        opened.ok()?;
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        let _ = CloseHandle(token);
        result.ok()?;
        Some(elevation.TokenIsElevated != 0)
    }
}

fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

#[allow(non_upper_case_globals)]
fn control_type_name(id: i32) -> String {
    let name = match UIA_CONTROLTYPE_ID(id) {
        UIA_EditControlTypeId => "Edit",
        UIA_DocumentControlTypeId => "Document",
        UIA_TextControlTypeId => "Text",
        UIA_ComboBoxControlTypeId => "ComboBox",
        UIA_PaneControlTypeId => "Pane",
        UIA_GroupControlTypeId => "Group",
        UIA_CustomControlTypeId => "Custom",
        UIA_WindowControlTypeId => "Window",
        _ => return format!("ControlType({id})"),
    };
    name.to_string()
}

// ---------------------------------------------------------------------------------------------
// Keyboard
// ---------------------------------------------------------------------------------------------

pub fn modifiers_down() -> bool {
    [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
        .iter()
        .any(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000 != 0)
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: crate::INJECTED_MARKER,
            },
        },
    }
}

/// Sends Ctrl+<letter> as one atomic SendInput batch. Elevated targets drop it silently (UIPI);
/// check `AppContext::is_elevated` first.
pub fn send_shortcut(letter: char) -> Result<()> {
    let vk = VIRTUAL_KEY(letter.to_ascii_uppercase() as u16);
    let inputs = [
        key(VK_CONTROL, false),
        key(vk, false),
        key(vk, true),
        key(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!("SendInput delivered {sent} of {} events", inputs.len());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------------------------

/// Formats whose data is a GDI or private handle, not an HGLOBAL. Byte-based equivalents (CF_DIB) are kept.
fn is_handle_format(format: u32) -> bool {
    matches!(format, 2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E)
        || (0x200..=0x3FF).contains(&format)
}

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(Some(HWND::default())) }.is_ok() {
                return Ok(Self);
            }
            sleep(Duration::from_millis(10));
        }
        bail!("clipboard is locked by another app")
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

/// Every byte-based clipboard format the user had.
pub struct ClipboardSnapshot {
    formats: Vec<(u32, Vec<u8>)>,
}

impl ClipboardSnapshot {
    /// Has a time and size budget; over budget it fails and the caller skips the restore.
    pub fn capture() -> Result<Self> {
        let _guard = ClipboardGuard::open()?;
        let started = Instant::now();
        let mut total = 0usize;
        let mut formats = Vec::new();
        let mut format = 0;
        loop {
            if started.elapsed() > CAPTURE_TIME_BUDGET || total > CAPTURE_BYTE_BUDGET {
                bail!(
                    "clipboard too large to back up ({total} bytes in {} formats)",
                    formats.len()
                );
            }
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if is_handle_format(format) {
                continue;
            }
            if let Ok(handle) = unsafe { GetClipboardData(format) } {
                if let Some(bytes) = unsafe { global_bytes(handle) } {
                    total += bytes.len();
                    formats.push((format, bytes));
                }
            }
        }
        Ok(Self { formats })
    }

    pub fn format_count(&self) -> usize {
        self.formats.len()
    }

    pub fn restore(&self) -> Result<()> {
        let _guard = ClipboardGuard::open()?;
        unsafe { EmptyClipboard()? };
        let failed = self
            .formats
            .iter()
            .filter(|(format, bytes)| unsafe { set_global(*format, bytes) }.is_err())
            .count();
        if failed > 0 {
            bail!(
                "{failed} of {} clipboard formats could not be restored",
                self.formats.len()
            );
        }
        Ok(())
    }
}

unsafe fn global_bytes(handle: HANDLE) -> Option<Vec<u8>> {
    unsafe {
        let global = HGLOBAL(handle.0);
        let size = GlobalSize(global);
        let pointer = GlobalLock(global);
        if pointer.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(pointer as *const u8, size).to_vec();
        let _ = GlobalUnlock(global);
        Some(bytes)
    }
}

unsafe fn set_global(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let pointer = GlobalLock(global);
        if pointer.is_null() {
            let _ = GlobalFree(Some(global));
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer as *mut u8, bytes.len());
        let _ = GlobalUnlock(global);
        // On success the clipboard owns the memory; on failure it is still ours to free.
        if let Err(error) = SetClipboardData(format, Some(HANDLE(global.0 as *mut c_void))) {
            let _ = GlobalFree(Some(global));
            return Err(error).context("SetClipboardData");
        }
        Ok(())
    }
}

pub fn clipboard_sequence() -> u64 {
    unsafe { GetClipboardSequenceNumber() as u64 }
}

pub fn clipboard_text() -> Option<String> {
    let _guard = ClipboardGuard::open().ok()?;
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) }.ok()?;
    let bytes = unsafe { global_bytes(handle) }?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..end]))
}

/// Writes Duma's temporary text, marked so Win+V history, cloud clipboard and clipboard managers skip it.
pub fn set_clipboard_text(text: &str) -> Result<()> {
    let _guard = ClipboardGuard::open()?;
    unsafe { EmptyClipboard()? };
    let mut bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    bytes.extend_from_slice(&[0, 0]);
    unsafe {
        set_global(CF_UNICODETEXT.0 as u32, &bytes)?;
        let zero = 0u32.to_le_bytes();
        let _ = set_global(
            RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
            &zero,
        );
        let _ = set_global(
            RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
            &zero,
        );
        let _ = set_global(
            RegisterClipboardFormatW(w!("CanUploadToCloudClipboard")),
            &zero,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real clipboard (and restores it); run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn clipboard_round_trip_restores_previous_text() {
        let original = ClipboardSnapshot::capture().unwrap();
        set_clipboard_text("before").unwrap();
        let snapshot = ClipboardSnapshot::capture().unwrap();
        set_clipboard_text("during").unwrap();
        assert_eq!(clipboard_text().as_deref(), Some("during"));
        snapshot.restore().unwrap();
        assert_eq!(clipboard_text().as_deref(), Some("before"));
        original.restore().unwrap();
    }
}
