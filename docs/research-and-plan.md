# Duma — research and plan

Status: draft, 2026-11-18. Star counts pulled from the GitHub API that day.

## Goal

One cohesive, near-instant layer for all text work on the computer: thought → dictation → voice edits → preset transforms → polished text. Windows and macOS. Every step logged so the user (and later a personal model) can learn from it.

## Landscape

| Project | Stars | Stack | License | Platforms | Worth stealing |
| --- | --- | --- | --- | --- | --- |
| [Handy](https://github.com/cjpais/Handy) | 39.2k | Rust + Tauri 2 | MIT | Win/Mac/Linux | The reference cross-platform core. Extracted crates: [`handy-keys`](https://github.com/handy-computer/handy-keys) (global hotkeys), [`handy-recorder`](https://github.com/handy-computer/handy-recorder) (mic capture), [`transcribe-cpp`](https://github.com/handy-computer/transcribe.cpp) (ggml STT, 16+ model families). Silero VAD, Parakeet V3. Deliberately "one tool, one job": no editing. |
| [Buzz](https://github.com/chidiwilliams/buzz) | 22.9k | Python | MIT | Win/Mac/Linux | File transcription, not dictation. Skip. |
| [FluidVoice](https://github.com/altic-dev/FluidVoice) | 13.1k | Swift | GPL-3.0 | Mac (Win beta) | Command Mode, Edit Mode, live preview overlay, per-app prompt sets, "hold Space to mute". Its own fine-tuned local cleanup models. GPL: ideas only. |
| [OpenWhispr](https://github.com/OpenWhispr/openwhispr) | 11.8k | Electron | MIT | Win/Mac/Linux | Voice-assistant hotkey that edits highlighted text in place; agent/MCP integration. Electron is heavier than we want. |
| [Vibe](https://github.com/thewh1teagle/vibe) | 8.2k | Tauri | MIT | Win/Mac/Linux | File transcription. Skip. |
| [VoiceInk](https://github.com/Beingpax/VoiceInk) | 7.2k | Swift | GPL-3.0 | Mac | Per-app/URL "Modes", screen-context awareness, AI assistant mode. No PRs accepted. |
| [Epicenter/Whispering](https://github.com/EpicenterHQ/epicenter) | 4.8k | TS/Tauri | mixed | Win/Mac/Linux | Transformation pipelines on transcripts. |
| [Voquill](https://github.com/voquill/voquill) | 1.0k | TS/Tauri | AGPL-3.0 | Win/Mac/Linux | Glossary sync, overlay. AGPL: ideas only. |
| [Voicetypr](https://github.com/ideaplexa/voicetypr) | 0.7k | Rust/Tauri/React | AGPL-3.0 | Win/Mac | Agent-ready CLI, LAN transcription server. |
| [SayIt](https://github.com/crosswk/SayIt) | 0.5k | TS | AGPL-3.0 | Win/Mac | **Selection = edit instruction**: select text, speak, selection gets rewritten. Reads text around cursor for tone. |
| [Tambourine](https://github.com/kstonekuan/tambourine-voice) | 0.4k | Tauri + Python/Pipecat | AGPL-3.0 | Win/Mac | Streaming pipeline over WebRTC to many STT/LLM providers. Voice editing only "planned". |
| [OpenWhisper](https://github.com/Knuckles92/OpenWhisper) | 0.3k | Python | MIT | Win/Mac/Linux | Saved transforms, learned rules from corrections, local API + MCP. |

Commercial bar: Wispr Flow already ships [Command Mode](https://docs.wisprflow.ai/articles/4816967992-how-to-use-command-mode) (select text, hold a separate shortcut, speak an instruction), [Transforms](https://docs.wisprflow.ai/articles/8068950331-how-to-use-transforms-beta) (custom-prompt rewrites on per-prompt shortcuts, beta), [Styles](https://docs.wisprflow.ai/articles/2368263928-how-to-setup-flow-styles) and auto-learning dictionary. The gap is not missing features; it is that they are separate modes, separate shortcuts, behind "Experimental", and cloud-only.

### Takeaways

1. Nobody owns the **loop**. Every app treats dictate, edit, and transform as separate one-shot actions. Duma's differentiator is one session where each output can immediately be refined by voice or a preset.
2. **Rust + Tauri 2 is the consensus cross-platform stack** (Handy, Voquill, Voicetypr, Tambourine, Verenu). Handy has already extracted the hard OS parts into MIT crates we can depend on.
3. **Local cleanup is solved enough to ship.** Coda runs `spokenly/cleanup-1` (0.6B, Apache-2.0) at ~60 ms median with a word-order guard, and falls back to rules when the model is cold or rejected.
4. Licenses: copy code only from MIT/Apache projects (Handy, OpenWhispr, Coda app code, USC). GPL/AGPL projects are inspiration only.

## Reference repos

### Universal-Spell-Check (USC) — take the lessons, not the code

Native Rust Windows tray app (vendored `native-windows-gui`, raw Win32, WinHTTP). What carries over:

- **Speed discipline**: one app-lifetime HTTP/2 session, no webview on the hot path, a perf budget in CI (`bench/perf-budget.json`), and an end-to-end bench that drives the real hotkey → copy → model → paste.
- **Clipboard correctness**: snapshot every clipboard format, wait for physical modifiers to release before injecting, never open the clipboard right after `Ctrl+V`, exclude captured text from Win+V history, restore on failure. Hard-won; re-implement on top of these rules.
- **Literal protection**: URLs, paths, keys, IDs become placeholders before the model sees text; a missing placeholder aborts the paste.
- **Rich text round-trip** (CF_HTML, Google Docs native slice). Valuable but deep; port later.
- **Log is the source of truth** (JSONL events, channel-stamped), Prod/Dev channels running side by side, Velopack updates.
- Why not the base: Windows-only UI layer and Win32 throughout; Duma needs a cross-platform core from day one.

### Coda — Mac-native inspiration

SwiftUI + local Voz ASR. What carries over:

- **Learning from edits**: after pasting, watch only that field for 60 s via Accessibility, capture `{model input, pasted, edited}`. Small spelling-like fixes become dictionary suggestions; cleanup fixes feed a LoRA.
- **Automatic personal fine-tune with a gate**: after 10 usable edits and 2 idle minutes, train a LoRA on the 0.6B cleanup model, then install it only if it beats the base model on held-out edits and a regression set. Every run retrains from the base. This is exactly the "overfit to the user" loop, already working.
- **Guarded paste**: re-check app/window/field before paste, abort on secure input, never press Return.
- **Tiered cleanup**: None / Light (rules) / Smart (model + guard), raw always recoverable in history.
- Snippets bypass cleanup; spoken URLs/numbers formatting; mutually-exclusive dictation vs meeting.
- Caveat: Voz/Who are source-available, not open source. Do not depend on them.

## Product: the Duma loop

One mental model: **every piece of text Duma touches gets a thread.** A thread is the conversation about that text: the original dictation, every spoken instruction, every preset run, every version, and the user's own manual edits observed afterward. Each new instruction is sent to the model with the thread's history, so "no, shorter, and keep the second point" works the way it would in a chat, while the result still lands in place in the target app.

| Trigger | Target | Behavior |
| --- | --- | --- |
| Hold dictation key, nothing selected | cursor | Dictate → clean → insert. |
| Hold dictation key, text selected | selection | Speech is an **instruction** ("make this shorter") → replace selection. |
| Tap again within N seconds of any Duma output | last output | Refine it by voice ("no, more casual") without reselecting. |
| Preset key (or say the preset name) | selection or last output | Run a saved prompt. Spell-check is preset #1 (USC's pipeline). |
| Undo key | last output | Restore previous version. Each thread keeps its version chain. |

Thread resolution: a selection that matches a version Duma produced (or the text in the field Duma last wrote to) reopens that thread; anything else starts a new one. Threads are browsable in the app afterward, so polishing can continue there too.

Models are plug-and-play behind provider traits (STT, LLM). **v1 is cloud-only**, using established providers; local engines, installation, and fine-tuning come after the cloud experience is polished.

Presets are first-class: name, prompt, model, hotkey, spoken alias, per-app defaults. Dictation cleanup itself is just the default preset.

## Speed architecture

Target: key release → text in field **< 500 ms p50** for a cloud sentence. Measured from day one.

1. **Stream while speaking.** Audio goes to a streaming cloud STT over WebSocket during the hold, so only the last chunk is pending at release.
2. **Warm everything.** Persistent HTTP/2 / WebSocket connections opened at startup and kept alive; overlay window pre-created hidden.
3. **Cheapest model that is good enough.** Deterministic rules first, a fast small cloud LLM for cleanup, a stronger model only for instructions and presets that need it.
4. **Stream the LLM output** and start the paste as soon as the output is final; never block on logging, history writes, or UI.
5. **Perf gates in CI** (USC pattern): every stage timed into the event log; the bench fails on regressions.

## Data capture

Capture everything that could later explain a bad result or train a better one. USC's logging depth is the floor, not the ceiling. All local; cloud sync is opt-in and later.

- **Storage:** SQLite as the queryable index, plus append-only JSONL event logs (USC pattern: every line stamped with channel, app version, pid, thread id) and raw audio (Opus) on disk. Writes happen off the hot path.
- **Per dictation:** audio, device and sample rate, VAD segments, STT provider/model/request id, interim and final transcripts with word timings and confidences, every stage timestamp (key down → first audio → first partial → release → final transcript → LLM first token → LLM done → paste issued → paste observed).
- **Per model call:** full prompt, thread context sent, model, parameters, raw response, token counts, latency, retries, guard decisions.
- **Target context:** app bundle id / exe, window title, URL for browsers, field role, text before and after the cursor (bounded), selection, secure-field flag.
- **Insertion:** method used, clipboard formats backed up and restored, whether the paste was observed landing in the field.
- **After insertion (the edit watch):** observe the field Duma wrote to for 60 s (longer later if useful), store every change as a diff against Duma's output, with timestamps. Stop on app/window/field change, secure input, or screen lock. Coda proves this works on Mac; Windows uses UI Automation text-change events.
- **Explicit feedback:** undo, re-run, thumbs, manual corrections in the app.
- **Privacy controls:** pause capture, per-app exclusions, never capture secure fields, retention limits, one-click export and delete.

Learning roadmap (after v1): dictionary suggestions from observed edits → prompt and preset tuning from the log → local models with gated per-user LoRA (Coda recipe) → per-user ASR adaptation.

## Tech stack (decided 2026-11-18)

**Rust core + Tauri 2 shell (React + TypeScript), Rust FFI straight to the OS APIs.** Greenfield, with Handy's crates as dependencies.

Why this stack:

- **Native access is a property of the core language, not the UI framework.** Rust reaches every native API Duma needs with no wrapper layer: macOS Accessibility (`AXUIElement`, `AXObserver` notifications for value and selection changes) through `objc2-application-services` / `accessibility-sys`, AppKit and CGEvent through `objc2-app-kit`; Windows UI Automation (TextPattern, text-changed events) through the `windows` crate or `uiautomation`, plus SendInput, clipboard, and WinHTTP as USC already does. Anything missing is a few lines of `extern "C"`.
- **Mac-first feel is reachable.** The overlay is a non-activating `NSPanel` so it never steals focus (`tauri-nspanel` plugin, or build the panel directly with `objc2`). Settings and history are ordinary windows where a webview is fine.
- **Never a speed blocker.** Hotkey, audio, network, paste, and the edit watch all run in Rust threads. The webview only renders state pushed to it.
- **Matches how Elliot works.** Same Rust code builds on Windows and Mac; Windows dev machines do most of the work, CI builds and tests macOS, the MacBook validates feel. One UI codebase.
- **Proven:** the cross-platform leaders (Handy, Voquill, Voicetypr, Tambourine) all ship on it.

Rejected:

- SwiftUI app + Rust core (UniFFI): best Mac polish, but the Mac app cannot be built or run on Windows, and Windows needs a second native UI.
- Electron: same webview story with a much heavier runtime and Node native addons for every OS call.
- .NET / Avalonia, Flutter: weaker macOS Accessibility interop than Rust's `objc2`.

Crate layout:

- `duma-core`: threads, versions, pipeline, provider traits (STT, LLM), presets, event log.
- `duma-platform`: hotkeys (`handy-keys`), mic (`handy-recorder`), selection capture, clipboard backup/restore, paste, field observation. `cfg(target_os)` modules for macOS and Windows behind one trait.
- `duma-providers`: cloud STT and LLM clients.
- `app/`: Tauri shell and React UI.

Known native traps to design for: Electron and Chromium apps expose little to Accessibility until asked (set `AXManualAccessibility` / `AXEnhancedUserInterface` on the app element); elevated Windows apps silently drop injected input (UIPI); unsigned or re-signed macOS builds lose Accessibility permission on every rebuild, so a stable Developer ID signing identity is needed early.

## Build order

1. **Walking skeleton:** hotkey → streaming cloud STT → cleanup → guarded paste, full event log and stage timings. Built on Windows, run on Mac from the start so neither platform drifts. Bench + perf budget in CI.
2. **Edit watch** on both platforms (Accessibility / UI Automation), writing diffs into the log.
3. Threads: selection-as-instruction, refine-last-output with thread context, undo.
4. Presets + spell-check preset (port USC behaviors: literal protection, modifier waits, clipboard rules).
5. Per-app context, snippets, history and thread browser UI, rich text round-trip.
6. Later: local engines, learning loop, fine-tuning.

## Open decisions

- Default cloud providers for v1: benchmark the fastest streaming STT and fast LLMs before choosing.
- License: MIT recommended, matching USC and allowing reuse of Handy/Coda MIT code.
