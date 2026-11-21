//! The engine: a hotkey-driven state machine plus the jobs it starts.
//!
//! The engine thread only reacts to events (never blocks on the network), so presses are timestamped,
//! a cancel key works at any time and stale presses are dropped. Slow work runs on a job thread:
//!
//! - **Dictate** (hold, or tap to toggle): stream audio to transcription while speaking; on release clean
//!   up the transcript and paste. With text selected, the speech is an instruction for that text.
//! - **Edit**: speak an instruction for the selection, or for Duma's last output when nothing is selected.
//! - **Preset**: run a saved prompt on the selection or the last output.
//! - **Undo**: put the previous version of the last output back.
//! - **Paste last**: paste the last result again (recovery when a paste could not land).
//!
//! Every run writes the audio first (so nothing is lost), then one detailed `run_completed` event, and
//! store writes are mirrored to the log.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use duma_openai::file_transcribe::FileTranscript;
use duma_openai::responses::{ResponseOutcome, ResponseRequest, ResponsesClient};
use duma_openai::transcribe::{self, Connection, Transcript};
use duma_platform::hotkeys::{HotkeyEvent, Hotkeys};
use duma_platform::{FieldHandle, Focus, InsertReport};
use serde::Serialize;
use serde_json::{Value, json};

use crate::log::{EventLog, now_rfc3339};
use crate::paths::{Channel, Paths};
use crate::prompts::{self, PromptContext};
use crate::settings::{ModelSettings, Preset, SelectionMode, Settings};
use crate::store::{NewVersion, Store, ThreadSummary, Version};
use crate::text;
use crate::watch::{self, WatchHandle, WatchSpec};

const JOB_DONE: &str = "__job_done";
/// Selections shorter than this are too ambiguous to match to an earlier thread by text.
const MIN_THREAD_MATCH_CHARS: usize = 12;
const LINEAGE_LIMIT: usize = 8;
/// Apps where Ctrl+C means "interrupt", so the clipboard selection fallback must not run.
const TERMINALS: &[&str] = &[
    "windowsterminal",
    "powershell",
    "pwsh",
    "cmd",
    "wezterm",
    "alacritty",
    "kitty",
    "terminal",
    "iterm",
    "ghostty",
    "warp",
];

/// What the overlay and main window show.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Listening {
        action: String,
        hands_free: bool,
    },
    Partial {
        text: String,
    },
    Working {
        label: String,
    },
    Done {
        action: String,
        text: String,
        ms: u64,
    },
    Message {
        level: String,
        text: String,
    },
    Idle,
    ThreadsChanged,
}

pub type UiSink = Arc<dyn Fn(UiEvent) + Send + Sync>;

/// Duma's most recent output in a field, kept current by the edit watch.
#[derive(Clone)]
struct LastOutput {
    thread_id: String,
    version_id: String,
    text: String,
    start_utf16: Option<usize>,
}

struct Inner {
    paths: Paths,
    log: EventLog,
    store: Mutex<Store>,
    settings: RwLock<Settings>,
    api_key: RwLock<Option<String>>,
    rt: tokio::runtime::Runtime,
    client: ResponsesClient,
    ui: UiSink,
    last_output: Mutex<Option<LastOutput>>,
    last_result: Mutex<Option<String>>,
    watch: Mutex<Option<WatchHandle>>,
    /// Where the last paste landed, until the run decides whether to watch it.
    pending_watch: Mutex<Option<PendingWatch>>,
    hotkeys: Mutex<Option<Hotkeys>>,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
    events: Sender<HotkeyEvent>,
}

// ---------------------------------------------------------------------------------------------
// Run record: one detailed log event per action
// ---------------------------------------------------------------------------------------------

#[derive(Default, Serialize)]
struct ModelCall {
    purpose: String,
    prompt_hash: String,
    outcome: ResponseOutcome,
}

#[derive(Default, Serialize)]
struct Run {
    run_id: String,
    action: String,
    hotkey: String,
    started_at: String,
    #[serde(skip)]
    t0: Option<Instant>,
    /// Milliseconds from the key press.
    timings: BTreeMap<String, u64>,
    hands_free: bool,
    app: Option<duma_platform::AppContext>,
    field: Option<duma_platform::FieldInfo>,
    /// Bounded field text around the cursor at the key press.
    field_context: Option<Value>,
    excluded_app: bool,
    recording: Option<Value>,
    audio_file: Option<String>,
    transcript: Option<Transcript>,
    fallback_transcript: Option<FileTranscript>,
    spoken_text: Option<String>,
    target: Option<String>,
    thread_id: Option<String>,
    parent_version_id: Option<String>,
    model_calls: Vec<ModelCall>,
    guard: Option<String>,
    output_text: Option<String>,
    insert: Option<InsertReport>,
    inserted: bool,
    focus_at_paste: Option<duma_platform::AppContext>,
    version_id: Option<String>,
    cancelled: bool,
    error: Option<String>,
    settings: Option<Value>,
}

impl Run {
    fn new(action: &str, hotkey: &str, t0: Instant) -> Self {
        Self {
            run_id: uuid::Uuid::now_v7().to_string(),
            action: action.into(),
            hotkey: hotkey.into(),
            started_at: now_rfc3339(),
            t0: Some(t0),
            ..Default::default()
        }
    }

    fn mark(&mut self, stage: &str) {
        if let Some(t0) = self.t0 {
            self.timings
                .insert(stage.into(), t0.elapsed().as_millis() as u64);
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.t0.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Spoken {
    Dictate,
    Edit,
}

struct Recording {
    kind: Spoken,
    key: String,
    pressed_at: Instant,
    hands_free: bool,
    run: Run,
    mic: duma_platform::audio::Recording,
    transcript: tokio::task::JoinHandle<Transcript>,
    focus: std::thread::JoinHandle<Focus>,
}

enum State {
    Idle,
    Recording(Box<Recording>),
    Working(Arc<AtomicBool>),
}

impl Engine {
    /// Starts the engine, its hotkeys and its runtime.
    pub fn start(paths: Paths, ui: UiSink) -> Result<Self> {
        duma_openai::install_crypto_provider();
        paths.ensure()?;
        let log = EventLog::open(paths.logs(), Channel::current());
        let settings = Settings::load(&paths.settings());
        if !paths.settings().exists() {
            let _ = settings.save(&paths.settings());
        }
        let store = Store::open(&paths.database())?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("duma-net")
            .enable_all()
            .build()?;
        let (events, receiver) = channel();
        let inner = Arc::new(Inner {
            paths,
            log,
            store: Mutex::new(store),
            settings: RwLock::new(settings),
            api_key: RwLock::new(crate::secrets::api_key()),
            rt,
            client: ResponsesClient::new(),
            ui,
            last_output: Mutex::new(None),
            last_result: Mutex::new(None),
            watch: Mutex::new(None),
            pending_watch: Mutex::new(None),
            hotkeys: Mutex::new(None),
        });
        let engine = Self {
            inner: inner.clone(),
            events: events.clone(),
        };

        match Hotkeys::start(events.clone()) {
            Ok(hotkeys) => {
                *inner.hotkeys.lock().unwrap() = Some(hotkeys);
                engine.rebind(false);
            }
            Err(error) => {
                inner
                    .log
                    .event("hotkeys_unavailable", json!({ "error": error.to_string() }));
                inner.message(
                    "error",
                    &format!("Global shortcuts are unavailable: {error}"),
                );
            }
        }
        inner.log.event(
            "engine_started",
            json!({
                "data_dir": inner.paths.root, "api_key_set": inner.api_key.read().unwrap().is_some(),
                "accessibility_trusted": duma_platform::accessibility_trusted(),
                "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            }),
        );
        let loop_engine = engine.clone();
        std::thread::Builder::new()
            .name("duma-engine".into())
            .spawn(move || loop_engine.run_loop(receiver))?;
        Ok(engine)
    }

    fn rebind(&self, include_cancel: bool) {
        let bindings = self.inner.settings.read().unwrap().bindings(include_cancel);
        if let Some(hotkeys) = self.inner.hotkeys.lock().unwrap().as_ref() {
            let failed = hotkeys.set_bindings(&bindings);
            if !failed.is_empty() {
                self.inner
                    .log
                    .event("hotkeys_failed", json!({ "failed": failed }));
                let names: Vec<String> = failed.iter().map(|(n, e)| format!("{n} ({e})")).collect();
                self.inner.message(
                    "warning",
                    &format!(
                        "Some shortcuts could not be registered: {}",
                        names.join(", ")
                    ),
                );
            }
        }
    }

    fn run_loop(self, receiver: Receiver<HotkeyEvent>) {
        let mut state = State::Idle;
        for event in receiver {
            state = self.handle(state, event);
        }
    }

    fn handle(&self, state: State, event: HotkeyEvent) -> State {
        let tap_ms = self.inner.settings.read().unwrap().tap_toggle_ms;
        match state {
            State::Idle => {
                if !event.pressed {
                    return State::Idle;
                }
                match event.name.as_str() {
                    "dictate" => self.start_recording(Spoken::Dictate, event),
                    "edit" => self.start_recording(Spoken::Edit, event),
                    name if name == "undo"
                        || name == "paste_last"
                        || name.starts_with("preset:") =>
                    {
                        let cancel = Arc::new(AtomicBool::new(false));
                        self.rebind(true);
                        let engine = self.clone();
                        let job_cancel = cancel.clone();
                        let name = name.to_string();
                        std::thread::spawn(move || {
                            engine.run_job(|e| e.direct_action(&name, event.at, &job_cancel))
                        });
                        State::Working(cancel)
                    }
                    _ => State::Idle,
                }
            }
            State::Recording(mut recording) => {
                if event.name == "cancel" && event.pressed {
                    self.cancel_recording(*recording);
                    return State::Idle;
                }
                if event.name != recording.key {
                    return State::Recording(recording);
                }
                let held = event.at.duration_since(recording.pressed_at);
                if !event.pressed && !recording.hands_free && held < Duration::from_millis(tap_ms) {
                    recording.hands_free = true;
                    recording.run.hands_free = true;
                    self.inner.ui(UiEvent::Listening {
                        action: recording.run.action.clone(),
                        hands_free: true,
                    });
                    return State::Recording(recording);
                }
                if !event.pressed && recording.hands_free {
                    return State::Recording(recording);
                }
                self.finish_recording(*recording, event.at)
            }
            State::Working(cancel) => {
                if event.name == JOB_DONE {
                    self.rebind(false);
                    return State::Idle;
                }
                if event.name == "cancel" && event.pressed {
                    cancel.store(true, Ordering::SeqCst);
                    self.inner.log.event("cancel_requested", json!({}));
                } else if event.pressed {
                    self.inner
                        .log
                        .event("press_ignored_busy", json!({ "hotkey": event.name }));
                }
                State::Working(cancel)
            }
        }
    }

    fn start_recording(&self, kind: Spoken, event: HotkeyEvent) -> State {
        let inner = &self.inner;
        let action = match kind {
            Spoken::Dictate => "dictate",
            Spoken::Edit => "edit",
        };
        let mut run = Run::new(action, &event.name, event.at);
        let Some(api_key) = inner.api_key.read().unwrap().clone() else {
            inner.message("error", "Add your OpenAI API key in Duma's settings first.");
            inner.log.event(
                "run_blocked",
                json!({ "reason": "no_api_key", "action": action }),
            );
            return State::Idle;
        };
        let settings = inner.settings.read().unwrap().clone();

        // Mic first: it is the only thing the user is waiting on.
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let mic = match duma_platform::audio::start(
            settings.microphone.clone(),
            transcribe::SAMPLE_RATE,
            Box::new(move |chunk| {
                let _ = audio_tx.send(transcribe::to_pcm16(chunk));
            }),
        ) {
            Ok(mic) => mic,
            Err(error) => {
                inner.message("error", &format!("Couldn't open the microphone: {error}"));
                inner.log.event("run_failed", json!({ "run_id": run.run_id, "stage": "microphone", "error": error.to_string() }));
                return State::Idle;
            }
        };
        run.mark("mic_started");

        // Focus is a cross-process call; keep it off this thread.
        let focus = std::thread::spawn(duma_platform::focus);

        // Open the socket while the user talks; audio queues until it is ready.
        let config = settings.transcribe_config();
        let ui = inner.ui.clone();
        let key = api_key.clone();
        let transcript = inner.rt.spawn(async move {
            let connection = match Connection::open(&key, &config).await {
                Ok(connection) => connection,
                Err(record) => return record,
            };
            let partial = Arc::new(Mutex::new(String::new()));
            let session = connection.start(Box::new(move |delta| {
                let mut text = partial.lock().unwrap();
                text.push_str(delta);
                ui(UiEvent::Partial { text: text.clone() });
            }));
            while let Some(chunk) = audio_rx.recv().await {
                if !session.send_audio(&chunk) {
                    break;
                }
            }
            session.finish().await
        });
        let client = inner.client.clone();
        let warm_model = settings.cleanup.model.clone();
        inner
            .rt
            .spawn(async move { client.warm(&api_key, &warm_model).await });

        self.rebind(true);
        inner.ui(UiEvent::Listening {
            action: action.into(),
            hands_free: false,
        });
        State::Recording(Box::new(Recording {
            kind,
            key: event.name,
            pressed_at: event.at,
            hands_free: false,
            run,
            mic,
            transcript,
            focus,
        }))
    }

    fn cancel_recording(&self, recording: Recording) {
        let mut run = recording.run;
        run.cancelled = true;
        recording.transcript.abort();
        if let Ok((samples, stats)) = recording.mic.stop() {
            run.recording = serde_json::to_value(&stats).ok();
            run.audio_file = self.inner.save_audio(&run.run_id, &samples);
        }
        run.mark("cancelled");
        self.inner.log_run(&run);
        self.inner.ui(UiEvent::Idle);
        self.rebind(false);
    }

    fn finish_recording(&self, recording: Recording, released_at: Instant) -> State {
        let mut run = recording.run;
        run.timings.insert(
            "key_released".into(),
            released_at.duration_since(recording.pressed_at).as_millis() as u64,
        );
        let (samples, stats) = match recording.mic.stop() {
            Ok(result) => result,
            Err(error) => {
                recording.transcript.abort();
                run.error = Some(format!("microphone: {error}"));
                self.inner.log_run(&run);
                self.inner
                    .message("error", &format!("Recording failed: {error}"));
                self.rebind(false);
                return State::Idle;
            }
        };
        run.mark("mic_stopped");
        run.recording = serde_json::to_value(&stats).ok();
        // Durable first: the audio is on disk before anything can fail.
        run.audio_file = self.inner.save_audio(&run.run_id, &samples);
        self.inner.log.event(
            "run_pending",
            json!({ "run_id": run.run_id, "audio_file": run.audio_file }),
        );
        self.inner.ui(UiEvent::Working {
            label: "Transcribing".into(),
        });

        let cancel = Arc::new(AtomicBool::new(false));
        let job_cancel = cancel.clone();
        let engine = self.clone();
        let kind = recording.kind;
        let (transcript, focus) = (recording.transcript, recording.focus);
        std::thread::spawn(move || {
            engine
                .run_job(move |e| e.spoken_job(kind, run, samples, transcript, focus, &job_cancel))
        });
        State::Working(cancel)
    }

    /// Runs a job, reports failures, and always returns the engine to idle.
    fn run_job(&self, job: impl FnOnce(&Engine) -> Result<()>) {
        if let Err(error) = job(self) {
            self.inner.message("error", &error.to_string());
        }
        let _ = self.events.send(HotkeyEvent {
            name: JOB_DONE.into(),
            pressed: true,
            at: Instant::now(),
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------------------------

enum Target {
    Cursor,
    Selection(String),
    LastOutput(LastOutput),
}

impl Target {
    fn name(&self) -> &'static str {
        match self {
            Target::Cursor => "cursor",
            Target::Selection(_) => "selection",
            Target::LastOutput(_) => "last_output",
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Target::Cursor => None,
            Target::Selection(text) => Some(text),
            Target::LastOutput(last) => Some(&last.text),
        }
    }
}

impl Engine {
    fn spoken_job(
        &self,
        kind: Spoken,
        mut run: Run,
        samples: Vec<f32>,
        transcript: tokio::task::JoinHandle<Transcript>,
        focus: std::thread::JoinHandle<Focus>,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let inner = &self.inner;
        let settings = inner.settings.read().unwrap().clone();
        let api_key = inner.api_key.read().unwrap().clone().unwrap_or_default();

        let mut record = inner.rt.block_on(transcript).unwrap_or_default();
        run.mark("transcript_done");
        let mut spoken = record.text.trim().to_string();
        if record.error.is_some() && !record.too_short {
            inner.ui(UiEvent::Working {
                label: "Retrying transcription".into(),
            });
            let wav = wav_bytes(&samples);
            let prompt = (!settings.vocabulary.is_empty()).then(|| settings.vocabulary.join(", "));
            let fallback = inner.rt.block_on(inner.client.transcribe_wav(
                &api_key,
                &settings.fallback_transcription_model,
                wav,
                prompt,
            ));
            run.mark("fallback_transcript_done");
            if fallback.error.is_none() {
                spoken = fallback.text.trim().to_string();
            }
            run.fallback_transcript = Some(fallback);
        }
        record.text = record.text.trim().to_string();
        run.transcript = Some(record);

        let focus = focus.join().unwrap_or_default();
        run.mark("focus_ready");
        let excluded =
            settings.is_excluded(focus.app.app_name.as_deref(), focus.app.app_id.as_deref());
        self.note_focus(&mut run, &focus, excluded, &settings);

        if spoken.is_empty() {
            run.spoken_text = Some(String::new());
            run.error = run.transcript.as_ref().and_then(|t| t.error.clone());
            inner.log_run(&run);
            let message = if run.error.is_some() {
                "Transcription failed; the audio was saved."
            } else {
                "Didn't catch anything."
            };
            inner.message("info", message);
            return Ok(());
        }
        run.spoken_text = Some(spoken.clone());
        if cancel.load(Ordering::SeqCst) {
            run.cancelled = true;
            inner.log_run(&run);
            inner.ui(UiEvent::Idle);
            return Ok(());
        }

        let selection = if excluded {
            None
        } else {
            focus.state.selected_text.clone().filter(|s| !s.is_empty())
        };
        let result = match (kind, selection) {
            (Spoken::Dictate, Some(selection))
                if settings.selection_mode == SelectionMode::Instruct =>
            {
                self.instruct(
                    &mut run,
                    &settings,
                    &focus,
                    Target::Selection(selection),
                    &spoken,
                    excluded,
                    cancel,
                )
            }
            (Spoken::Dictate, _) => {
                self.dictate(&mut run, &settings, &focus, &spoken, excluded, cancel)
            }
            (Spoken::Edit, Some(selection)) => self.instruct(
                &mut run,
                &settings,
                &focus,
                Target::Selection(selection),
                &spoken,
                excluded,
                cancel,
            ),
            (Spoken::Edit, None) => {
                self.stop_watch();
                let target = self
                    .last_output_target()
                    .map(Target::LastOutput)
                    .unwrap_or(Target::Cursor);
                self.instruct(
                    &mut run, &settings, &focus, target, &spoken, excluded, cancel,
                )
            }
        };
        self.finish_run(run, result)
    }

    fn finish_run(&self, mut run: Run, result: Result<()>) -> Result<()> {
        run.mark("total");
        if let Err(error) = &result {
            run.error = Some(error.to_string());
        }
        self.inner.log_run(&run);
        if result.is_ok() && run.inserted {
            self.inner.ui(UiEvent::Done {
                action: run.action.clone(),
                text: run.output_text.clone().unwrap_or_default(),
                ms: run.elapsed_ms(),
            });
            self.inner.ui(UiEvent::ThreadsChanged);
        } else if result.is_ok() || run.cancelled {
            self.inner.ui(UiEvent::Idle);
        }
        result
    }

    fn note_focus(&self, run: &mut Run, focus: &Focus, excluded: bool, settings: &Settings) {
        run.app = Some(focus.app.clone());
        run.field = Some(focus.info.clone());
        run.excluded_app = excluded;
        if !excluded && let Some(text) = &focus.state.text {
            let (before, selected, after) = text::context_around(
                text,
                focus.state.selection,
                settings.capture.log_context_chars,
            );
            run.field_context = Some(json!({
                "before": before, "selected": selected, "after": after,
                "selection_utf16": focus.state.selection, "field_len_utf16": text::utf16_len(text),
            }));
        }
    }

    fn prompt_context(&self, focus: &Focus, excluded: bool, settings: &Settings) -> PromptContext {
        let before_cursor = (!excluded && settings.capture.model_context_chars > 0)
            .then_some(focus.state.text.as_deref())
            .flatten()
            .map(|t| {
                text::context_around(
                    t,
                    focus.state.selection,
                    settings.capture.model_context_chars,
                )
                .0
            });
        PromptContext {
            app_name: focus.app.app_name.clone(),
            window_title: (!excluded)
                .then(|| focus.app.window_title.clone())
                .flatten(),
            before_cursor,
            vocabulary: settings.vocabulary.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dictate(
        &self,
        run: &mut Run,
        settings: &Settings,
        focus: &Focus,
        spoken: &str,
        excluded: bool,
        cancel: &AtomicBool,
    ) -> Result<()> {
        run.target = Some("cursor".into());
        let words = spoken.split_whitespace().count();
        let mut output =
            if settings.rules_only_max_words > 0 && words <= settings.rules_only_max_words {
                run.guard = Some("rules_only".into());
                text::rules_cleanup(spoken)
            } else {
                self.inner.ui(UiEvent::Working {
                    label: "Cleaning up".into(),
                });
                let context = self.prompt_context(focus, excluded, settings);
                let (instructions, input) = prompts::dictation(settings, spoken, &context);
                let outcome = self.call_model(
                    run,
                    "dictation_cleanup",
                    &settings.cleanup,
                    &instructions,
                    input,
                    settings,
                );
                match outcome {
                    Some(cleaned) => {
                        let cleaned = text::strip_wrapping(&cleaned, spoken);
                        match text::dictation_guard(spoken, &cleaned) {
                            Ok(()) => cleaned,
                            Err(reason) => {
                                run.guard = Some(format!("rejected: {reason}"));
                                text::rules_cleanup(spoken)
                            }
                        }
                    }
                    None => {
                        run.guard = Some("model_failed: raw transcript".into());
                        text::rules_cleanup(spoken)
                    }
                }
            };
        if !excluded {
            let before = focus
                .state
                .text
                .as_deref()
                .map(|t| text::context_around(t, focus.state.selection, 1).0);
            output = text::join_with_before(before.as_deref(), &output);
        }
        if cancel.load(Ordering::SeqCst) {
            run.cancelled = true;
            return Ok(());
        }
        run.output_text = Some(output.clone());
        self.deliver(run, settings, focus, &Target::Cursor, &output, excluded)?;
        if run.inserted {
            let store = self.inner.store.lock().unwrap();
            let thread = store.create_thread(
                focus.app.app_name.as_deref(),
                focus.app.app_id.as_deref(),
                focus.app.window_title.as_deref(),
            )?;
            let version = store.add_version(NewVersion {
                thread_id: &thread,
                parent_id: None,
                kind: "dictation",
                text: output.trim_start(),
                source: Some(spoken),
                run_id: Some(&run.run_id),
                model: Some(&settings.cleanup.model),
            })?;
            drop(store);
            self.after_insert(run, settings, focus, &thread, &version, excluded);
        }
        Ok(())
    }

    /// Finds or creates the thread for `target` and returns (thread id, parent version).
    fn resolve_thread(
        &self,
        run: &mut Run,
        focus: &Focus,
        target: &Target,
    ) -> Result<(String, Option<Version>)> {
        let store = self.inner.store.lock().unwrap();
        let resolved = match target {
            Target::LastOutput(last) => (last.thread_id.clone(), store.head(&last.thread_id)?),
            Target::Selection(selection) => {
                let last = self.inner.last_output.lock().unwrap().clone();
                if let Some(last) = last.filter(|l| l.text.trim() == selection.trim()) {
                    (last.thread_id.clone(), store.head(&last.thread_id)?)
                } else if let Some(found) = (selection.chars().count() >= MIN_THREAD_MATCH_CHARS)
                    .then(|| store.find_version_by_text(selection).ok().flatten())
                    .flatten()
                {
                    (found.thread_id.clone(), Some(found))
                } else {
                    let thread = store.create_thread(
                        focus.app.app_name.as_deref(),
                        focus.app.app_id.as_deref(),
                        focus.app.window_title.as_deref(),
                    )?;
                    let original = store.add_version(NewVersion {
                        thread_id: &thread,
                        parent_id: None,
                        kind: "original",
                        text: selection,
                        source: None,
                        run_id: Some(&run.run_id),
                        model: None,
                    })?;
                    self.inner.log.event("version_created", json!(original));
                    (thread, Some(original))
                }
            }
            Target::Cursor => (
                store.create_thread(
                    focus.app.app_name.as_deref(),
                    focus.app.app_id.as_deref(),
                    focus.app.window_title.as_deref(),
                )?,
                None,
            ),
        };
        run.thread_id = Some(resolved.0.clone());
        run.parent_version_id = resolved.1.as_ref().map(|v| v.id.clone());
        Ok(resolved)
    }

    #[allow(clippy::too_many_arguments)]
    fn instruct(
        &self,
        run: &mut Run,
        settings: &Settings,
        focus: &Focus,
        target: Target,
        instruction: &str,
        excluded: bool,
        cancel: &AtomicBool,
    ) -> Result<()> {
        run.action = format!("{}:instruction", run.action);
        run.target = Some(target.name().into());
        let (thread_id, parent) = self.resolve_thread(run, focus, &target)?;
        let lineage = match &parent {
            Some(parent) => {
                let store = self.inner.store.lock().unwrap();
                let mut chain = store.lineage(&thread_id, LINEAGE_LIMIT)?;
                if chain.last().map(|v| &v.id) != Some(&parent.id) {
                    chain = vec![parent.clone()];
                }
                chain
            }
            None => Vec::new(),
        };
        self.inner.ui(UiEvent::Working {
            label: "Editing".into(),
        });
        let context = self.prompt_context(focus, excluded, settings);
        let (instructions, input) =
            prompts::edit(settings, &lineage, target.text(), instruction, &context);
        let output = self
            .call_model(
                run,
                "edit_instruction",
                &settings.edit,
                &instructions,
                input,
                settings,
            )
            .map(|o| text::strip_wrapping(&o, target.text().unwrap_or(instruction)))
            .ok_or_else(|| anyhow!("The edit model failed; nothing was changed."))?;
        if cancel.load(Ordering::SeqCst) {
            run.cancelled = true;
            return Ok(());
        }
        run.output_text = Some(output.clone());
        self.deliver(run, settings, focus, &target, &output, excluded)?;
        if run.inserted {
            let version = self.inner.store.lock().unwrap().add_version(NewVersion {
                thread_id: &thread_id,
                parent_id: parent.as_ref().map(|p| p.id.as_str()),
                kind: "instruction",
                text: &output,
                source: Some(instruction),
                run_id: Some(&run.run_id),
                model: Some(&settings.edit.model),
            })?;
            self.after_insert(run, settings, focus, &thread_id, &version, excluded);
        }
        Ok(())
    }

    /// Preset, undo and paste-last: no recording.
    fn direct_action(&self, name: &str, at: Instant, cancel: &AtomicBool) -> Result<()> {
        let inner = &self.inner;
        let settings = inner.settings.read().unwrap().clone();
        let mut run = Run::new(name, name, at);
        let result = (|| -> Result<()> {
            if name == "paste_last" {
                let text = inner
                    .last_result
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| anyhow!("Nothing to paste yet."))?;
                let focus = duma_platform::focus();
                run.app = Some(focus.app.clone());
                run.output_text = Some(text.clone());
                return self.deliver(&mut run, &settings, &focus, &Target::Cursor, &text, false);
            }
            // Use the user's latest manual edits as the base before changing anything.
            self.stop_watch();
            let focus = duma_platform::focus();
            run.mark("focus_ready");
            let excluded =
                settings.is_excluded(focus.app.app_name.as_deref(), focus.app.app_id.as_deref());
            self.note_focus(&mut run, &focus, excluded, &settings);

            if name == "undo" {
                return self.undo(&mut run, &settings, &focus, excluded);
            }
            let preset_id = name.trim_start_matches("preset:");
            let preset = settings
                .presets
                .iter()
                .find(|p| p.id == preset_id)
                .cloned()
                .ok_or_else(|| anyhow!("Unknown preset {preset_id}"))?;
            let target = self.selection_target(&focus, excluded)?;
            self.run_preset(
                &mut run, &settings, &focus, &preset, target, excluded, cancel,
            )
        })();
        self.finish_run(run, result)
    }

    /// The selection (via accessibility, else the clipboard where safe), else Duma's last output.
    fn selection_target(&self, focus: &Focus, excluded: bool) -> Result<Target> {
        if let Some(selection) = focus.state.selected_text.clone().filter(|s| !s.is_empty()) {
            return Ok(Target::Selection(selection));
        }
        let field_unreadable = focus.state.text.is_none();
        let app = format!(
            "{} {}",
            focus.app.app_name.as_deref().unwrap_or(""),
            focus.app.app_id.as_deref().unwrap_or("")
        )
        .to_lowercase();
        let terminal = TERMINALS.iter().any(|t| app.contains(t));
        if field_unreadable
            && !terminal
            && !excluded
            && let Some(selection) = duma_platform::capture_selection_via_clipboard()?
        {
            return Ok(Target::Selection(selection));
        }
        self.last_output_target()
            .map(Target::LastOutput)
            .ok_or_else(|| anyhow!("Select some text first."))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_preset(
        &self,
        run: &mut Run,
        settings: &Settings,
        focus: &Focus,
        preset: &Preset,
        target: Target,
        excluded: bool,
        cancel: &AtomicBool,
    ) -> Result<()> {
        run.target = Some(target.name().into());
        let (thread_id, parent) = self.resolve_thread(run, focus, &target)?;
        let source = target.text().unwrap_or_default().to_string();
        self.inner.ui(UiEvent::Working {
            label: preset.name.clone(),
        });
        let (instructions, input) = prompts::preset(preset, &source);
        let model = ModelSettings {
            model: preset.model.clone().unwrap_or(settings.edit.model.clone()),
            ..settings.edit.clone()
        };
        let output = self
            .call_model(
                run,
                &format!("preset:{}", preset.id),
                &model,
                &instructions,
                input,
                settings,
            )
            .map(|o| text::strip_wrapping(&o, &source))
            .ok_or_else(|| anyhow!("{} failed; nothing was changed.", preset.name))?;
        if cancel.load(Ordering::SeqCst) {
            run.cancelled = true;
            return Ok(());
        }
        run.output_text = Some(output.clone());
        if output == source {
            self.inner
                .message("info", &format!("{}: no changes needed.", preset.name));
            return Ok(());
        }
        self.deliver(run, settings, focus, &target, &output, excluded)?;
        if run.inserted {
            let version = self.inner.store.lock().unwrap().add_version(NewVersion {
                thread_id: &thread_id,
                parent_id: parent.as_ref().map(|p| p.id.as_str()),
                kind: "preset",
                text: &output,
                source: Some(&preset.name),
                run_id: Some(&run.run_id),
                model: Some(&model.model),
            })?;
            self.after_insert(run, settings, focus, &thread_id, &version, excluded);
        }
        Ok(())
    }

    fn undo(
        &self,
        run: &mut Run,
        settings: &Settings,
        focus: &Focus,
        excluded: bool,
    ) -> Result<()> {
        let last = self
            .last_output_target()
            .ok_or_else(|| anyhow!("Nothing to undo."))?;
        let store = self.inner.store.lock().unwrap();
        let head = store
            .head(&last.thread_id)?
            .ok_or_else(|| anyhow!("Nothing to undo."))?;
        let parent_id = head
            .parent_id
            .clone()
            .ok_or_else(|| anyhow!("This is the first version; nothing to undo."))?;
        let parent = store
            .version(&parent_id)?
            .ok_or_else(|| anyhow!("The previous version is missing."))?;
        drop(store);
        run.target = Some("last_output".into());
        run.thread_id = Some(last.thread_id.clone());
        run.parent_version_id = Some(parent.id.clone());
        run.output_text = Some(parent.text.clone());
        self.deliver(
            run,
            settings,
            focus,
            &Target::LastOutput(last.clone()),
            &parent.text,
            excluded,
        )?;
        if run.inserted {
            self.inner
                .store
                .lock()
                .unwrap()
                .set_head(&last.thread_id, &parent.id)?;
            self.inner.log.event(
                "thread_head_moved",
                json!({ "thread_id": last.thread_id, "head_id": parent.id, "run_id": run.run_id }),
            );
            self.after_insert(run, settings, focus, &last.thread_id, &parent, excluded);
        }
        Ok(())
    }

    fn last_output_target(&self) -> Option<LastOutput> {
        self.inner.last_output.lock().unwrap().clone()
    }

    fn stop_watch(&self) {
        let watch = self.inner.watch.lock().unwrap().take();
        if let Some(watch) = watch {
            watch.stop();
        }
    }

    /// Calls a model and records the call. Returns the text, or `None` on failure.
    fn call_model(
        &self,
        run: &mut Run,
        purpose: &str,
        model: &ModelSettings,
        instructions: &str,
        input: String,
        settings: &Settings,
    ) -> Option<String> {
        let api_key = self
            .inner
            .api_key
            .read()
            .unwrap()
            .clone()
            .unwrap_or_default();
        let request = ResponseRequest {
            model: model.model.clone(),
            instructions: instructions.to_string(),
            input,
            reasoning_effort: model.reasoning_effort.clone(),
            verbosity: model.verbosity.clone(),
            service_tier: None,
            store: settings.openai_store,
            timeout_ms: model.timeout_ms,
        };
        run.mark(&format!("{purpose}_sent"));
        let outcome = self
            .inner
            .rt
            .block_on(self.inner.client.create(&api_key, &request));
        run.mark(&format!("{purpose}_done"));
        let text = outcome.error.is_none().then(|| outcome.text.clone());
        run.model_calls.push(ModelCall {
            purpose: purpose.into(),
            prompt_hash: prompts::hash(instructions),
            outcome,
        });
        text
    }

    /// Puts `output` into the target field with the paste guards.
    fn deliver(
        &self,
        run: &mut Run,
        settings: &Settings,
        focus: &Focus,
        target: &Target,
        output: &str,
        excluded: bool,
    ) -> Result<()> {
        let _ = settings;
        let inner = &self.inner;
        self.stop_watch();
        *inner.last_result.lock().unwrap() = Some(output.to_string());
        let now = duma_platform::focus();
        run.focus_at_paste = Some(now.app.clone());
        if now.app.is_elevated {
            duma_platform::set_clipboard_text(output)?;
            bail!(
                "This app runs as administrator, so Duma can't type into it. The text is on your clipboard."
            );
        }
        if now.info.is_secure {
            bail!("The focused field is a password field; Duma won't paste there.");
        }
        let field = match target {
            Target::Cursor => now.field.clone(),
            Target::Selection(_) | Target::LastOutput(_) => {
                let original = focus.field.clone();
                if !original.as_ref().is_some_and(FieldHandle::is_focused) {
                    bail!(
                        "The field changed before the result was ready. Press paste-last to insert it."
                    );
                }
                original
            }
        };
        if let Target::LastOutput(last) = target {
            let last = self
                .last_output_target()
                .filter(|l| l.thread_id == last.thread_id)
                .unwrap_or_else(|| last.clone());
            let field = field
                .as_ref()
                .ok_or_else(|| anyhow!("Select the text to change first."))?;
            let start = last.start_utf16.unwrap_or(0);
            field.select(start, &last.text).map_err(|e| {
                anyhow!("Couldn't find Duma's last text ({e}). Select it and try again.")
            })?;
        }
        let before = field.as_ref().and_then(|f| f.read().ok());
        run.mark("paste_started");
        let report = duma_platform::insert_text(output, field.as_ref())?;
        run.mark("paste_sent");
        if let Some(observed) = report.paste_observed_ms {
            run.timings.insert(
                "paste_observed".into(),
                run.timings["paste_started"] + observed,
            );
        }
        run.insert = Some(report);
        run.inserted = true;

        // Remember where the output landed for refine/undo, and watch it.
        if let (Some(field), Some(before)) = (field, before) {
            let replaced = before
                .selection
                .or_else(|| before.text.as_ref().map(|t| (text::utf16_len(t), 0)));
            self.inner
                .pending_watch
                .lock()
                .unwrap()
                .replace(PendingWatch {
                    field,
                    before: before.text.unwrap_or_default(),
                    replaced,
                    excluded,
                });
        }
        Ok(())
    }

    fn after_insert(
        &self,
        run: &mut Run,
        settings: &Settings,
        focus: &Focus,
        thread_id: &str,
        version: &Version,
        excluded: bool,
    ) {
        let _ = focus;
        run.version_id = Some(version.id.clone());
        self.inner.log.event("version_created", json!(version));
        let pending = self.inner.pending_watch.lock().unwrap().take();
        let start = pending.as_ref().and_then(|p| p.replaced.map(|(s, _)| s));
        *self.inner.last_output.lock().unwrap() = Some(LastOutput {
            thread_id: thread_id.to_string(),
            version_id: version.id.clone(),
            text: version.text.clone(),
            start_utf16: start,
        });
        let Some(pending) = pending else { return };
        if !settings.capture.watch_edits
            || excluded
            || pending.excluded
            || pending.before.is_empty() && pending.replaced.is_none()
        {
            return;
        }
        self.stop_watch();
        let spec = WatchSpec {
            run_id: run.run_id.clone(),
            field: pending.field,
            before: pending.before,
            output: run.output_text.clone().unwrap_or_default(),
            replaced: pending.replaced,
            duration: Duration::from_secs(settings.capture.watch_seconds),
        };
        let change_inner = self.inner.clone();
        let finish_inner = self.inner.clone();
        let version_id = version.id.clone();
        let thread = thread_id.to_string();
        let finish_thread = thread_id.to_string();
        let handle = watch::start(
            spec,
            self.inner.log.clone(),
            move |span, start| {
                if let Some(last) = change_inner.last_output.lock().unwrap().as_mut()
                    && (last.version_id == version_id || last.thread_id == thread)
                {
                    last.text = span.to_string();
                    last.start_utf16 = Some(start);
                }
            },
            move |result| {
                let Some(final_span) = result.final_span.clone().filter(|_| result.changed) else {
                    return;
                };
                if !final_span.trim().is_empty() {
                    finish_inner.record_manual_edit(&result.run_id, &finish_thread, &final_span);
                }
            },
        );
        *self.inner.watch.lock().unwrap() = Some(handle);
    }
}

struct PendingWatch {
    field: FieldHandle,
    before: String,
    replaced: Option<(usize, usize)>,
    excluded: bool,
}

// ---------------------------------------------------------------------------------------------
// Inner helpers
// ---------------------------------------------------------------------------------------------

impl Inner {
    fn ui(&self, event: UiEvent) {
        (self.ui)(event);
    }

    fn message(&self, level: &str, text: &str) {
        self.ui(UiEvent::Message {
            level: level.into(),
            text: text.into(),
        });
    }

    fn log_run(&self, run: &Run) {
        let mut value = serde_json::to_value(run).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("settings".into(), json!(self.settings_snapshot()));
        }
        self.log.event("run_completed", value);
    }

    fn settings_snapshot(&self) -> Value {
        let settings = self.settings.read().unwrap();
        json!({
            "transcription": settings.transcription,
            "cleanup": settings.cleanup,
            "edit": settings.edit,
            "dictation_prompt_hash": prompts::hash(&settings.dictation_prompt),
            "edit_prompt_hash": prompts::hash(&settings.edit_prompt),
            "selection_mode": settings.selection_mode,
            "vocabulary_size": settings.vocabulary.len(),
            "openai_store": settings.openai_store,
        })
    }

    fn save_audio(&self, run_id: &str, samples: &[f32]) -> Option<String> {
        if !self.settings.read().unwrap().capture.save_audio || samples.is_empty() {
            return None;
        }
        let path = self.paths.audio().join(format!("{run_id}.wav"));
        std::fs::write(&path, wav_bytes(samples)).ok()?;
        Some(path.to_string_lossy().into_owned())
    }

    /// The edit watch saw the user change Duma's text: store it as the thread's new head.
    fn record_manual_edit(&self, run_id: &str, thread_id: &str, text: &str) {
        let store = self.store.lock().unwrap();
        let Ok(Some(head)) = store.head(thread_id) else {
            return;
        };
        if head.text == text {
            return;
        }
        if let Ok(version) = store.add_version(NewVersion {
            thread_id,
            parent_id: Some(&head.id),
            kind: "manual_edit",
            text,
            source: None,
            run_id: Some(run_id),
            model: None,
        }) {
            self.log.event("version_created", json!(version));
            if let Some(current) = self
                .last_output
                .lock()
                .unwrap()
                .as_mut()
                .filter(|c| c.thread_id == thread_id)
            {
                current.version_id = version.id.clone();
                current.text = version.text.clone();
            }
            self.ui(UiEvent::ThreadsChanged);
        }
    }
}

fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: transcribe::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    if let Ok(mut writer) = hound::WavWriter::new(&mut cursor, spec) {
        for sample in transcribe::to_pcm16(samples) {
            let _ = writer.write_sample(sample);
        }
        let _ = writer.finalize();
    }
    cursor.into_inner()
}

// ---------------------------------------------------------------------------------------------
// Public API for the UI
// ---------------------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Status {
    pub api_key: Option<String>,
    pub accessibility_trusted: bool,
    pub data_dir: String,
    pub logs_dir: String,
    pub channel: &'static str,
    pub version: &'static str,
}

impl Engine {
    pub fn settings(&self) -> Settings {
        self.inner.settings.read().unwrap().clone()
    }

    pub fn set_settings(&self, settings: Settings) -> Result<()> {
        settings.save(&self.inner.paths.settings())?;
        *self.inner.settings.write().unwrap() = settings;
        self.inner.log.event(
            "settings_changed",
            json!({ "settings": self.inner.settings_snapshot() }),
        );
        self.rebind(false);
        Ok(())
    }

    pub fn set_api_key(&self, key: &str) -> Result<()> {
        crate::secrets::set_api_key(key)?;
        let key = key.trim();
        *self.inner.api_key.write().unwrap() = (!key.is_empty()).then(|| key.to_string());
        self.inner
            .log
            .event("api_key_changed", json!({ "set": !key.is_empty() }));
        Ok(())
    }

    pub fn status(&self) -> Status {
        Status {
            api_key: self
                .inner
                .api_key
                .read()
                .unwrap()
                .as_deref()
                .map(crate::secrets::masked),
            accessibility_trusted: duma_platform::accessibility_trusted(),
            data_dir: self.inner.paths.root.to_string_lossy().into_owned(),
            logs_dir: self.inner.paths.logs().to_string_lossy().into_owned(),
            channel: Channel::current().as_str(),
            version: crate::APP_VERSION,
        }
    }

    pub fn threads(&self, limit: usize, offset: usize) -> Result<Vec<ThreadSummary>> {
        self.inner.store.lock().unwrap().threads(limit, offset)
    }

    pub fn thread(&self, id: &str) -> Result<Vec<Version>> {
        self.inner.store.lock().unwrap().versions(id)
    }

    pub fn log_event(&self, event: &str, data: Value) {
        self.inner.log.event(event, data);
    }

    /// Sends a synthetic hotkey event (used by the UI's buttons and by tests).
    pub fn press(&self, name: &str, pressed: bool) {
        let _ = self.events.send(HotkeyEvent {
            name: name.into(),
            pressed,
            at: Instant::now(),
        });
    }
}
