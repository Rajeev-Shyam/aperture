//! ContextPreviewPanel — the trust surface, G7 (doc 11 §4).
//
//  Renders the ACTUAL serialized Context Payload (doc 03 §4), not a summary:
//    1. Intent line (editable preset).
//    2. Items list — each typed with a kind icon, content expandable, per-item
//       remove (×). "What you see is what ships."
//    3. Redaction report — every applied rule with count (from doc 13's pipeline).
//    4. Enrichment affordances ("make context richer"): Add current selection ·
//       Add screen summary (may trigger a local VLM job, doc 06) · Add more
//       history (time-range slider extends event_trail) · Add screenshot (opt-in;
//       shows the downscaled image + token estimate, doc 09 §5) · free-text.
//    5. Footer: transport target + health dot · payload size/token estimate ·
//       Cancel / Send.
//
//  INVARIANT (doc 11 §4 / doc 15 §2): edits MUTATE the payload object; the panel
//  re-renders from that object; `preview_send` transmits exactly its
//  serialization (SHA-256 hash-logged as `cloud_send`, doc 03 §4). We never send
//  a separately-assembled body — `onChange` is the single mutation path, and
//  Send passes the very object the panel holds.
//
//  Contract law (doc 15 §2): only THIS panel sets `user_approved` (via
//  `preview_set_approved`); only the gateway consumes an approved payload.

import { useEffect, useMemo, useRef, useState } from "react";

import {
  previewSend,
  previewSetApproved,
  PAYLOAD_SIZE_WARN_BYTES,
  type ContextPayload,
  type Intent,
  type PayloadItem,
  type StructuredSuggestions,
  type TransportTarget,
} from "../lib/ipc";

interface Props {
  /** The live payload object — rendering + editing target. */
  payload: ContextPayload;
  /** Single mutation path: replace the object the panel (and Send) hold. */
  onChange: (next: ContextPayload) => void;
  /** Close the panel (Cancel, or after a successful Send). */
  onClose: (result?: StructuredSuggestions) => void;
}

const INTENT_PRESETS: { value: Intent; label: string }[] = [
  { value: "summarize_current", label: "Summarize current" },
  { value: "answer_query", label: "Answer query" },
  { value: "explain_pattern", label: "Explain pattern" },
  { value: "custom", label: "Custom" },
];

const TRANSPORT_LABELS: Record<TransportTarget, string> = {
  "claude-cli": "Claude CLI",
  "claude-desktop-mcp": "Claude Desktop (MCP)",
  "messages-api": "Messages API",
};

const ITEM_ICON: Record<PayloadItem["kind"], string> = {
  ocr_text: "🅣",
  event_trail: "≣",
  connector: "⛓",
  screenshot: "🖼",
  user_addition: "✎",
};

export function ContextPreviewPanel({ payload, onChange, onClose }: Props) {
  const [sending, setSending] = useState(false);
  const [freeText, setFreeText] = useState("");
  const [historyMinutes, setHistoryMinutes] = useState(0);
  const panelRef = useRef<HTMLDivElement>(null);

  // `aria-modal` must be backed by the real modal contract (this is the ONE gate
  // where the user reviews exactly what egresses): move focus in on open, restore
  // it to the opener on close, trap Tab, and map Escape to Cancel — the
  // zero-residue safe path (doc 13 §3).
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    panelRef.current?.focus();
    return () => opener?.focus?.();
  }, []);

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose(); // Cancel — drop everything, zero residue (doc 13 §3).
      return;
    }
    if (e.key !== "Tab") return;
    const focusables = panelRef.current?.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
    );
    if (!focusables || focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }

  // Size/token estimate over the EXACT wire serialization (doc 09 §5). We strip
  // nothing — JSON.stringify here matches what `preview_send` ships (modulo the
  // skip-serialized `user_approved` flag, which is not on this TS type anyway).
  const { bytes, tokenEstimate, overWarn } = useMemo(() => {
    const wire = JSON.stringify(payload);
    const b = new TextEncoder().encode(wire).length;
    // Rough heuristic (~4 bytes/token); the real estimate is computed core-side.
    return { bytes: b, tokenEstimate: Math.ceil(b / 4), overWarn: b > PAYLOAD_SIZE_WARN_BYTES };
  }, [payload]);

  // ---- Item editing (per-item remove; What-You-See-Is-What-Ships) ----------
  function removeItem(index: number) {
    onChange({ ...payload, items: payload.items.filter((_, i) => i !== index) });
  }

  function setIntent(intent: Intent) {
    onChange({ ...payload, intent });
  }

  // ---- Enrichment affordances (doc 11 §4) ----------------------------------
  function addUserText(text: string) {
    if (!text.trim()) return;
    onChange({ ...payload, items: [...payload.items, { kind: "user_addition", text }] });
    setFreeText("");
  }

  function addSelection() {
    // TODO(M7:) invoke a core command to capture the current selection text,
    //           then append it as a `user_addition` item. Placeholder marks it
    //           explicitly so it can't masquerade as real content.
    onChange({
      ...payload,
      items: [...payload.items, { kind: "user_addition", text: "[selection pending]" }],
    });
  }

  function addScreenSummary() {
    // TODO(M5:) invoke a local-VLM scene-summary job (doc 06); on completion
    //           append the structured summary as a `connector`/`user_addition`.
    //           This may queue a GPU job behind the single-permit mutex (doc 12)
    //           and trigger the gpu_busy degrade while it runs.
  }

  function addScreenshot() {
    // TODO(M5:) opt-in only. Core captures, downscales to <=1568px / ~1.15MP
    //           (doc 09 §5), returns { width, height, data_b64 } -> append a
    //           `screenshot` item. The footer token estimate then reflects it.
  }

  function applyHistoryRange(minutes: number) {
    setHistoryMinutes(minutes);
    // TODO(M7:) invoke a core command to extend the `event_trail` item over the
    //           selected range (capped at 50 events, EVENT_TRAIL_MAX, doc 03 §4),
    //           replacing the existing event_trail item in `items`.
  }

  // ---- Send (transmits the EXACT object the panel holds) -------------------
  async function send() {
    if (sending) return;
    setSending(true);
    try {
      // Contract law: ONLY this panel sets approval (doc 15 §2b).
      await previewSetApproved(payload);
      // Transmit exactly this object's serialization (hash-logged, doc 03 §4).
      const result = await previewSend(payload);
      onClose(result);
    } finally {
      setSending(false);
    }
  }

  return (
    <div
      className="preview surface-glass surface-interactive"
      role="dialog"
      aria-modal="true"
      aria-label="Context preview — review exactly what will be sent"
      ref={panelRef}
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      {/* 1. Intent (editable preset) */}
      <header className="preview__head">
        <label className="preview__intent">
          <span>Intent</span>
          <select value={payload.intent} onChange={(e) => setIntent(e.target.value as Intent)}>
            {INTENT_PRESETS.map((p) => (
              <option key={p.value} value={p.value}>
                {p.label}
              </option>
            ))}
          </select>
        </label>
        <code className="preview__id" title="payload_id">
          {payload.payload_id}
        </code>
      </header>

      {/* 2. Items list — typed, expandable, per-item remove. WYSIWYS. */}
      <section className="preview__items" aria-label="Payload items">
        {payload.items.length === 0 && (
          <p className="preview__empty">No items — nothing will be sent. Add context below.</p>
        )}
        {payload.items.map((item, i) => (
          <details key={i} className="preview__item">
            <summary>
              <span className="preview__item-icon" aria-hidden>
                {ITEM_ICON[item.kind]}
              </span>
              <span className="preview__item-kind">{item.kind}</span>
              <button
                className="btn btn--icon preview__item-remove"
                aria-label={`Remove ${item.kind} item`}
                onClick={(e) => {
                  e.preventDefault();
                  removeItem(i);
                }}
              >
                ×
              </button>
            </summary>
            {/* The full, exact content — this is what ships. */}
            <pre className="preview__item-body">{renderItemBody(item)}</pre>
          </details>
        ))}
      </section>

      {/* 3. Redaction report — rule + count, from doc 13's pipeline. */}
      {payload.redactions.length > 0 && (
        <section className="preview__redactions" aria-label="Redaction report">
          <h4>Redacted before preview</h4>
          <ul>
            {payload.redactions.map((r, i) => (
              <li key={i}>
                <span className="preview__redaction-rule">{r.rule}</span>
                <span className="preview__redaction-count">×{r.count}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {/* 4. Enrichment affordances ("make context richer"). */}
      <section className="preview__enrich" aria-label="Add context">
        <h4>Make context richer</h4>
        <div className="preview__enrich-row">
          <button className="btn" onClick={addSelection}>
            Add selection
          </button>
          <button className="btn" onClick={addScreenSummary}>
            Add screen summary
          </button>
          <button className="btn" onClick={addScreenshot}>
            Add screenshot (opt-in)
          </button>
        </div>

        <label className="preview__history">
          <span>Add more history: {historyMinutes} min</span>
          <input
            type="range"
            min={0}
            max={240}
            step={15}
            value={historyMinutes}
            onChange={(e) => applyHistoryRange(Number(e.target.value))}
          />
        </label>

        <div className="preview__freetext">
          <textarea
            placeholder="Add a note to the context…"
            value={freeText}
            onChange={(e) => setFreeText(e.target.value)}
          />
          <button className="btn" onClick={() => addUserText(freeText)}>
            Add note
          </button>
        </div>
      </section>

      {/* 5. Footer: transport target + health dot · size/token · Cancel/Send. */}
      <footer className="preview__foot">
        <div className="preview__transport">
          {/* TODO(M7:) the health dot color comes from a transport health query
              (reasoning::Health). Default to "setup" until wired. */}
          <span
            className="preview__health-dot"
            style={{ background: "var(--health-setup)" }}
            aria-label="transport health"
          />
          <span>{TRANSPORT_LABELS[payload.transport_target]}</span>
        </div>

        <div className={`preview__estimate ${overWarn ? "preview__estimate--warn" : ""}`}>
          {formatBytes(bytes)} · ~{tokenEstimate} tok
          {overWarn && <span title="Exceeds the 50 KB warn threshold (doc 09 §5)"> ⚠</span>}
        </div>

        <div className="preview__foot-actions">
          <button className="btn" onClick={() => onClose()} disabled={sending}>
            Cancel
          </button>
          <button
            className="btn btn--primary"
            onClick={() => void send()}
            disabled={sending || payload.items.length === 0}
          >
            {sending ? "Sending…" : "Send"}
          </button>
        </div>
      </footer>
    </div>
  );
}

/** Render the exact, human-readable content of one item for the expandable body. */
function renderItemBody(item: PayloadItem): string {
  switch (item.kind) {
    case "ocr_text":
      return `${item.redacted ? "[redacted] " : ""}${item.text}`;
    case "user_addition":
      return item.text;
    case "screenshot":
      return `screenshot ${item.width}×${item.height} (${item.data_b64.length} b64 chars)`;
    case "event_trail":
      return JSON.stringify(item.events, null, 2);
    case "connector":
      return `type: ${item.type}\n${JSON.stringify(item.payload, null, 2)}`;
  }
}

function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  return `${(b / 1024).toFixed(1)} KB`;
}
