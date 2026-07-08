//! Glass-surface budget (ADR-039, doc 14 §5).
//
//  The performance cap: at most `glassCap` concurrent glass (backdrop-filter)
//  surfaces; every visible bubble beyond that renders in the opaque fallback
//  (`.surface-opaque`). With the UX cap of 3 visible bubbles (doc 11 §3) and the
//  default glassCap of 2, that is 2 glass + 1 opaque = 3 visible (doc 14 §5).
//
//  Degrade-under-load (`gpu_busy`) is ORTHOGONAL and handled globally in CSS
//  (`body.gpu-busy .surface-glass` → opaque, see gpuBusy.ts): while the GPU mutex
//  is held EVERY surface is opaque regardless of this budget. This module owns
//  only the concurrency cap, so the two rules compose without conflict.
//
//  Pure + framework-agnostic (typechecked via `tsc`); the final cap is set at the
//  M8 PresentMon gate (doc 14 §5).

/** ADR-039 default: ≤2 glass surfaces. The final cap is set at the M8 PresentMon test. */
export const DEFAULT_GLASS_CAP = 2;

/**
 * Whether the bubble at zero-based visible `index` must render opaque to honor the
 * glass-surface budget. `glassCap` comes from `ui.max_glass_surfaces` (settings),
 * clamped to ≥ 0 (a cap of 0 ⇒ every surface opaque).
 */
export function isOpaqueForBudget(index: number, glassCap: number = DEFAULT_GLASS_CAP): boolean {
  return index >= Math.max(0, glassCap);
}
