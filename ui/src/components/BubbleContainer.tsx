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
//  Slot admission is freshness × confidence (owner decision #5) and the visible
//  cap is enforced in one place (`promote`), shared by arrivals and by the
//  freed-slot path. Ratings live here rather than in each Bubble so a thumb
//  pressed on one monitor lights on all of them (decision #10), and the
//  tunables re-read on `settings_changed` so the Dashboard's dwell control is
//  live (decision #7).
//
//  TODO(M3-followup): draggable stack + persisted position (doc 11 §3/§8, Q66).

import { useEffect, useRef, useState } from "react";

import {
  bubbleClick,
  getSettings,
  listSuggestions,
  onBubbleSpec,
  onSettingsChanged,
  onSuggestionLifecycle,
  onSuggestionRated,
  recordFeedback,
  type BubbleLifecycleState,
  type UnlistenFn,
} from "../lib/ipc";
import {
  admit,
  promote,
  tuningFromSettings,
  type BubbleInstance,
  type BubbleTuning,
} from "../state/bubbleLifecycle";
import { isOpaqueForBudget } from "../state/glassBudget";
import { Bubble } from "./Bubble";

interface Props {
  /** Open the Context-Preview panel for this bubble's `action_ref`. */
  onAskClaude: (actionRef: string) => void;
}

export function BubbleContainer({ onAskClaude }: Props) {
  const [bubbles, setBubbles] = useState<BubbleInstance[]>([]);
  // The recorded 👍/👎 per suggestion id. Held HERE, not in each Bubble, so a
  // thumb pressed on one monitor lights on all of them (decision #10) and
  // survives the Bubble remounting.
  const [ratings, setRatings] = useState<Record<string, "up" | "down">>({});
  // Live tunables (decision #7): caps + dwell + the freshness half-life. In a
  // ref because the event handlers below are registered once and must read the
  // CURRENT value; mirrored into state so a change re-renders the dwell prop.
  const [tuning, setTuning] = useState<BubbleTuning>(() => tuningFromSettings(null));
  const tuningRef = useRef(tuning);
  tuningRef.current = tuning;

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let cancelled = false;

    const readTuning = () =>
      getSettings()
        .then((s) => {
          if (!cancelled) setTuning(tuningFromSettings(s));
        })
        .catch(() => {
          // Keep the previous (or default) tunables — never fall back to
          // something wider than the caps the user configured.
        });
    void readTuning();

    // Restore any queued suggestions surviving in SQLite (doc 11 §7). Reversed:
    // `list_suggestions` returns newest-first, and the stack is column-reverse,
    // so admitting oldest-first puts the newest visually on top — the same
    // order a live session produces.
    void listSuggestions().then((specs) => {
      if (cancelled) return;
      const now = Date.now();
      setBubbles((cur) =>
        [...specs]
          .reverse()
          .reduce(
            (acc, e) =>
              admit(
                acc,
                { id: e.id, spec: e.spec },
                tuningRef.current.maxVisible,
                now,
                tuningRef.current.freshnessHalfLifeMs,
              ),
            cur,
          ),
      );
    });

    // New suggestions arriving from the pipeline (doc 08 §6 -> doc 11 §3).
    void onBubbleSpec((e) => {
      setBubbles((cur) =>
        admit(
          cur,
          { id: e.id, spec: e.spec },
          tuningRef.current.maxVisible,
          Date.now(),
          tuningRef.current.freshnessHalfLifeMs,
        ),
      );
    }).then((u) => (cancelled ? u() : unlisteners.push(u)));

    // Core-driven lifecycle transitions (e.g. server-side expiry on staleness)
    // AND the cross-monitor convergence broadcast for user-driven ones.
    void onSuggestionLifecycle((e) => {
      applyLifecycle(e.id, e.state);
    }).then((u) => (cancelled ? u() : unlisteners.push(u)));

    // A thumb was recorded — on this window or another (decision #10).
    void onSuggestionRated((e) => {
      setRatings((cur) => ({ ...cur, [e.id]: e.rating }));
    }).then((u) => (cancelled ? u() : unlisteners.push(u)));

    // Settings were written (decision #7): re-read rather than wait for a
    // restart. Only `ui` matters here.
    void onSettingsChanged((e) => {
      if (e.sections.includes("ui")) void readTuning();
    }).then((u) => (cancelled ? u() : unlisteners.push(u)));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A raised cap must fill its new slots immediately, not on the next arrival.
  useEffect(() => {
    setBubbles((cur) => promote(cur, tuning.maxVisible));
  }, [tuning.maxVisible]);

  /**
   * Apply a lifecycle transition; on terminal `exit`, drop the bubble and fill
   * the slot it just freed with the highest-scoring queued bubble.
   *
   * Promotion belongs HERE, on the only path that actually runs. It used to
   * live in a separate `removeBubble` wired to the Bubble's `onExited`, which
   * never fired: `onLifecycle("exit")` removes the bubble from this list, so
   * the Bubble unmounts and the effect that would have called `onExited` is
   * torn down first. That was invisible while a cap bug in `admit` let every
   * arrival become visible immediately — with the cap now enforced, bubbles
   * really do queue, and a queue that is never drained is a bubble that never
   * appears at all.
   */
  function applyLifecycle(id: string, state: BubbleLifecycleState) {
    setBubbles((cur) =>
      state === "exit"
        ? promote(
            cur.filter((b) => b.id !== id),
            tuningRef.current.maxVisible,
          )
        : cur.map((b) => (b.id === id ? { ...b, state } : b)),
    );
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
          // ADR-039/C4 (R2): the glass-surface budget; the 3rd+ visible renders
          // opaque (cap from ui.max_glass_surfaces, default 2 — doc 14 §5).
          opaque={isOpaqueForBudget(i, tuning.glassCap)}
          rated={ratings[b.id] ?? null}
          dwellMs={tuning.dwellMs}
          onResume={() => onResume(b)}
          onDismiss={() => {
            // User-driven: persist + teach the ladder (doc 08 §7) so the
            // bubble cannot resurrect on a WebView respawn. The core
            // re-broadcasts the transition so every monitor converges.
            void recordFeedback(b.id, "dismissed");
            applyLifecycle(b.id, "dismissed");
          }}
          onMute={() => {
            // "Mute this pattern": straight to the engine's 7-day mute
            // (doc 08 §7); the bubble resolves as dismissed.
            void recordFeedback(b.id, "muted");
            applyLifecycle(b.id, "dismissed");
          }}
          onRate={(kind) => {
            // The explicit "useful?" thumbs — SC7's signal (Q81). The bubble
            // stays; only the rating is recorded. Echo locally for instant
            // feedback; the core's `suggestion_rated` broadcast then converges
            // every other monitor (decision #10).
            setRatings((cur) => ({ ...cur, [b.id]: kind }));
            void recordFeedback(b.id, kind);
          }}
          // Belt-and-braces: if the exit transition above ever stops removing
          // the bubble, the animation's own completion still retires it.
          onExited={() => applyLifecycle(b.id, "exit")}
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
