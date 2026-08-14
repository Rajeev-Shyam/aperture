//! The HUD cluster (doc 11): capture indicator + the dashboard and privacy
//  affordances, grouped so they move as one unit. Drag ANYWHERE on the cluster
//  (buttons included — a press only becomes a drag after ~6 px, so clicks
//  still work) and release near a corner or edge; it snaps to the nearest of
//  8 anchors and the choice persists in `ui.hud_anchor` across restarts.

import { useEffect, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";

import { getSettings, setSettings, type HudAnchor } from "../lib/ipc";
import { useDraggable } from "../state/useDraggable";

const ANCHORS: HudAnchor[] = [
  "top-left",
  "top-center",
  "top-right",
  "center-left",
  "center-right",
  "bottom-left",
  "bottom-center",
  "bottom-right",
];

const INSET = "var(--edge-inset)";

/** Fixed-position style for each anchor. */
function anchorStyle(anchor: HudAnchor): CSSProperties {
  switch (anchor) {
    case "top-left":
      return { top: INSET, left: INSET, alignItems: "flex-start" };
    case "top-center":
      return { top: INSET, left: "50%", transform: "translateX(-50%)", alignItems: "center" };
    case "top-right":
      return { top: INSET, right: INSET, alignItems: "flex-end" };
    case "center-left":
      return { top: "50%", left: INSET, transform: "translateY(-50%)", alignItems: "flex-start" };
    case "center-right":
      return { top: "50%", right: INSET, transform: "translateY(-50%)", alignItems: "flex-end" };
    case "bottom-left":
      return { bottom: INSET, left: INSET, alignItems: "flex-start" };
    case "bottom-center":
      return { bottom: INSET, left: "50%", transform: "translateX(-50%)", alignItems: "center" };
    case "bottom-right":
      return { bottom: INSET, right: INSET, alignItems: "flex-end" };
  }
}

/** Snap a drop point to the nearest of the 8 anchors (viewport thirds). */
function nearestAnchor(x: number, y: number, fallback: HudAnchor): HudAnchor {
  const col = x < window.innerWidth / 3 ? "left" : x > (2 * window.innerWidth) / 3 ? "right" : "center";
  const row = y < window.innerHeight / 3 ? "top" : y > (2 * window.innerHeight) / 3 ? "bottom" : "center";
  if (row === "center" && col === "center") return fallback; // dead middle: keep
  const name = (row === "center" ? `center-${col}` : `${row}-${col}`) as HudAnchor;
  return ANCHORS.includes(name) ? name : fallback;
}

interface Props {
  children: ReactNode;
}

export function Hud({ children }: Props) {
  const [anchor, setAnchor] = useState<HudAnchor>("top-right");
  const anchorRef = useRef(anchor);
  anchorRef.current = anchor;
  const rootRef = useRef<HTMLDivElement>(null);

  const drag = useDraggable(rootRef, {
    resetOnDrop: true, // the HUD snaps to an anchor rather than free-floating
    onDrop: (x, y) => {
      const next = nearestAnchor(x, y, anchorRef.current);
      setAnchor(next);
      // Merge-persist: set_settings replaces whole top-level sections, so
      // carry the existing `ui` block forward.
      void getSettings()
        .then((s) => setSettings({ ui: { ...(s.ui ?? {}), hud_anchor: next } as never }))
        .catch((err) => console.error("hud anchor persist failed:", err));
    },
  });

  useEffect(() => {
    void getSettings()
      .then((s) => {
        const a = s.ui?.hud_anchor;
        if (a && ANCHORS.includes(a)) setAnchor(a);
      })
      .catch(() => {});
  }, []);

  const style: CSSProperties = drag.style ?? anchorStyle(anchor);

  return (
    <div
      ref={rootRef}
      className={`hud surface-interactive ${drag.dragging ? "hud--dragging" : ""}`}
      style={style}
      title="Drag to move — snaps to corners and edges"
      {...drag.handleProps}
    >
      <div className="hud__grip" aria-hidden>
        ⠿
      </div>
      {children}
    </div>
  );
}
