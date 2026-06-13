//! Degrade-under-load wiring (doc 11 §6, doc 14 §5).
//
//  Subscribes to the `gpu_busy` broadcast (mutex-derived in the orchestration
//  crate, doc 12 §3) and toggles a `gpu-busy` class on <body>. design-tokens.css
//  keys the glass -> `--fallback-opaque` swap off that class: opaque fill, no
//  blur, fades only. `prefers-reduced-motion` lands on the same fallback (handled
//  purely in CSS).
//
//  This module owns NO React state — it is a side-effecting global so the swap
//  applies even to surfaces outside the React tree, and is the single place the
//  body class is mutated.

import { onGpuBusy, type UnlistenFn } from "../lib/ipc";

const BODY_CLASS = "gpu-busy";

/** Imperatively reflect the current GPU-busy state onto <body>. */
export function setGpuBusy(busy: boolean): void {
  document.body.classList.toggle(BODY_CLASS, busy);
}

/** Read the last-applied state from the DOM (no separate store to drift). */
export function isGpuBusy(): boolean {
  return document.body.classList.contains(BODY_CLASS);
}

/**
 * Begin listening for `gpu_busy` events and applying the degrade swap. Call once
 * from the OverlayRoot mount; await + store the returned unlisten for cleanup.
 *
 * TODO(M5:) confirm the broadcast fires on BOTH edges (acquire + release) so the
 *           overlay restores glass on release — the swap must be visually clean
 *           (doc 14 §5 M8 gate: PresentMon, no overlay frame drops under a VLM job).
 */
export function startGpuBusyWatch(): Promise<UnlistenFn> {
  return onGpuBusy(setGpuBusy);
}
