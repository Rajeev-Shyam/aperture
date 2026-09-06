//! The HUD orb (doc 11; 2026-09-06 redesign, owner's ask "one tiny bubble that
//  expands when I click it, draggable by itself, hideable while gaming").
//
//  Collapsed, the HUD is one ~36 px circle carrying the capture dot. Click it
//  and a single row of controls unfolds beside it — capture toggle, snooze,
//  dashboard, agent, privacy, hide — toward the free side of its anchor, so the
//  orb itself never moves. It collapses on an outside click, Escape, or another
//  orb click. Drag ANYWHERE on it (buttons included — a press only becomes a
//  drag after ~6 px, so clicks still work) and release near a corner or edge;
//  it snaps to the nearest of 8 anchors and the choice persists in
//  `ui.hud_anchor` across restarts.
//
//  Hide: the row's ⊘ sets `ui.hud_hidden`; the HUD unmounts (its rect leaves
//  the hit-test set, so the spot is click-through again). The tray's "Show
//  overlay controls" flips the same key; both sides converge through the
//  `settings_changed` broadcast — this component never trusts its last click.

import { useEffect, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";

import {
  getSettings,
  onSettingsChanged,
  setSettings,
  type HudAnchor,
  type UiSettings,
  type UnlistenFn,
} from "../lib/ipc";
import { ANCHORS, anchorClasses, anchorStyle, nearestAnchor } from "../state/hudLayout";
import { useCaptureState } from "../state/useCaptureState";
import { useDraggable } from "../state/useDraggable";
import { CaptureIndicator } from "./CaptureIndicator";

interface Props {
  /** The App-owned controls (snooze, dashboard, agent, privacy). */
  children: ReactNode;
}

/** Merge-persist into the `ui` section: `set_settings` replaces whole
 *  top-level sections, so the existing block is carried forward. */
function persistUi(patch: Partial<UiSettings>): void {
  void getSettings()
    .then((s) => setSettings({ ui: { ...(s.ui ?? {}), ...patch } as never }))
    .catch((err) => console.error("hud prefs persist failed:", err));
}

export function Hud({ children }: Props) {
  const [anchor, setAnchor] = useState<HudAnchor>("top-right");
  const [hidden, setHidden] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const anchorRef = useRef(anchor);
  anchorRef.current = anchor;
  const rootRef = useRef<HTMLDivElement>(null);
  const capture = useCaptureState();

  const drag = useDraggable(rootRef, {
    resetOnDrop: true, // the HUD snaps to an anchor rather than free-floating
    onDrop: (x, y) => {
      const next = nearestAnchor(x, y, window.innerWidth, window.innerHeight, anchorRef.current);
      setAnchor(next);
      persistUi({ hud_anchor: next });
    },
  });

  // Read the persisted prefs (anchor, hidden) and follow every `ui` write:
  // that broadcast is how the tray's "Show overlay controls" reaches here.
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    const read = () => {
      void getSettings()
        .then((s) => {
          if (cancelled) return;
          const a = s.ui?.hud_anchor;
          if (a && ANCHORS.includes(a)) setAnchor(a);
          setHidden(s.ui?.hud_hidden === true);
        })
        .catch(() => {});
    };
    read();
    void onSettingsChanged((e) => {
      if (e.sections.includes("ui")) read();
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Collapse on an outside press or Escape so the row can't strand itself.
  useEffect(() => {
    if (!expanded) return;
    const onDown = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setExpanded(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setExpanded(false);
    };
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [expanded]);

  if (hidden) return null;

  const style: CSSProperties = drag.style ?? anchorStyle(anchor);
  const className = [
    "hud",
    "surface-interactive",
    anchorClasses(anchor),
    expanded ? "hud--expanded" : "hud--collapsed",
    drag.dragging ? "hud--dragging" : "",
  ]
    .filter(Boolean)
    .join(" ");
  const stateWord = capture.capturing ? "capturing" : "capture off";

  return (
    <div ref={rootRef} className={className} style={style} {...drag.handleProps}>
      <button
        className="hud__orb surface-glass"
        aria-expanded={expanded}
        aria-label={`Aperture controls — ${stateWord}`}
        title={`Aperture — ${stateWord}. Click for controls, drag to move.`}
        onClick={() => setExpanded((v) => !v)}
      >
        <span
          className={`capture__dot ${capture.capturing ? "capture__dot--on" : "capture__dot--off"} ${
            capture.pulse ? "capture__dot--pulse" : ""
          }`}
          aria-hidden
        />
      </button>
      {expanded && (
        <div className="hud__row surface-opaque" role="toolbar" aria-label="Aperture controls">
          <CaptureIndicator
            capturing={capture.capturing}
            detail={capture.detail}
            pulse={capture.pulse}
            busy={capture.busy}
            onToggle={() => void capture.toggle()}
          />
          <span className="hud__divider" aria-hidden />
          {children}
          <span className="hud__divider" aria-hidden />
          <button
            className="privacy-open"
            aria-label="Hide the overlay controls"
            title="Hide these controls — bring them back from the tray icon (Show overlay controls)"
            onClick={() => {
              setExpanded(false);
              setHidden(true);
              persistUi({ hud_hidden: true });
            }}
          >
            ⊘
          </button>
        </div>
      )}
    </div>
  );
}
