// Typed wrappers over the Tauri commands in src-tauri/src/lib.rs.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ModelSettings = {
  model: string;
  reasoning_effort: string | null;
  verbosity: string | null;
  timeout_ms: number;
};

export type Preset = { id: string; name: string; prompt: string; hotkey: string; model: string | null };

export type Settings = {
  hotkeys: { dictate: string; edit: string; undo: string; paste_last: string; cancel: string };
  transcription: { model: string; delay: string | null; languages: string[]; keywords: string[]; prompt: string | null };
  fallback_transcription_model: string;
  cleanup: ModelSettings;
  edit: ModelSettings;
  dictation_prompt: string;
  edit_prompt: string;
  presets: Preset[];
  vocabulary: string[];
  selection_mode: "instruct" | "replace";
  capture: {
    save_audio: boolean;
    watch_edits: boolean;
    watch_seconds: number;
    log_context_chars: number;
    model_context_chars: number;
    excluded_apps: string[];
  };
  openai_store: boolean;
  microphone: string | null;
  tap_toggle_ms: number;
  rules_only_max_words: number;
};

export type Status = {
  api_key: string | null;
  accessibility_trusted: boolean;
  data_dir: string;
  logs_dir: string;
  channel: string;
  version: string;
};

export type ThreadSummary = {
  id: string;
  created_at: string;
  updated_at: string;
  app_name: string | null;
  window_title: string | null;
  head_id: string | null;
  head_text: string | null;
  version_count: number;
};

export type Version = {
  id: string;
  thread_id: string;
  parent_id: string | null;
  created_at: string;
  kind: string;
  text: string;
  source: string | null;
  run_id: string | null;
  model: string | null;
};

export type UiEvent =
  | { type: "listening"; action: string; hands_free: boolean }
  | { type: "partial"; text: string }
  | { type: "working"; label: string }
  | { type: "done"; action: string; text: string; ms: number }
  | { type: "message"; level: string; text: string }
  | { type: "idle" }
  | { type: "threads_changed" };

export const api = {
  status: () => invoke<Status>("status"),
  settings: () => invoke<Settings>("get_settings"),
  defaultSettings: () => invoke<Settings>("default_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  setApiKey: (key: string) => invoke<void>("set_api_key", { key }),
  threads: (limit = 200, offset = 0) => invoke<ThreadSummary[]>("list_threads", { limit, offset }),
  thread: (id: string) => invoke<Version[]>("get_thread", { id }),
  requestAccessibility: () => invoke<void>("request_accessibility"),
};

export function onUiEvent(handler: (event: UiEvent) => void): Promise<UnlistenFn> {
  return listen<UiEvent>("duma://ui", (e) => handler(e.payload));
}
