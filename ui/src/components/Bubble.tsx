//! A single Bubble (doc 11 §3 anatomy, doc 14 §3/§4 recipe + states).
//
//  Anatomy: glyph · title · sublabel · Resume (primary) · dismiss (×) · overflow
//  (⋯ → "Ask Claude about this" / "Mute this pattern" / "Exclude this app").
//
//  Lifecycle (doc 11 §3): queued ─► entering(180ms) ─► idle(dwell 12s, hover
//  pauses) ─► clicked/dismissed/expired ─► exit. Hover PAUSES the dwell; this
//  component owns the DwellTimer for its own idle phase and reports every
//  transition up via `onLifecycle` (so the container fans it to the core as the
//  `suggestion_*` feedback event, SC7's source).
//
//  Only `opacity`/`transform` animate (doc 14 §4); `backdrop-filter` is never
//  animated. Under `gpu_busy` the CSS strips blur + collapses motion to fades.

import { useEffect, useRef, useState } from "react";

import type { BubbleLifecycleState } from "../lib/ipc";
import { DEFAULTS, DwellTimer, type BubbleInstance } from "../state/bubbleLifecycle";

interface Props {
  instance: BubbleInstance;
  onResume: () => void;
  onDismiss: () => void;
  /** Fired after the exit animation completes — container drops the bubble. */
  onExited: () => void;
  /** Report a self-driven transition (e.g. entering->idle, expired). */
  onLifecycle: (state: BubbleLifecycleState) => void;
  /** Overflow → "Ask Claude about this" opens the Context-Preview panel. */
  onAskClaude: () => void;
}

export function Bubble({
  instance,
  onResume,
  onDismiss,
  onExited,
  onLifecycle,
  onAskClaude,
}: Props) {
  const { spec, state } = instance;
  const [overflowOpen, setOverflowOpen] = useState(false);
  const dwellRef = useRef<DwellTimer | null>(null);

  // entering -> idle after the 180ms enter animation, then start the dwell.
  useEffect(() => {
    if (state !== "entering") return;
    const t = setTimeout(() => onLifecycle("idle"), DEFAULTS.enterMs);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state]);

  // idle: run the 12s dwell; expiry -> mild-decay exit (doc 11 §3).
  useEffect(() => {
    if (state !== "idle") return;
    const timer = new DwellTimer(() => onLifecycle("expired"), DEFAULTS.dwellMs);
    dwellRef.current = timer;
    timer.start();
    return () => {
      timer.cancel();
      dwellRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state]);

  // clicked/dismissed/expired -> play the exit, then notify the container.
  useEffect(() => {
    if (state !== "clicked" && state !== "dismissed" && state !== "expired") return;
    // Show the brief active/decay frame, then transition to exit.
    const toExit = setTimeout(() => onLifecycle("exit"), DEFAULTS.exitMs);
    return () => clearTimeout(toExit);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state]);

  useEffect(() => {
    if (state !== "exit") return;
    const done = setTimeout(onExited, DEFAULTS.exitMs);
    return () => clearTimeout(done);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state]);

  // Hover pauses the dwell countdown (doc 11 §3).
  const onMouseEnter = () => dwellRef.current?.pause();
  const onMouseLeave = () => dwellRef.current?.resume();

  // Map lifecycle state -> the doc 14 §4 visual class.
  const stateClass =
    state === "entering"
      ? "bubble--entering"
      : state === "exit" || state === "dismissed" || state === "expired"
        ? "bubble--exit"
        : state === "clicked"
          ? "bubble--clicked"
          : "bubble--idle";

  return (
    <div
      className={`bubble surface-glass surface-interactive ${stateClass}`}
      role="group"
      aria-label={spec.title}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
    >
      <div className="bubble__glyph" aria-hidden>
        {spec.glyph}
      </div>

      <div className="bubble__title" title={spec.title}>
        {spec.title}
        {spec.source === "claude" && <span className="bubble__source-tag">via Claude</span>}
      </div>

      {spec.sublabel && <div className="bubble__sublabel">{spec.sublabel}</div>}

      <div className="bubble__actions">
        <button className="btn btn--primary" onClick={onResume}>
          Resume
        </button>
        <button className="btn btn--icon" aria-label="Dismiss" onClick={onDismiss}>
          ×
        </button>
        <button
          className="btn btn--icon"
          aria-label="More actions"
          aria-haspopup="menu"
          aria-expanded={overflowOpen}
          onClick={() => setOverflowOpen((v) => !v)}
        >
          ⋯
        </button>
      </div>

      {overflowOpen && (
        <div className="bubble__overflow surface-glass" role="menu">
          <button
            role="menuitem"
            onClick={() => {
              setOverflowOpen(false);
              onAskClaude();
            }}
          >
            Ask Claude about this
          </button>
          <button
            role="menuitem"
            onClick={() => {
              setOverflowOpen(false);
              // TODO(M3:) invoke a mute-pattern command (feedback -> doc 08 §7).
              onDismiss();
            }}
          >
            Mute this pattern
          </button>
          <button
            role="menuitem"
            onClick={() => {
              setOverflowOpen(false);
              // TODO(M9:) invoke an exclude-app command (-> exclusion list, doc 13 §4).
              onDismiss();
            }}
          >
            Exclude this app
          </button>
        </div>
      )}
    </div>
  );
}
