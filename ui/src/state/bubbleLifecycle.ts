//! Bubble lifecycle state machine + 20s dwell timer (doc 11 §3 R2/Q65, doc 14 §4).
//
//  The machine drives one bubble through:
//      queued ─► entering(180ms) ─► idle(dwell 20s, hover pauses)
//                                      ├─► clicked   ─► resolving ─► exit
//                                      ├─► dismissed ─► exit
//                                      └─► expired   ─► exit
//
//  Every transition writes the corresponding `suggestion_*` feedback event
//  (SC7's data source, doc 11 §3) — emitted here via `onLifecycle` so the owning
//  component can fan it to the Rust core. This module is framework-agnostic
//  (plain timers + a reducer) so it is unit-testable without React.
//
//  Hover PAUSES the dwell countdown (doc 11 §3): the remaining time is banked on
//  `pauseDwell()` and resumed on `resumeDwell()`.

import type { BubbleSpec, BubbleLifecycleState } from "../lib/ipc";

/** Tunables (mirror config/settings.default.json `ui` block). Overridable so the
 *  container can apply runtime settings from `get_settings`. */
export const DEFAULTS = {
  /** doc 14 §4: scale .96->1 + fade. */
  enterMs: 180,
  /** doc 11 §3 (R2/Q65: 12s → 20s): idle dwell before mild-decay expiry. */
  dwellMs: 20_000,
  /** doc 14 §4: exit fade + 4px translate-down. */
  exitMs: 180,
} as const;

/** Reason a bubble left `idle` — distinguishes the feedback event written. */
export type Resolution = "clicked" | "dismissed" | "expired";

/** One live bubble instance, owned by BubbleContainer. */
export interface BubbleInstance {
  id: string;
  spec: BubbleSpec;
  state: BubbleLifecycleState;
  /** Score used to drop the lowest when >3 are visible (doc 11 §3). */
  score: number;
}

/** Maps a resolution to its lifecycle state (and, by the container, to the
 *  `suggestion_*` event name). */
export function resolutionState(r: Resolution): BubbleLifecycleState {
  switch (r) {
    case "clicked":
      return "clicked";
    case "dismissed":
      return "dismissed";
    case "expired":
      return "expired";
  }
}

/**
 * A self-contained dwell timer with hover-pause semantics. One per visible
 * bubble. Owns no UI; fires `onExpire` when the (pausable) countdown elapses.
 *
 * TODO(M3:) the container constructs one of these when a bubble enters `idle`;
 *           it must NOT start until the 180ms enter animation completes so the
 *           20s dwell is honest (doc 11 §3, R2/Q65).
 */
export class DwellTimer {
  private remainingMs: number;
  private deadline = 0;
  private handle: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly onExpire: () => void,
    dwellMs: number = DEFAULTS.dwellMs,
  ) {
    this.remainingMs = dwellMs;
  }

  /** Start (or restart) counting down the banked remaining time. */
  start(): void {
    this.clear();
    this.deadline = Date.now() + this.remainingMs;
    this.handle = setTimeout(this.onExpire, this.remainingMs);
  }

  /** Hover entered: bank the remaining time and stop the clock (doc 11 §3). */
  pause(): void {
    if (this.handle === null) return;
    this.remainingMs = Math.max(0, this.deadline - Date.now());
    this.clear();
  }

  /** Hover left: resume from the banked remaining time. */
  resume(): void {
    if (this.handle !== null) return; // already running
    this.start();
  }

  /** Cancel entirely (bubble resolved by click/dismiss/exit). */
  cancel(): void {
    this.clear();
  }

  private clear(): void {
    if (this.handle !== null) {
      clearTimeout(this.handle);
      this.handle = null;
    }
  }
}

/**
 * Pure transition helper: given the current instances and an incoming
 * `BubbleSpec`, return the next list applying the ≤3-visible cap (doc 11 §3,
 * doc 14 §5) — excess stays `queued`, lowest score dropped first.
 *
 * TODO(M3:) wire the real scoring (freshness × confidence, doc 08 §5); for now
 *           confidence is the score proxy.
 */
export function admit(
  current: BubbleInstance[],
  incoming: { id: string; spec: BubbleSpec },
  maxVisible: number,
): BubbleInstance[] {
  const next: BubbleInstance[] = [
    ...current.filter((b) => b.id !== incoming.id),
    {
      id: incoming.id,
      spec: incoming.spec,
      state: "queued",
      score: incoming.spec.confidence,
    },
  ];

  // Highest score first; the first `maxVisible` enter, the rest stay queued.
  next.sort((a, b) => b.score - a.score);
  return next.map((b, i) => ({
    ...b,
    state: i < maxVisible && b.state === "queued" ? "entering" : b.state,
  }));
}
