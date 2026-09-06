//! Pure layout helpers for the HUD orb (doc 11 §2; 2026-09-06 redesign).
//
//  The HUD is one small circle ("the orb") that snaps to one of 8 anchors
//  (4 corners + 4 edge midpoints) and expands, on click, into a single row of
//  controls. Everything here is pure so vitest can pin it: which anchor a drop
//  point snaps to, the fixed-position style of an anchor, and the modifier
//  classes that tell the CSS which way the row grows and where popovers open.

import type { CSSProperties } from "react";

import type { HudAnchor } from "../lib/ipc";

export const ANCHORS: HudAnchor[] = [
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
export function anchorStyle(anchor: HudAnchor): CSSProperties {
  switch (anchor) {
    case "top-left":
      return { top: INSET, left: INSET };
    case "top-center":
      return { top: INSET, left: "50%", transform: "translateX(-50%)" };
    case "top-right":
      return { top: INSET, right: INSET };
    case "center-left":
      return { top: "50%", left: INSET, transform: "translateY(-50%)" };
    case "center-right":
      return { top: "50%", right: INSET, transform: "translateY(-50%)" };
    case "bottom-left":
      return { bottom: INSET, left: INSET };
    case "bottom-center":
      return { bottom: INSET, left: "50%", transform: "translateX(-50%)" };
    case "bottom-right":
      return { bottom: INSET, right: INSET };
  }
}

/**
 * Snap a drop point to the nearest of the 8 anchors (viewport thirds). The
 * dead middle third keeps `fallback` — there is no centre anchor.
 */
export function nearestAnchor(
  x: number,
  y: number,
  viewportWidth: number,
  viewportHeight: number,
  fallback: HudAnchor,
): HudAnchor {
  const col = x < viewportWidth / 3 ? "left" : x > (2 * viewportWidth) / 3 ? "right" : "center";
  const row = y < viewportHeight / 3 ? "top" : y > (2 * viewportHeight) / 3 ? "bottom" : "center";
  if (row === "center" && col === "center") return fallback;
  const name = `${row}-${col}` as HudAnchor;
  return ANCHORS.includes(name) ? name : fallback;
}

/**
 * Modifier classes for an anchor: `hud--col-<left|center|right>` decides which
 * way the expanded row grows (a right-anchored orb grows leftward, so the orb
 * itself never moves), `hud--row-<top|center|bottom>` decides whether popovers
 * (the snooze menu) open downward or upward so they stay on screen.
 */
export function anchorClasses(anchor: HudAnchor): string {
  const [row, col] = anchor.split("-");
  return `hud--col-${col} hud--row-${row}`;
}
