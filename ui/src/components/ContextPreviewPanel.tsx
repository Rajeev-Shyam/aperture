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
  listTrailEvents,
  previewRetarget,
  previewSend,
  previewSetApproved,
  transportHealth,
  PAYLOAD_SIZE_WARN_BYTES,
  type ContextPayload,
  type Health,
  type Intent,
  type PayloadItem,
  type StructuredSuggestions,
  type TransportTarget,
} from "../lib/ipc";
import { useDraggable } from "../state/useDraggable";
import { useModalSurface } from "../state/useModalSurface";

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

  // Real transport health for the footer dot (was hardcoded "setup" yellow,
  // 2026-08-15 review). Queried once per open per transport.
  const [health, setHealth] = useState<Health | null>(null);
  useEffect(() => {
    let cancelled = false;
    setHealth(null);
    void transportHealth(payload.transport_target)
      .then((h) => {
        if (!cancelled) setHealth(h);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [payload.transport_target]);

  // `aria-modal` must be backed by the real modal contract (this is the ONE gate
  // where the user reviews exactly what egresses): make the click-through
  // overlay accept input, move focus in on open, restore it to the opener on
  // close, trap Tab, and map Escape to Cancel — the zero-residue safe path
  // (doc 13 §3). Without the overlay half, Send/Cancel are literally unclickable.
  // Non-exclusive: the panel's own rect keeps Send/Cancel clickable while the
  // rest of the screen stays click-through to the user's apps.
  const trapKeys = useModalSurface(panelRef, { exclusive: false });
  // Window-style drag by the header.
  const drag = useDraggable(panelRef);

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Escape") {
      e.preventDefault();
      // Cancel — drop everything, zero residue (doc 13 §3). Gated on `sending`
      // exactly like the Cancel button (08-22 review): mid-send the session is
      // already with the gateway, and closing here would cancel a payload that
      // may be on the wire. The core tombstones a cancel that races a send
      // anyway (preview_cancel), but the panel must not invite the race.
      if (!sending) onClose();
      return;
    }
    trapKeys(e);
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

  // "Add selection" / "Add screen summary" / "Add screenshot" are DISABLED with
  // honest copy: their capture paths land with v2's screen-serializer (Doc 22
  // §3.2). They used to render enabled — one injected a literal
  // "[selection pending]" placeholder that could ship inside an approved
  // payload; the others were silent no-ops (2026-08-15 review). The trust
  // surface must never fake content or claim affordances it doesn't have.

  /** Extend/shrink the event_trail over the selected range (ADR-040/Q71):
   *  fetch the metadata rows and swap the payload's `event_trail` item —
   *  WYSIWYS, and approval re-runs redaction over the result. */
  function applyHistoryRange(minutes: number) {
    setHistoryMinutes(minutes);
    if (minutes === 0) return; // 0 = keep the trail the builder assembled
    void listTrailEvents(minutes)
      .then((events) => {
        onChange({
          ...payload,
          items: [
            ...payload.items.filter((i) => i.kind !== "event_trail"),
            ...(events.length ? [{ kind: "event_trail", events } as PayloadItem] : []),
          ],
        });
      })
      .catch((e) => setSendError(`history range failed: ${e}`));
  }

  // ---- Send / Approve (over the EXACT object the panel holds) --------------
  const [sendError, setSendError] = useState<string | null>(null);
  // The push path is bound to the transport the footer named (SDLC review
  // 2026-08-19 finding 1): when it is not ready the core sends nothing and
  // reports what it WOULD have used. The user re-targets explicitly — the
  // approval is dropped core-side, so the next Send is a fresh, informed one
  // against a footer that now names the real transport.
  const [mismatch, setMismatch] = useState<{
    named: TransportTarget;
    available: TransportTarget | null;
  } | null>(null);

  // A const so the narrowing survives into the button's click closure.
  const alternative: TransportTarget | null = mismatch?.available ?? null;

  // MCP is PULL (doc 09 §3): approving releases the payload to Claude Desktop
  // when IT calls `aperture_get_context` — there is no push transmission.
  const isMcpPull = payload.transport_target === "claude-desktop-mcp";

  async function send() {
    if (sending) return;
    setSending(true);
    setSendError(null);
    setMismatch(null);
    try {
      // Contract law: ONLY this panel sets approval (doc 15 §2b). The core
      // syncs the edits, re-runs redaction, and binds approval to the bytes.
      const approval = await previewSetApproved(payload);
      onChange(approval.payload);
      if (approval.changed) {
        // Redaction altered content the user hasn't seen — show the real wire
        // bytes and require a second, informed approval (doc 13 §3).
        setSendError(
          `Redaction changed the content — review the updated items, then press ${
            isMcpPull ? "Approve for Claude" : "Send"
          } again.`,
        );
        return;
      }
      if (isMcpPull) {
        // Approved: Claude Desktop's next `aperture_get_context` call receives
        // exactly these bytes (audited core-side). Nothing is pushed from here.
        // Close WITH an (empty) outcome — a bare close is Cancel in App.tsx and
        // would previewCancel() the approval we just made (review 2026-08-14).
        onClose({});
        return;
      }
      // Transmit the approved object (hash-bound core-side, doc 03 §4).
      const result = await previewSend(approval.payload.payload_id);
      if (result.kind === "transport_mismatch") {
        // Nothing left the machine. Name the gap and offer the real route
        // (finding 1) — never silently fall through to another transport.
        setMismatch({ named: result.named, available: result.available });
        return;
      }
      onClose(result.suggestions);
    } catch (e) {
      // A failed transport leaves the approval retryable core-side; say so
      // instead of dead-ending silently.
      setSendError(String(e));
    } finally {
      setSending(false);
    }
  }

  /** Re-stamp the payload onto the transport the core said IS available. The
   *  core drops the approval, so this never sends: the user reviews the
   *  footer's new transport label and presses Send again (finding 1). */
  const [retargeting, setRetargeting] = useState(false);
  async function retarget(target: TransportTarget) {
    if (retargeting || sending) return;
    setRetargeting(true); // its own flag: the Send button must not read "Sending…"
    try {
      const updated = await previewRetarget(payload.payload_id, target);
      // Merge ONLY the transport. The core re-stamps `transport_target` and
      // drops the approval, nothing else — but its copy of the items is the
      // last-APPROVED set, so swapping in the whole returned payload would
      // silently resurrect any item the user removed after the mismatch
      // (08-22 review). `preview_set_approved` re-syncs the panel's items
      // from `payload` at the next Send anyway.
      onChange({ ...payload, transport_target: updated.transport_target });
      setMismatch(null);
      setSendError(`Now set to ${TRANSPORT_LABELS[target]}. Review and press Send again.`);
    } catch (e) {
      setSendError(String(e));
    } finally {
      setRetargeting(false);
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
      style={drag.style}
    >
      {/* 1. Intent (editable preset) */}
      <header className="preview__head panel-handle" {...drag.handleProps}>
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

      {/* 4. Enrichment affordances ("make context richer"). The three capture
          paths ship with v2's screen-serializer — disabled, never fake. */}
      <section className="preview__enrich" aria-label="Add context">
        <h4>Make context richer</h4>
        <div className="preview__enrich-row">
          <button className="btn" disabled title="Coming with v2 — selection capture isn't built yet">
            Add selection (v2)
          </button>
          <button className="btn" disabled title="Coming with v2 — local VLM scene summary isn't wired yet">
            Add screen summary (v2)
          </button>
          <button className="btn" disabled title="Coming with v2 — screenshot capture isn't wired yet">
            Add screenshot (v2)
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
          <span
            className="preview__health-dot"
            style={{
              background:
                health?.kind === "ready"
                  ? "var(--health-ready)"
                  : health?.kind === "unavailable"
                    ? "var(--health-down)"
                    : "var(--health-setup)",
            }}
            aria-label={`transport health: ${health?.kind ?? "checking"}`}
            title={health && health.kind !== "ready" ? health.detail : undefined}
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
            {sending
              ? isMcpPull
                ? "Approving…"
                : "Sending…"
              : isMcpPull
                ? "Approve for Claude"
                : "Send"}
          </button>
        </div>
      </footer>
      {sendError && (
        <p className="preview__send-error" role="alert">
          {sendError}
        </p>
      )}
      {/* Finding 1: the named transport was not ready, so nothing was sent.
          The alternative is offered, never taken on the user's behalf. */}
      {mismatch && (
        <div className="preview__send-error" role="alert">
          <span>{TRANSPORT_LABELS[mismatch.named]} isn't available right now. </span>
          {alternative !== null ? (
            <button
              className="btn"
              onClick={() => void retarget(alternative)}
              disabled={retargeting || sending}
            >
              Send via {TRANSPORT_LABELS[alternative]} instead
            </button>
          ) : (
            <span>No push transport is available — check the Advanced tab.</span>
          )}
        </div>
      )}
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
