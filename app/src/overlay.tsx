import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { onUiEvent, type UiEvent } from "./api";
import "./overlay.css";

type View = { tone: "listen" | "work" | "done" | "info" | "error"; label: string; text: string };

const ACTION_LABELS: Record<string, string> = {
  dictate: "Listening",
  edit: "Listening for an edit",
};

function reduce(view: View | null, event: UiEvent): View | null {
  switch (event.type) {
    case "listening":
      return {
        tone: "listen",
        label: (ACTION_LABELS[event.action] ?? "Listening") + (event.hands_free ? " · hands-free" : ""),
        text: "",
      };
    case "partial":
      return view ? { ...view, text: event.text } : view;
    case "working":
      return { tone: "work", label: event.label, text: view?.text ?? "" };
    case "done":
      return { tone: "done", label: `Done · ${(event.ms / 1000).toFixed(1)} s`, text: event.text };
    case "message":
      return { tone: event.level === "error" ? "error" : "info", label: event.level === "error" ? "Couldn't finish" : "Duma", text: event.text };
    case "idle":
      return null;
    default:
      return view;
  }
}

function Overlay() {
  const [view, setView] = useState<View | null>(null);
  useEffect(() => {
    const unlisten = onUiEvent((event) => setView((current) => reduce(current, event)));
    return () => void unlisten.then((f) => f());
  }, []);
  if (!view) return null;
  const tail = view.text.length > 140 ? "…" + view.text.slice(-140) : view.text;
  return (
    <div className="stage">
      <div className={`pill ${view.tone}`}>
        <span className="dot" />
        <div className="body">
          <div className="label">{view.label}</div>
          {tail && <div className="text">{tail}</div>}
        </div>
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<Overlay />);
