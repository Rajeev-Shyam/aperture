//! What a modal surface needs on the Aperture overlay (doc 11 §2, doc 13, a11y).
//
//  Two obligations, both easy to forget and both silently fatal:
//
//  1. **Input.** The overlay window is created click-through
//     (`WS_EX_TRANSPARENT`) and `"focus": false` — right for passive bubbles,
//     fatal for a dialog. Without clearing that bit, every click on a modal
//     falls through to the app underneath and the buttons simply never fire.
//     The `set_overlay_interactive` command toggles it; this hook pairs the
//     mount/unmount calls so a surface cannot forget the `false`.
//
//  2. **Focus.** `aria-modal="true"` is a promise to assistive tech that focus
//     is inside the surface and cannot wander behind it. Declaring it without
//     implementing it is worse than not declaring it: a screen-reader user is
//     told the rest of the page is inert while Tab quietly walks them out.
//
//  Escape is deliberately NOT handled here — whether Escape is a safe exit is
//  per-surface (Cancel on the preview, Close on the privacy panel, nothing on a
//  first-run flow that must be answered), so each caller wires it explicitly.

import { useEffect, type RefObject } from "react";

import { focusOverlay, setOverlayInteractive } from "../lib/ipc";

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Mark `ref` as a live modal surface: move focus in, restore focus on unmount,
 * and cycle Tab within it.
 *
 * `exclusive` (default `true`) additionally makes the WHOLE overlay window
 * accept input for the surface's lifetime. No current surface uses it —
 * first-run was the last and went non-exclusive (decision #12): the
 * `focusOverlay` path below already makes a surface answerable with no prior
 * click, so exclusivity is reserved for a future dialog that must also eat
 * every click on the monitor. Panels pass `exclusive: false`: their own rect
 * (published by `useHitTestRects`) makes them clickable, while everywhere
 * OUTSIDE the panel stays click-through to the user's apps — a visible panel
 * must not swallow the rest of the screen.
 *
 * Returns the `onKeyDown` handler the surface must spread onto its root element.
 */
export function useModalSurface(
  ref: RefObject<HTMLElement | null>,
  opts?: { exclusive?: boolean },
): (e: React.KeyboardEvent) => void {
  const exclusive = opts?.exclusive ?? true;
  useEffect(() => {
    if (exclusive) {
      // If this fails the modal is unusable, so it is worth a console error —
      // but it must not throw and leave the surface half-initialised.
      void setOverlayInteractive(true).catch((e) =>
        console.error("overlay did not become interactive; this modal may be unclickable", e),
      );
    } else {
      // Non-exclusive panels still need OS keyboard focus: the overlay window
      // is created focus:false, so DOM focus alone leaves Escape/Tab/typing
      // going to the user's foreground app until they click inside — panels
      // opened with no click (tray, MCP preview_request) were keyboard-dead
      // (2026-08-15 review). Click-through outside the panel is unaffected.
      void focusOverlay().catch(() => {});
    }
    const opener = document.activeElement as HTMLElement | null;
    ref.current?.focus();
    return () => {
      if (exclusive) {
        void setOverlayInteractive(false).catch(() => {
          /* going back to click-through is best-effort on teardown */
        });
      }
      // Restoring focus to whatever opened the surface is what makes closing it
      // non-disorienting; without it focus falls back to <body>.
      opener?.focus?.();
    };
  }, [ref, exclusive]);

  return (e: React.KeyboardEvent) => {
    if (e.key !== "Tab") return;
    const focusables = ref.current?.querySelectorAll<HTMLElement>(FOCUSABLE);
    if (!focusables || focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };
}
