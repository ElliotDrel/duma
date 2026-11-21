# Duma v1 architecture

Decided with Elliot on 2026-11-20. KISS: the fewest moving parts that make the core work well. Anything not listed here is a future GitHub issue, not v1.

## Core features

1. **Dictate:** hold the hotkey, speak, release. Duma records, transcribes, polishes, and pastes the text at the cursor.
2. **Spell check:** select text, press the hotkey. Duma replaces the selection with the corrected text (Universal Spell Check behavior).
3. **Paste last:** press the hotkey to paste your most recent result, from any PC signed into your account.

Nothing else in v1: no dictionary, threads, voice edits, presets, undo, or edit watch.

## Moving parts

| Part | Job |
| --- | --- |
| Desktop app (Rust + Tauri, Mac and Windows) | Hotkeys, mic, overlay pill, selection copy, paste, sign-in. |
| Supabase | Sign-in, one `runs` table, two edge functions (`token`, `polish`). Holds the OpenAI key. |
| OpenAI | `gpt-live-transcribe` for speech, `gpt-6-luna` (reasoning effort `none`, standard tier) for polish and spell check. |

## Flows

**Dictate**

1. Key down: the app asks `token` for a short-lived OpenAI client secret (cached until near expiry) and opens the transcription socket.
2. While the key is held, audio streams to OpenAI. No partial text is shown or sent back to the UI.
3. Key up: the app commits the audio and waits for the full transcript.
4. The app sends the transcript to `polish`. The function saves the transcript, polishes it, saves the result, and returns the text.
5. The app pastes the text into the focused field.

**Spell check:** the app copies the selection, sends it to `polish` with mode `spellcheck`, and pastes the result over the selection. Saved as a run like a dictation.

**Paste last:** the app fetches the user's newest run and pastes it. Offline, it falls back to this PC's last result in memory.

There is no local database and no sync. The app reads the cloud on demand.

## Data

- **Cloud (`runs` table, per run):** user, mode (`dictate` / `spellcheck`), transcript or input text, polished text, app name, model, stage timings, error, created at. Row-level security: users read only their own runs.
- **Local only:** the audio of each dictation and the JSONL event log, in the app data folder. Audio never goes anywhere except OpenAI for transcription.
- **Never:** the OpenAI key on a user's machine.

## Paying for users' AI

The OpenAI key lives only in Supabase secrets. Sign-in is required and access is invite-only. `polish` and `token` enforce a per-user daily cap. The OpenAI project has a hard monthly budget as a backstop.

## Speed

Measured with `crates/duma-core/examples/bench.rs`. Today: transcript 0.33-0.54 s after release, polish 1.15-2.0 s. Polish is the bottleneck. v1 levers: a stable cacheable prompt prefix, a warm connection to `polish`, and the `polish` function hosted near OpenAI (US East).

## Non-negotiables (to be guarded by CI)

- Text lands in the field the user was in, never anywhere else.
- The user's clipboard is restored after a paste.
- No secret in the app, the repo, or the logs.
- Release-to-text latency does not regress beyond the bench budget.

## Removed from the pre-v1 code

Threads and versions, voice edit, presets (except spell check), undo, edit watch, vocabulary, selection modes, local SQLite store, user-supplied API key in the keychain, most of the Settings page.
