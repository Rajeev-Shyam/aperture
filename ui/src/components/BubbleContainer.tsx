//! BubbleContainer (doc 11 §3, doc 14 §5). Owns the live set of bubbles, the
//  ≤3-visible hard cap (the Doc 14 performance cap IS the UX cap), the queue
//  overflow (lowest score dropped first), and the bottom-right stack layout.
//
//  Data in: the `bubble_spec` event stream + `suggestion_lifecycle` (the core
//  may drive expiry server-side, e.g. on staleness — doc 08 §5). On mount it
//  also pulls `list_suggestions` so a WebView2 respawn restores the queue from
//  SQLite (doc 11 §7).
//
//  Each visible bubble renders entering -> idle (20 s dwell, hover pauses) and
//  records user-driven transitions (dismiss / expiry / click) through the
//  `record_feedback` IPC so the durable suggestions row updates and the
//  engine's decay/mute ladder learns (doc 08 §7, SC7's data source).
//  Core-driven transitions arriving via `suggestion_lifecycle` are NOT
//  re-recorded — the core already knows.
//
//  TODO(M3-followup): the 👍/👎 "useful?" thumbs affordance (doc 11 §3, Q81) —
//  `recordFeedback(id, "up" | "down")` is wired and waiting. Also pending:
//  draggable stack + persisted position (doc 11 §3/§8, Q66).

import { useEffect, useRef, useState } from "react";

import {
  bubbleClick,
  getSettings,
  listSuggestions,
  onBubbleSpec,
  onSuggestionLifecycle,
  recordFeedback,
  type BubbleLifecycleState,
  type UnlistenFn,
} from "../lib/ipc";
import { admit, type BubbleInstance } from "../state/bubbleLifecycle";
import { Bubble } from "./Bubble";

interface Props {
  /** Open the Context-Preview panel for this bubble's `action_ref`. */
  onAskClaude: (actionRef: string) => void;
}

export function BubbleContainer({ onAskClaude }: Props) {
  const [bubbles, setBubbles] = useState<BubbleInstance[]>([]);
  // doc 11 §3 / doc 14 §5 hard cap; refreshed from settings on mount.
  const maxVisibleRef = useRef(3);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let cancelled = false;

    // Pull runtime caps from settings (ui.max_concurrent_bubbles).
    void getSettings().then((s) => {
      const n = s.ui?.max_concurrent_bubbles;
      if (typeof n === "number" && n > 0) maxVisibleRef.current = n;
    });

    // Restore any queued suggestions surviving in SQLite (doc 11 §7).
    void listSuggestions().then((specs) => {
      if (cancelled) return;
      setBubbles((cur) =>
        specs.reduce((acc, e) => admit(acc, { id: e.id, spec: e.spec }, maxVisibleRef.current), cur),
      );
    });

    // New suggestions arriving from the pipeline (doc 08 §6 -> doc 11 §3).
    void onBubbleSpec((e) => {
      setBubbles((cur) => admit(cur, { id: e.id, spec: e.spec }, maxVisibleRef.current));
    }).then((u) => (cancelled ? u() : unlisteners.push(u)));

    // Core-driven lifecycle transitions (e.g. server-side expiry on staleness).
    void onSuggestionLifecycle((e) => {
      applyLifecycle(e.id, e.state);
    }).then((u) => (cancelled ? u() : unlisteners.push(u)));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Apply an external lifecycle transition (or remove on terminal `exit`). */
  function applyLifecycle(id: string, state: BubbleLifecycleState) {
    setBubbles((cur) => {
      if (state === "exit") return cur.filter((b) => b.id !== id);
      return cur.map((b) => (b.id === id ? { ...b, state } : b));
    });
  }

  /** A bubble finished its exit animation -> drop it and promote a queued one. */
  function removeBubble(id: string) {
    setBubbles((cur) => {
      const rest = cur.filter((b) => b.id !== id);
      // Promote the highest-scored queued bubble into the freed visible slot.
      const queued = rest
        .filter((b) => b.state === "queued")
        .sort((a, b) => b.score - a.score);
      const visibleCount = rest.filter((b) => b.state !== "queued").length;
      if (queued.length && visibleCount < maxVisibleRef.current) {
        const promote = queued[0].id;
        return rest.map((b) => (b.id === promote ? { ...b, state: "entering" } : b));
      }
      return rest;
    });
  }

  function onResume(b: BubbleInstance) {
    // Core resolves action_ref -> connector -> open (Critical Path B, M4).
    // The durable clicked-state + engine reinforcement go through
    // record_feedback (bubble_click owns only the outcome column).
    // TODO(M4-followup): swap to fallback copy in-bubble on a Failed outcome
    // (doc 10 §6) instead of logging — lands with the thumbs/drag UI pass.
    bubbleClick(b.id, b.spec.action_ref)
      .then((outcome) => {
        if (outcome !== "Resumed") {
          console.warn("resume degraded/failed (doc 10 §6):", outcome);
        }
      })
      .catch((e) => console.error("bubble_click failed:", e));
    void recordFeedback(b.id, "clicked");
    applyLifecycle(b.id, "clicked");
  }

  // Only render visible (non-queued) bubbles; the stack is bottom-right,
  // column-reverse so the newest sits on top (CSS in bubble.css, doc 11 §3).
  const visible = bubbles.filter((b) => b.state !== "queued");

  return (
    <div className="bubble-stack" aria-live="polite">
      {visible.map((b, i) => (
        <Bubble
          key={b.id}
          instance={b}
          // ADR-039/C4 (R2): ≤2 glass surfaces; the 3rd+ visible renders opaque.
          opaque={i >= 2}
          onResume={() => onResume(b)}
          onDismiss={() => {
            // User-driven: persist + teach the ladder (doc 08 §7) so the
            // bubble cannot resurrect on a WebView respawn.
            void recordFeedback(b.id, "dismissed");
            applyLifecycle(b.id, "dismissed");
          }}
          onExited={() => removeBubble(b.id)}
          onLifecycle={(state) => {
            // Dwell expiry originates in this WebView -> record it once.
            if (state === "expired") void recordFeedback(b.id, "expired");
            applyLifecycle(b.id, state);
          }}
          onAskClaude={() => onAskClaude(b.spec.action_ref)}
        />
      ))}
    </div>
  );
}
