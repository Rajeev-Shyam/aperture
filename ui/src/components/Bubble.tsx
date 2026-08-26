//! A single Bubble (doc 11 §3 anatomy, doc 14 §3/§4 recipe + states).
//
//  Anatomy: glyph · title · sublabel · Resume (primary) · dismiss (×) · overflow
//  (⋯ → "Ask Claude about this" / "Mute this pattern" / "Stop capturing X" /
//  "Exclusions…").
//
//  Lifecycle (doc 11 §3): queued ─► entering(180ms) ─► idle(dwell 20s, hover
//  pauses) ─► clicked/dismissed/expired ─► exit. Hover PAUSES the dwell; this
//  component owns the DwellTimer for its own idle phase and reports every
//  transition up via `onLifecycle` (so the container fans it to the core as the
//  `suggestion_*` feedback event, SC7's source).
//
//  Only `opacity`/`transform` animate (doc 14 §4); `backdrop-filter` is never
//  animated. Under `gpu_busy` the CSS strips blur + collapses motion to fades.

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import {
  addExclusion,
  listExclusions,
  openPrivacy,
  setExclusion,
  type BubbleLifecycleState,
  type ExclusionOffer,
} from "../lib/ipc";
import { DEFAULTS, DwellTimer, type BubbleInstance } from "../state/bubbleLifecycle";

/** Connector-type token → the mark rendered in the 28 px glyph chip.
 *
 *  `BubbleSpec.glyph` carries a SEMANTIC token ("video", "globe", …), which the
 *  suggestion generator also persists into the `suggestions` row — so the token
 *  is a stored value and the mark must be chosen here, at render, or a restored
 *  row would forever show whatever the mark was on the day it was written.
 *  (It was previously rendered verbatim, i.e. the literal word "video" inside a
 *  28 px box.) An unknown token falls back to the neutral mark rather than
 *  printing itself — v2 connectors (doc 15 §3) render without a UI change.
 *  [VERIFY — final set lands with the M8 design-token pass, doc 14.] */
const GLYPH_MARKS: Record<string, string> = {
  video: "▶",
  globe: "🌐",
  doc: "📄",
  code: "⌨",
  switch: "⇄",
  spark: "✦",
};

export function glyphMark(token: string): string {
  return GLYPH_MARKS[token] ?? GLYPH_MARKS.spark;
}

interface Props {
  instance: BubbleInstance;
  onResume: () => void;
  onDismiss: () => void;
  /** Overflow → "Mute this pattern": 7-day mute + dismiss (doc 08 §7). */
  onMute: () => void;
  /** The explicit "useful?" thumbs — the SC7 signal (doc 11 §3, Q81). */
  onRate: (kind: "up" | "down") => void;
  /** The rating recorded for this suggestion, from the container's shared map.
   *  It lives there rather than here so a thumb pressed on one monitor lights
   *  on all of them (decision #10) and survives this component remounting. */
  rated?: "up" | "down" | null;
  /** Idle dwell for THIS bubble, from `ui.bubble_dwell_sec` (decision #7).
   *  Captured when the bubble enters `idle`: changing the setting must not
   *  restart a countdown the user is already watching. */
  dwellMs?: number;
  /** Fired after the exit animation completes — container drops the bubble. */
  onExited: () => void;
  /** Report a self-driven transition (e.g. entering->idle, expired). */
  onLifecycle: (state: BubbleLifecycleState) => void;
  /** Overflow → "Ask Claude about this" opens the Context-Preview panel. */
  onAskClaude: () => void;
  /**
   * ADR-039/C4 (R2): at most 2 glass surfaces — the 3rd visible bubble renders
   * in the opaque fallback class (3 visible total; final cap at M8 PresentMon).
   */
  opaque?: boolean;
}

export function Bubble({
  instance,
  onResume,
  onDismiss,
  onMute,
  onRate,
  onExited,
  onLifecycle,
  onAskClaude,
  rated = null,
  dwellMs = DEFAULTS.dwellMs,
  opaque = false,
}: Props) {
  const { spec, state } = instance;
  const [overflowOpen, setOverflowOpen] = useState(false);
  // Decision #8: the applied rule, so the menu can confirm it and offer Undo.
  // `id` is the durable `exclusion_list` row id `add_exclusion` returned;
  // `prior` records what that row was BEFORE the click, which is what Undo has
  // to restore (see `applyExclusion`).
  const [excluded, setExcluded] = useState<{
    label: string;
    id: number;
    prior: "absent" | "disabled" | "enabled";
  } | null>(null);
  const [excludeError, setExcludeError] = useState<string | null>(null);
  // The overflow menu is PORTALLED to <body>: `.bubble` sets `contain: strict`
  // (bound rasterization, doc 14 §3), which clips any absolutely-positioned
  // descendant — so an in-tree menu never paints. We render it fixed-positioned
  // against the trigger's rect instead (review #1). It is opaque chrome, so it
  // never counts toward the ≤2-glass budget (review #21, ADR-039).
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [menuPos, setMenuPos] = useState<{ right: number; bottom: number } | null>(null);
  const dwellRef = useRef<DwellTimer | null>(null);

  function toggleOverflow() {
    setOverflowOpen((open) => {
      const next = !open;
      if (next && triggerRef.current) {
        const r = triggerRef.current.getBoundingClientRect();
        // Anchor above the ⋯ trigger, right-aligned (the stack sits bottom-right).
        setMenuPos({ right: window.innerWidth - r.right, bottom: window.innerHeight - r.top + 6 });
      }
      return next;
    });
  }

  // entering -> idle after the 180ms enter animation, then start the dwell.
  useEffect(() => {
    if (state !== "entering") return;
    const t = setTimeout(() => onLifecycle("idle"), DEFAULTS.enterMs);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state]);

  // idle: run the configured dwell; expiry -> mild-decay exit (doc 11 §3).
  // `dwellMs` is intentionally NOT in the dep list: a settings change mid-dwell
  // would otherwise restart the countdown of a bubble already on screen.
  useEffect(() => {
    if (state !== "idle") return;
    const timer = new DwellTimer(() => onLifecycle("expired"), dwellMs);
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

  // Hover pauses the dwell countdown (doc 11 §3). The ⋯ menu pauses it too:
  // it is PORTALLED to <body>, so opening it and moving the cursor onto it
  // fires this bubble's mouseleave — the dwell resumed and could expire the
  // bubble (unmounting the menu) mid-decision. That matters much more now that
  // the menu holds a real, consequential action (decision #8).
  const hoveringRef = useRef(false);
  const onMouseEnter = () => {
    hoveringRef.current = true;
    dwellRef.current?.pause();
  };
  const onMouseLeave = () => {
    hoveringRef.current = false;
    if (!overflowOpen) dwellRef.current?.resume();
  };
  useEffect(() => {
    if (overflowOpen) dwellRef.current?.pause();
    // Closing the menu must not out-vote an active hover (doc 11 §3).
    else if (!hoveringRef.current) dwellRef.current?.resume();
  }, [overflowOpen]);

  /** Apply one offer (decision #8). The bubble deliberately STAYS: excluding an
   *  app is a capture decision, not a judgment on this suggestion, and the menu
   *  has to remain mounted to offer Undo.
   *
   *  The pre-read is what makes Undo safe. `add_exclusion` is idempotent on
   *  `(match_kind, pattern)` and **re-enables** a matching row rather than
   *  inserting a second one — so without knowing the row's prior state, "Undo"
   *  would delete a rule the user had set up earlier and merely switched off,
   *  or switch off one that was already protecting them. */
  async function applyExclusion(offer: ExclusionOffer) {
    setExcludeError(null);
    try {
      const match = (await listExclusions()).find(
        (r) => r.match_kind === offer.match_kind && r.pattern === offer.pattern,
      );
      if (match?.enabled) {
        // Nothing to do and nothing to undo — say so instead of pretending the
        // click changed something.
        setExcluded({ label: offer.label, id: match.id, prior: "enabled" });
        return;
      }
      const id = await addExclusion(offer.match_kind, offer.pattern);
      setExcluded({ label: offer.label, id, prior: match ? "disabled" : "absent" });
    } catch (e) {
      // Surfaced in the menu, never swallowed: a silent failure here would let
      // the user believe capture had stopped when it had not.
      setExcludeError(String(e));
    }
  }

  /** Put the rule back exactly as it was: delete only what this click created. */
  async function undoExclusion() {
    if (!excluded || excluded.prior === "enabled") return;
    try {
      await setExclusion(excluded.id, excluded.prior === "disabled" ? false : null);
      setExcluded(null);
    } catch (e) {
      setExcludeError(String(e));
    }
  }

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
      className={`bubble ${opaque ? "surface-opaque" : "surface-glass"} surface-interactive ${stateClass}`}
      role="group"
      aria-label={spec.title}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
    >
      <div className="bubble__glyph" aria-hidden>
        {glyphMark(spec.glyph)}
      </div>

      <div className="bubble__title" title={spec.title}>
        {spec.title}
        {spec.source === "claude" && <span className="bubble__source-tag">via Claude</span>}
      </div>

      {/* A Resume that did not open swaps the sublabel for fallback copy and
          withdraws the button (doc 10 §6): the bubble stays, Dismiss and the
          thumbs still work, and nothing pretends the click succeeded. */}
      {instance.fallback ? (
        <div className="bubble__sublabel bubble__sublabel--fallback" role="alert">
          {instance.fallback}
        </div>
      ) : (
        spec.sublabel && <div className="bubble__sublabel">{spec.sublabel}</div>
      )}

      <div className="bubble__actions">
        {!instance.fallback && (
          <button className="btn btn--primary" onClick={onResume}>
            Resume
          </button>
        )}
        {/* The explicit "useful?" thumbs — SC7's data source (doc 11 §3, Q81).
            Rating does not dismiss: the user judged it, the bubble stays. */}
        <button
          className={`btn btn--icon bubble__thumb ${rated === "up" ? "bubble__thumb--set" : ""}`}
          aria-label="Useful"
          aria-pressed={rated === "up"}
          onClick={() => onRate("up")}
        >
          👍
        </button>
        <button
          className={`btn btn--icon bubble__thumb ${rated === "down" ? "bubble__thumb--set" : ""}`}
          aria-label="Not useful"
          aria-pressed={rated === "down"}
          onClick={() => onRate("down")}
        >
          👎
        </button>
        <button className="btn btn--icon" aria-label="Dismiss" onClick={onDismiss}>
          ×
        </button>
        <button
          ref={triggerRef}
          className="btn btn--icon"
          aria-label="More actions"
          aria-haspopup="menu"
          aria-expanded={overflowOpen}
          onClick={toggleOverflow}
        >
          ⋯
        </button>
      </div>

      {overflowOpen &&
        menuPos &&
        createPortal(
          <div
            className="bubble__overflow surface-opaque surface-interactive"
            role="menu"
            style={{ position: "fixed", right: menuPos.right, bottom: menuPos.bottom }}
            onKeyDown={(e) => {
              if (e.key === "Escape") setOverflowOpen(false);
            }}
          >
            <button
              role="menuitem"
              autoFocus
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
                // Straight to the 7-day mute (doc 08 §7) — no ladder counting.
                onMute();
              }}
            >
              Mute this pattern
            </button>
            {/* Decision #8: the real "stop capturing this". The core derived
                these from the bubble's own connector state and pre-escaped the
                pattern, so one click is a durable, hot-reloaded rule — the same
                machinery the Privacy panel writes (doc 13 §4). */}
            {!excluded &&
              (spec.exclusion_offers ?? []).map((offer) => (
                <button
                  key={`${offer.match_kind}:${offer.pattern}`}
                  role="menuitem"
                  onClick={() => void applyExclusion(offer)}
                >
                  Stop capturing {offer.label}
                </button>
              ))}
            {excluded && (
              <div className="bubble__overflow-note" role="status">
                {excluded.prior === "enabled"
                  ? `${excluded.label} was already excluded.`
                  : `Not capturing ${excluded.label} anymore.`}
                {excluded.prior !== "enabled" && (
                  <button role="menuitem" onClick={() => void undoExclusion()}>
                    Undo
                  </button>
                )}
              </div>
            )}
            {excludeError && (
              <div className="bubble__overflow-error" role="alert">
                Could not add the rule: {excludeError}
              </div>
            )}
            <button
              role="menuitem"
              onClick={() => {
                setOverflowOpen(false);
                // The manager, for everything the offers above cannot express
                // (opened on THIS monitor — decision #13). The bubble stays.
                void openPrivacy().catch(() => {});
              }}
            >
              Exclusions…
            </button>
          </div>,
          document.body,
        )}
    </div>
  );
}
