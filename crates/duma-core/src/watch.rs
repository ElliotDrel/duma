//! The edit watch: after Duma inserts text, follow the same field for a while and record what the user
//! changes in Duma's span. This is the main training signal for improving cleanup and edits.
//!
//! - The baseline is the field as it looks once the paste landed, so the app's own normalization of the
//!   paste (smart quotes, auto-formatting) is not counted as a user edit.
//! - When the field empties (a chat message was sent), the last span before the clear is kept.
//! - Every change is logged with a bounded window of raw text, so better diffing can be re-run later.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use duma_platform::FieldHandle;
use serde::Serialize;
use serde_json::json;

use crate::log::EventLog;
use crate::text::{common_prefix, common_suffix, track_span, utf16_len};

const POLL: Duration = Duration::from_millis(250);
const SLOW_POLL: Duration = Duration::from_secs(1);
/// A read slower than this switches to the slow poll (huge documents).
const SLOW_READ: Duration = Duration::from_millis(80);
const WINDOW_CHARS: usize = 300;
const MAX_LOGGED_SPAN: usize = 8000;

#[derive(Clone, Debug, Serialize)]
pub struct WatchResult {
    pub run_id: String,
    /// Duma's span as the app rendered it after the paste.
    pub baseline_span: Option<String>,
    pub final_span: Option<String>,
    /// UTF-16 offset of the final span in the field.
    pub final_start_utf16: Option<usize>,
    pub changed: bool,
    pub submitted: bool,
    pub reason: String,
    pub changes: usize,
    pub elapsed_ms: u64,
}

pub struct WatchHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<WatchResult>>,
}

impl WatchHandle {
    /// Stops the watch and returns its result (finalized on the watch thread).
    pub fn stop(mut self) -> Option<WatchResult> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().and_then(|t| t.join().ok())
    }
}

pub struct WatchSpec {
    pub run_id: String,
    pub field: FieldHandle,
    /// Field text immediately before the paste.
    pub before: String,
    /// The pasted text.
    pub output: String,
    /// Selection (UTF-16 start, length) the paste replaced, when known.
    pub replaced: Option<(usize, usize)>,
    pub duration: Duration,
}

/// Starts watching. `on_change` gets each new span (text, UTF-16 start); `on_finish` gets the result.
pub fn start(
    spec: WatchSpec,
    log: EventLog,
    on_change: impl Fn(&str, usize) + Send + 'static,
    on_finish: impl FnOnce(&WatchResult) + Send + 'static,
) -> WatchHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name("duma-edit-watch".into())
        .spawn(move || {
            let result = run(spec, &log, &thread_stop, on_change);
            log.event("edit_watch_finished", json!(result));
            on_finish(&result);
            result
        })
        .expect("watch thread");
    WatchHandle {
        stop,
        thread: Some(thread),
    }
}

fn window(text: &str, start: usize, end: usize) -> String {
    let before: Vec<char> = text[..start].chars().collect();
    let before: String = before[before.len().saturating_sub(WINDOW_CHARS)..]
        .iter()
        .collect();
    let after: String = text[end..].chars().take(WINDOW_CHARS).collect();
    format!("{before}⟦{}⟧{after}", truncate(&text[start..end]))
}

fn truncate(text: &str) -> String {
    text.chars().take(MAX_LOGGED_SPAN).collect()
}

fn run(
    spec: WatchSpec,
    log: &EventLog,
    stop: &AtomicBool,
    on_change: impl Fn(&str, usize),
) -> WatchResult {
    let started = Instant::now();
    let mut result = WatchResult {
        run_id: spec.run_id.clone(),
        baseline_span: None,
        final_span: None,
        final_start_utf16: None,
        changed: false,
        submitted: false,
        reason: "timeout".into(),
        changes: 0,
        elapsed_ms: 0,
    };
    // Byte bounds of the untouched text around the replaced range in `before`.
    let (prefix_bound, suffix_bound) = match spec.replaced {
        Some((start, len)) => {
            let a = crate::text::utf16_to_byte(&spec.before, start);
            let b = crate::text::utf16_to_byte(&spec.before, start + len);
            (a, spec.before.len() - b.max(a))
        }
        None => (usize::MAX, usize::MAX),
    };

    let mut baseline: Option<(String, usize, usize)> = None; // (text, prefix, suffix)
    let mut last_text = spec.before.clone();
    let mut last_span: Option<String> = None;
    let mut poll = POLL;
    let mut failures = 0;

    while started.elapsed() < spec.duration {
        if stop.load(Ordering::SeqCst) {
            result.reason = "stopped".into();
            break;
        }
        std::thread::sleep(poll);
        let read_started = Instant::now();
        let text = match spec.field.read() {
            Ok(state) => match state.text {
                Some(text) => text,
                None => {
                    result.reason = "unreadable".into();
                    break;
                }
            },
            Err(_) => {
                failures += 1;
                if failures >= 3 {
                    result.reason = "field_gone".into();
                    break;
                }
                continue;
            }
        };
        failures = 0;
        if read_started.elapsed() > SLOW_READ {
            poll = SLOW_POLL;
        }
        if text == last_text {
            continue;
        }
        let t_ms = started.elapsed().as_millis() as u64;

        let Some((base, prefix, suffix)) = &baseline else {
            // First change: the paste landed. Locate Duma's span against the pre-paste text.
            let prefix = common_prefix(&spec.before, &text).min(prefix_bound);
            let suffix = common_suffix(&spec.before, &text)
                .min(suffix_bound)
                .min(text.len() - prefix);
            let span = text[prefix..text.len() - suffix].to_string();
            log.event(
                "edit_watch_baseline",
                json!({
                    "run_id": spec.run_id, "t_ms": t_ms, "span": truncate(&span),
                    "normalized_by_app": span != spec.output, "field_len": text.len(),
                }),
            );
            on_change(&span, utf16_len(&text[..prefix]));
            result.final_start_utf16 = Some(utf16_len(&text[..prefix]));
            result.baseline_span = Some(span.clone());
            last_span = Some(span);
            baseline = Some((text.clone(), prefix, suffix));
            last_text = text;
            continue;
        };

        if text.trim().is_empty() {
            result.submitted = true;
            result.reason = "field_cleared".into();
            log.event(
                "edit_watch_cleared",
                json!({ "run_id": spec.run_id, "t_ms": t_ms }),
            );
            break;
        }
        let (start, end, outside_changed) = track_span(base, *prefix, *suffix, &text);
        let span = text[start..end].to_string();
        if last_span.as_deref() != Some(span.as_str()) {
            result.changes += 1;
            log.event(
                "edit_watch_change",
                json!({
                    "run_id": spec.run_id, "t_ms": t_ms, "span": truncate(&span),
                    "window": window(&text, start, end), "outside_changed": outside_changed, "field_len": text.len(),
                }),
            );
            on_change(&span, utf16_len(&text[..start]));
            result.final_start_utf16 = Some(utf16_len(&text[..start]));
            last_span = Some(span);
        }
        last_text = text;
    }

    result.final_span = last_span;
    result.changed = result.baseline_span.is_some() && result.final_span != result.baseline_span;
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    result
}
