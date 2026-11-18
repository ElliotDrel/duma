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

One mental model: **every action operates on a target span of text and produces a new version of it.**

| Trigger | Target | Behavior |
| --- | --- | --- |
| Hold dictation key, nothing selected | cursor | Dictate → clean → insert. |
| Hold dictation key, text selected | selection | Speech is an **instruction** ("make this shorter") → replace selection. |
| Tap again within N seconds of any Duma output | last output | Refine it by voice ("no, more casual") without reselecting. |
| Preset key (or say the preset name) | selection or last output | Run a saved prompt. Spell-check is preset #1 (USC's pipeline). |
| Undo key | last output | Restore previous version. Each session keeps its version chain. |

Presets are first-class: name, prompt, model, hotkey, spoken alias, per-app defaults. Dictation cleanup itself is just the default preset.

## Speed architecture

Target: key release → text in field **< 500 ms p50** for a cloud sentence, < 300 ms local. Measured from day one.

1. **Stream while speaking.** Audio goes to STT (streaming WebSocket or local chunked decoding behind VAD) during the hold, so only the last chunk is pending at release.
2. **Warm everything.** Persistent HTTP/2 / WebSocket connections opened at startup and kept alive; local models loaded at idle; overlay window pre-created hidden.
3. **Cheapest model that is good enough.** Rules → local 0.6B cleanup → cloud LLM, escalating only when needed. Short text never pays for a big model.
4. **Stream the LLM output** and start the paste as soon as the output is final; never block on logging, history writes, or UI.
5. **Perf gates in CI** (USC pattern): every stage timed into the event log; the bench fails on regressions.

## Data model

Local SQLite, append-only events per session: audio (Opus), raw ASR with timings, each preset/edit step (input, prompt, model, output, latency), inserted text, target app, and post-insert user edits (UI Automation on Windows, Accessibility on macOS, bounded watch like Coda). Export as JSONL for training. Cloud sync is opt-in and later.

Learning roadmap:
1. Dictionary suggestions from observed edits.
2. Gated LoRA on the local cleanup model (Coda recipe).
3. Per-user ASR adaptation: LoRA on Whisper/Parakeet using the user's audio + their corrected text as labels.

## Tech stack

- **Rust workspace**, all hot-path logic in Rust:
  - `duma-core`: session/version model, pipeline, provider traits (STT, LLM), presets, event log.
  - `duma-platform`: hotkeys (`handy-keys`), mic (`handy-recorder`), selection capture, clipboard backup/restore, paste, focused-field observation. `windows` crate on Windows, `objc2` on macOS.
  - `duma-stt` / `duma-llm`: cloud providers plus local via `transcribe-cpp` and a local cleanup model.
- **Tauri 2 shell** for tray, overlay, settings, history, presets UI (React + TypeScript). The webview never sits on the hot path.
- Updates: Velopack (proven in USC, supports both OSes) or Tauri updater.
- Rejected: Electron (memory/startup), native UI per OS (double the UI work), Swift-only (Elliot develops on Windows).

Mac from Windows: CI builds and tests macOS on GitHub runners. Real Mac testing still needs an Apple Developer ID; without stable signing, macOS drops Accessibility permission on every rebuild (Coda's `SIGNING.md`).

## Build order

1. **Walking skeleton (Windows):** hotkey → streaming cloud STT → cleanup → guarded paste, with event log and stage timings. Bench + perf budget in CI.
2. Selection-as-instruction and refine-last-output (the loop).
3. Presets + spell-check preset (port USC behaviors: literal protection, modifier waits, clipboard rules).
4. macOS parity (AX selection, CGEvent paste, permissions onboarding, signing).
5. Local engines: Parakeet/Whisper via `transcribe-cpp`, local cleanup model with guard.
6. Edit observation → dictionary suggestions → gated LoRA.
7. Rich text round-trip, per-app context, snippets, history UI polish.

## Open decisions

- Greenfield with Handy's crates (recommended) vs forking Handy.
- Default providers for v0 (fastest streaming STT and LLM to benchmark before choosing).
- License: MIT recommended, matching USC and allowing reuse of Handy/Coda MIT code.
