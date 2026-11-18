import { openPath } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useState } from "react";
import { api, onUiEvent, type Preset, type Settings, type Status, type ThreadSummary, type Version } from "./api";

type Page = "threads" | "presets" | "settings";

export function App() {
  const [page, setPage] = useState<Page>("threads");
  const [status, setStatus] = useState<Status | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [toast, setToast] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setStatus(await api.status());
    setSettings(await api.settings());
  }, []);

  useEffect(() => {
    void refresh();
    const onFocus = () => void api.status().then(setStatus);
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

  const save = async (next: Settings) => {
    try {
      await api.saveSettings(next);
      setSettings(next);
      setToast("Saved");
    } catch (error) {
      setToast(String(error));
    }
    setTimeout(() => setToast(null), 2200);
  };

  if (!status || !settings) return <div className="loading">Loading…</div>;
  const needsSetup = !status.api_key || !status.accessibility_trusted;

  return (
    <div className="shell">
      <aside className="nav">
        <div className="brand">
          Duma{status.channel === "dev" && <span className="badge">dev</span>}
        </div>
        {(["threads", "presets", "settings"] as Page[]).map((p) => (
          <button key={p} className={page === p ? "active" : ""} onClick={() => setPage(p)}>
            {p[0].toUpperCase() + p.slice(1)}
          </button>
        ))}
        <div className="nav-foot">v{status.version}</div>
      </aside>
      <main>
        {needsSetup && <Setup status={status} onChange={refresh} />}
        {page === "threads" && <Threads settings={settings} />}
        {page === "presets" && <Presets settings={settings} onSave={save} />}
        {page === "settings" && <SettingsPage settings={settings} status={status} onSave={save} onKeySaved={refresh} />}
      </main>
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}

function Setup({ status, onChange }: { status: Status; onChange: () => void }) {
  return (
    <section className="setup">
      <h2>Finish setting up</h2>
      {!status.api_key && (
        <div className="step">
          <p>Add your OpenAI API key. It is stored in your system keychain, never in files or logs.</p>
          <ApiKeyField onSaved={onChange} />
        </div>
      )}
      {!status.accessibility_trusted && (
        <div className="step">
          <p>
            Grant Accessibility access so Duma can read the field you're typing in and paste into it. After enabling
            Duma in System Settings, return here.
          </p>
          <button onClick={() => void api.requestAccessibility()}>Open Accessibility settings</button>
        </div>
      )}
    </section>
  );
}

function ApiKeyField({ onSaved }: { onSaved: () => void }) {
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="row">
      <input
        type="password"
        placeholder="sk-…"
        value={key}
        onChange={(e) => setKey(e.target.value)}
        autoComplete="off"
        spellCheck={false}
      />
      <button
        disabled={!key.trim()}
        onClick={async () => {
          try {
            await api.setApiKey(key);
            setKey("");
            setError(null);
            onSaved();
          } catch (e) {
            setError(String(e));
          }
        }}
      >
        Save key
      </button>
      {error && <span className="error">{error}</span>}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------

const KIND_LABELS: Record<string, string> = {
  dictation: "Dictated",
  original: "Original",
  instruction: "Voice edit",
  preset: "Preset",
  manual_edit: "Your edit",
};

function when(iso: string) {
  const date = new Date(iso);
  return date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

function Threads({ settings }: { settings: Settings }) {
  const [threads, setThreads] = useState<ThreadSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [versions, setVersions] = useState<Version[]>([]);
  const [query, setQuery] = useState("");

  const load = useCallback(() => void api.threads().then(setThreads), []);
  useEffect(() => {
    load();
    const unlisten = onUiEvent((e) => e.type === "threads_changed" && load());
    return () => void unlisten.then((f) => f());
  }, [load]);
  useEffect(() => {
    if (selected) void api.thread(selected).then(setVersions);
  }, [selected, threads]);

  const shown = threads.filter((t) => !query || (t.head_text ?? "").toLowerCase().includes(query.toLowerCase()));
  const head = threads.find((t) => t.id === selected)?.head_id;

  return (
    <section className="threads">
      <div className="list">
        <input className="search" placeholder="Search" value={query} onChange={(e) => setQuery(e.target.value)} />
        {shown.length === 0 && (
          <div className="empty">
            Nothing yet. Hold <kbd>{settings.hotkeys.dictate}</kbd> and speak.
          </div>
        )}
        {shown.map((t) => (
          <button key={t.id} className={`thread ${selected === t.id ? "active" : ""}`} onClick={() => setSelected(t.id)}>
            <div className="preview">{t.head_text || "(empty)"}</div>
            <div className="meta">
              {t.app_name ?? "Unknown app"} · {when(t.updated_at)} · {t.version_count} version{t.version_count === 1 ? "" : "s"}
            </div>
          </button>
        ))}
      </div>
      <div className="detail">
        {!selected && <div className="empty">Select a thread to see every version and instruction.</div>}
        {selected &&
          versions.map((v) => (
            <article key={v.id} className={`version ${v.id === head ? "head" : ""}`}>
              <header>
                <span className={`kind ${v.kind}`}>{KIND_LABELS[v.kind] ?? v.kind}</span>
                {v.id === head && <span className="current">current</span>}
                <span className="meta">{when(v.created_at)}</span>
                <button className="link" onClick={() => void navigator.clipboard.writeText(v.text)}>
                  Copy
                </button>
              </header>
              {v.source && v.kind !== "manual_edit" && <div className="source">“{v.source}”</div>}
              <p>{v.text}</p>
            </article>
          ))}
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------------------------

function Presets({ settings, onSave }: { settings: Settings; onSave: (s: Settings) => Promise<void> }) {
  const [presets, setPresets] = useState<Preset[]>(settings.presets);
  useEffect(() => setPresets(settings.presets), [settings]);
  const update = (i: number, patch: Partial<Preset>) =>
    setPresets(presets.map((p, j) => (j === i ? { ...p, ...patch } : p)));
  const dirty = JSON.stringify(presets) !== JSON.stringify(settings.presets);

  return (
    <section className="page">
      <h1>Presets</h1>
      <p className="lede">
        Select text anywhere and press a preset's shortcut. With nothing selected, a preset runs on Duma's last
        output. Every run becomes a version in that text's thread.
      </p>
      {presets.map((p, i) => (
        <div className="card" key={i}>
          <div className="grid2">
            <label>
              Name
              <input value={p.name} onChange={(e) => update(i, { name: e.target.value })} />
            </label>
            <label>
              Shortcut
              <input value={p.hotkey} onChange={(e) => update(i, { hotkey: e.target.value })} placeholder="Ctrl+Alt+5" />
            </label>
          </div>
          <label>
            Prompt
            <textarea rows={3} value={p.prompt} onChange={(e) => update(i, { prompt: e.target.value })} />
          </label>
          <div className="row">
            <label className="grow">
              Model (blank uses the edit model)
              <input value={p.model ?? ""} onChange={(e) => update(i, { model: e.target.value || null })} />
            </label>
            <button className="danger" onClick={() => setPresets(presets.filter((_, j) => j !== i))}>
              Delete
            </button>
          </div>
        </div>
      ))}
      <div className="row">
        <button
          onClick={() =>
            setPresets([...presets, { id: crypto.randomUUID().slice(0, 8), name: "New preset", prompt: "", hotkey: "", model: null }])
          }
        >
          Add preset
        </button>
        <button className="primary" disabled={!dirty} onClick={() => void onSave({ ...settings, presets })}>
          Save presets
        </button>
      </div>
    </section>
  );
}

// ---------------------------------------------------------------------------------------------

function lines(list: string[]) {
  return list.join("\n");
}
function parseLines(text: string) {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function SettingsPage({
  settings,
  status,
  onSave,
  onKeySaved,
}: {
  settings: Settings;
  status: Status;
  onSave: (s: Settings) => Promise<void>;
  onKeySaved: () => void;
}) {
  const [draft, setDraft] = useState<Settings>(settings);
  const [vocabulary, setVocabulary] = useState(lines(settings.vocabulary));
  const [excluded, setExcluded] = useState(lines(settings.capture.excluded_apps));
  useEffect(() => {
    setDraft(settings);
    setVocabulary(lines(settings.vocabulary));
    setExcluded(lines(settings.capture.excluded_apps));
  }, [settings]);

  const set = <K extends keyof Settings>(key: K, value: Settings[K]) => setDraft({ ...draft, [key]: value });
  const merged: Settings = {
    ...draft,
    vocabulary: parseLines(vocabulary),
    capture: { ...draft.capture, excluded_apps: parseLines(excluded) },
  };
  const dirty = JSON.stringify(merged) !== JSON.stringify(settings);

  return (
    <section className="page">
      <h1>Settings</h1>

      <h2>OpenAI</h2>
      <div className="card">
        <p className="muted">Key: {status.api_key ?? "not set"}</p>
        <ApiKeyField onSaved={onKeySaved} />
        <div className="grid3">
          <label>
            Transcription model
            <input
              value={draft.transcription.model}
              onChange={(e) => set("transcription", { ...draft.transcription, model: e.target.value })}
            />
          </label>
          <label>
            Cleanup model
            <input value={draft.cleanup.model} onChange={(e) => set("cleanup", { ...draft.cleanup, model: e.target.value })} />
          </label>
          <label>
            Edit model
            <input value={draft.edit.model} onChange={(e) => set("edit", { ...draft.edit, model: e.target.value })} />
          </label>
        </div>
        <label className="check">
          <input type="checkbox" checked={draft.openai_store} onChange={(e) => set("openai_store", e.target.checked)} />
          Let OpenAI store responses (Duma keeps its own local log either way)
        </label>
      </div>

      <h2>Shortcuts</h2>
      <div className="card grid2">
        {(["dictate", "edit", "undo", "paste_last", "cancel"] as const).map((k) => (
          <label key={k}>
            {{ dictate: "Dictate (hold, or tap for hands-free)", edit: "Voice edit", undo: "Undo last change", paste_last: "Paste last result", cancel: "Cancel (while active)" }[k]}
            <input value={draft.hotkeys[k]} onChange={(e) => set("hotkeys", { ...draft.hotkeys, [k]: e.target.value })} />
          </label>
        ))}
        <label>
          With text selected, dictation…
          <select
            value={draft.selection_mode}
            onChange={(e) => set("selection_mode", e.target.value as Settings["selection_mode"])}
          >
            <option value="instruct">edits the selection (speech is an instruction)</option>
            <option value="replace">replaces the selection</option>
          </select>
        </label>
      </div>

      <h2>Vocabulary</h2>
      <div className="card">
        <p className="muted">Names and terms Duma should spell your way. One per line.</p>
        <textarea rows={4} value={vocabulary} onChange={(e) => setVocabulary(e.target.value)} />
      </div>

      <h2>Data capture</h2>
      <div className="card">
        <label className="check">
          <input
            type="checkbox"
            checked={draft.capture.save_audio}
            onChange={(e) => set("capture", { ...draft.capture, save_audio: e.target.checked })}
          />
          Keep the audio of every dictation
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={draft.capture.watch_edits}
            onChange={(e) => set("capture", { ...draft.capture, watch_edits: e.target.checked })}
          />
          Learn from my edits: watch the field for
          <input
            className="tiny"
            type="number"
            min={5}
            value={draft.capture.watch_seconds}
            onChange={(e) => set("capture", { ...draft.capture, watch_seconds: Number(e.target.value) })}
          />
          seconds after inserting
        </label>
        <label>
          Characters before the cursor sent to the model as context (0 sends none)
          <input
            type="number"
            min={0}
            value={draft.capture.model_context_chars}
            onChange={(e) => set("capture", { ...draft.capture, model_context_chars: Number(e.target.value) })}
          />
        </label>
        <label>
          Never read text in these apps (one per line)
          <textarea rows={3} value={excluded} onChange={(e) => setExcluded(e.target.value)} />
        </label>
        <div className="row">
          <button onClick={() => void openPath(status.logs_dir)}>Open logs</button>
          <button onClick={() => void openPath(status.data_dir)}>Open data folder</button>
        </div>
      </div>

      <h2>Prompts</h2>
      <div className="card">
        <label>
          Dictation cleanup
          <textarea rows={8} value={draft.dictation_prompt} onChange={(e) => set("dictation_prompt", e.target.value)} />
        </label>
        <label>
          Voice edits
          <textarea rows={8} value={draft.edit_prompt} onChange={(e) => set("edit_prompt", e.target.value)} />
        </label>
        <button
          onClick={async () => {
            const defaults = await api.defaultSettings();
            setDraft({ ...draft, dictation_prompt: defaults.dictation_prompt, edit_prompt: defaults.edit_prompt });
          }}
        >
          Restore default prompts
        </button>
      </div>

      <div className="savebar">
        <button className="primary" disabled={!dirty} onClick={() => void onSave(merged)}>
          Save settings
        </button>
      </div>
    </section>
  );
}
