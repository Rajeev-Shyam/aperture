//! BubbleContainer (doc 11 §3, doc 14 §5). Owns the live set of bubbles, the
//  ≤3-visible hard cap (the Doc 14 performance cap IS the UX cap), the queue
//  overflow (lowest score dropped first), and the bottom-right stack layout.
//
//  Data in: the `bubble_spec` event stream + `suggestion_lifecycle` (the core
//  may drive expiry server-side, e.g. on staleness — doc 08 §5). On mount it
//  also pulls `list_suggestions` so a WebView2 respawn restores the queue from
//  SQLite (doc 11 §7).
//
//  Each visible bubble renders entering -> idle (12s dwell, hover pauses) and
//  emits the matching `suggestion_*` feedback via `suggestion_lifecycle`
//  (SC7's data source). The actual feedback-event write happens in the core when
//  it receives `bubble_click` / the lifecycle signal.

import { useEffect, useRef, useState } from "react";

import {
  bubbleClick,
  getSettings,
  listSuggestions,
  onBubbleSpec,
  onSuggestionLifecycle,
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
    // Core resolves action_ref -> connector -> open AND records suggestion_clicked.
    void bubbleClick(b.id, b.spec.action_ref);
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
          onDismiss={() => applyLifecycle(b.id, "dismissed")}
          onExited={() => removeBubble(b.id)}
          onLifecycle={(state) => applyLifecycle(b.id, state)}
          onAskClaude={() => onAskClaude(b.spec.action_ref)}
        />
      ))}
    </div>
  );
}
