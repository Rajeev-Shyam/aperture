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

import { DEFAULT_GLASS_CAP } from "./glassBudget";
import type { BubbleSpec, BubbleLifecycleState, Settings } from "../lib/ipc";

/** Tunables (mirror config/settings.default.json `ui` block). Overridable so the
 *  container can apply runtime settings from `get_settings`. */
export const DEFAULTS = {
  /** doc 14 §4: scale .96->1 + fade. */
  enterMs: 180,
  /** doc 11 §3 (R2/Q65: 12s → 20s): idle dwell before mild-decay expiry. The
   *  live value comes from `ui.bubble_dwell_sec` (decision #7); this is the
   *  fallback used when settings have not been read yet. */
  dwellMs: 20_000,
  /** doc 14 §4: exit fade + 4px translate-down. */
  exitMs: 180,
  /** doc 11 §3 / doc 14 §5: the hard visible cap (`ui.max_concurrent_bubbles`). */
  maxVisible: 3,
  /** Half-life of the freshness factor in the admission score (decision #5).
   *  10 min is chosen against the engine's own cadence: the global cap is
   *  2-8 suggestions/hour, so bubbles arrive minutes apart, and at this
   *  half-life a 10-minute-old 0.95-confidence bubble (0.475) correctly yields
   *  its slot to a brand-new 0.7 one. */
  freshnessHalfLifeMs: 10 * 60_000,
} as const;

/** The runtime-tunable subset of the above, resolved from settings. */
export interface BubbleTuning {
  maxVisible: number;
  glassCap: number;
  dwellMs: number;
  freshnessHalfLifeMs: number;
}

/** Clamp a settings number into a sane band, falling back on anything absent,
 *  non-finite, or out of range — same posture as the core's `EngineConfig`:
 *  a typo in settings must never be able to break the UI's caps. */
function num(v: unknown, lo: number, hi: number, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) && v >= lo && v <= hi ? v : fallback;
}

/** Resolve the live bubble tunables from a `get_settings` response (decision
 *  #7 for the dwell; the caps were already settings-driven but read only at
 *  mount). Pure, so the container can re-resolve on every `settings_changed`. */
export function tuningFromSettings(s: Settings | null | undefined): BubbleTuning {
  const ui = s?.ui;
  return {
    maxVisible: num(ui?.max_concurrent_bubbles, 1, 6, DEFAULTS.maxVisible),
    // 0 is meaningful here (every surface opaque), so the band starts at 0.
    glassCap: num(ui?.max_glass_surfaces, 0, 6, DEFAULT_GLASS_CAP),
    // 3 s floor: below the 180 ms enter + read time a bubble is just a flash.
    dwellMs: num(ui?.bubble_dwell_sec, 3, 600, DEFAULTS.dwellMs / 1000) * 1000,
    freshnessHalfLifeMs:
      num(ui?.bubble_freshness_half_life_sec, 10, 86_400, DEFAULTS.freshnessHalfLifeMs / 1000) *
      1000,
  };
}

/** Reason a bubble left `idle` — distinguishes the feedback event written. */
export type Resolution = "clicked" | "dismissed" | "expired";

/** One live bubble instance, owned by BubbleContainer. */
export interface BubbleInstance {
  id: string;
  spec: BubbleSpec;
  state: BubbleLifecycleState;
  /** Score used to drop the lowest when >3 are visible (doc 11 §3). */
  score: number;
  /** Fallback copy after a Resume that did not open (doc 10 §6, SDLC review
   *  2026-08-19 finding 3b): rendered in place of the sublabel, with Resume
   *  withdrawn. The bubble stays — its dwell keeps running and Dismiss still
   *  records `dismissed`; only `clicked` is never recorded for it. */
  fallback?: string;
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
 * The owning Bubble constructs one when it enters `idle` — i.e. after the 180 ms
 * enter animation — so the configured dwell is honest (doc 11 §3, R2/Q65).
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
 * Slot-admission score (owner decision #5, 2026-08-16): **freshness ×
 * confidence**, replacing the confidence-only proxy the code's own comment
 * flagged as a placeholder.
 *
 * Confidence alone gave a queued bubble a permanent rank: a 0.95 suggestion
 * from an hour ago outranked every 0.8 one arriving now, forever, so the queue
 * could hand a freed slot to something the user had already moved on from.
 * Freshness decays exponentially (`0.5^(age / halfLife)`) rather than cutting
 * off at a threshold — a cliff would just move the arbitrariness rather than
 * remove it, and the *hard* freshness rule (state past its TTL ⇒ no candidate
 * at all) already lives core-side in doc 08 §5.
 *
 * An unknown `created_ts` scores as perfectly fresh: it means a row written
 * before the column existed, and penalizing it would silently bury the whole
 * pre-upgrade queue.
 */
export function admissionScore(
  spec: BubbleSpec,
  nowMs: number,
  halfLifeMs: number = DEFAULTS.freshnessHalfLifeMs,
): number {
  const created = spec.created_ts;
  if (created == null || halfLifeMs <= 0) return spec.confidence;
  // Clamp negative ages: a clock adjustment must not mint a >1 freshness.
  const ageMs = Math.max(0, nowMs - created);
  return spec.confidence * Math.pow(0.5, ageMs / halfLifeMs);
}

/**
 * Fill every free visible slot with the highest-scoring queued bubbles, and
 * fill no more than that.
 *
 * This is also the fix for a cap violation (found while wiring decision #5):
 * `admit` used to sort the whole list and promote anything landing in the first
 * `maxVisible` positions, but bubbles already visible were never demoted — so a
 * high-scoring arrival while 3 were on screen produced a **4th** visible bubble,
 * breaking both the doc 11 §3 UX cap and the doc 14 §5 performance cap (the
 * glass budget then rendered 2 glass + 2 opaque).
 *
 * Visible bubbles are deliberately left in place rather than re-ranked: a
 * bubble that has earned a slot keeps it for its dwell. Re-sorting the visible
 * set on every arrival made existing bubbles jump position and swap between
 * glass and opaque under the user's cursor.
 */
export function promote(list: BubbleInstance[], maxVisible: number): BubbleInstance[] {
  const free = maxVisible - list.filter((b) => b.state !== "queued").length;
  if (free <= 0) return list;
  const winners = new Set(
    list
      .filter((b) => b.state === "queued")
      .sort((a, b) => b.score - a.score)
      .slice(0, free)
      .map((b) => b.id),
  );
  return winners.size === 0
    ? list
    : list.map((b) => (winners.has(b.id) ? { ...b, state: "entering" } : b));
}

/**
 * Pure transition helper: given the current instances and an incoming
 * `BubbleSpec`, return the next list honoring the ≤`maxVisible` cap (doc 11 §3,
 * doc 14 §5) — excess stays `queued`, highest score promoted first.
 *
 * Queued bubbles are RE-SCORED on every admission, because their freshness has
 * moved since they were queued; that is the whole point of decision #5. Order
 * in the array stays arrival order — the stack is `column-reverse`, so the
 * newest renders on top without disturbing anything already placed.
 */
export function admit(
  current: BubbleInstance[],
  incoming: { id: string; spec: BubbleSpec },
  maxVisible: number,
  nowMs: number = Date.now(),
  halfLifeMs: number = DEFAULTS.freshnessHalfLifeMs,
): BubbleInstance[] {
  const arrival: BubbleInstance = {
    id: incoming.id,
    spec: incoming.spec,
    state: "queued",
    score: admissionScore(incoming.spec, nowMs, halfLifeMs),
  };
  const next: BubbleInstance[] = [
    ...current.filter((b) => b.id !== incoming.id),
    arrival,
  ].map((b) =>
    b.state === "queued" ? { ...b, score: admissionScore(b.spec, nowMs, halfLifeMs) } : b,
  );

  return promote(next, maxVisible);
}
