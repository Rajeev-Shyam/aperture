//! The master capture state as the overlay sees it (doc 11, doc 12 §6, doc 13
//  §8) — one subscription shared by the HUD orb's dot and the expanded row's
//  toggle (2026-09-06: extracted from CaptureIndicator so both can render it).
//
//  THIRD INVARIANT (capture toggle): turning capture OFF releases capture, kills
//  the sidecars, and drives VRAM->~0 in <3s. The UI reflects this immediately —
//  the toggle flips optimistically and the `capture_indicator` event confirms
//  the real state + a "releasing…" detail so the user SEES the teardown happen.
//
//  Capture defaults OFF until the user opts in at first run (doc 13 §8); the
//  initial state arrives via the first `capture_indicator` event.

import { useEffect, useState } from "react";

import {
  onCaptureIndicator,
  toggleCapture,
  type CaptureIndicatorEvent,
  type UnlistenFn,
} from "../lib/ipc";

export interface CaptureState {
  capturing: boolean;
  /** Core-supplied detail ("releasing…", a failure reason) or null. */
  detail: string | null;
  /** Brief activity pulse on each indicator tick while capturing. */
  pulse: boolean;
  /** A toggle is in flight; ignore further clicks. */
  busy: boolean;
  toggle: () => Promise<void>;
}

export function useCaptureState(): CaptureState {
  const [capturing, setCapturing] = useState(false);
  const [detail, setDetail] = useState<string | null>(null);
  const [pulse, setPulse] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    let pulseTimer: ReturnType<typeof setTimeout> | null = null;

    void onCaptureIndicator((e: CaptureIndicatorEvent) => {
      setCapturing(e.capturing);
      setDetail(e.detail ?? null);
      setBusy(false);
      if (e.capturing) {
        setPulse(true);
        if (pulseTimer) clearTimeout(pulseTimer);
        pulseTimer = setTimeout(() => setPulse(false), 240);
      }
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });

    return () => {
      cancelled = true;
      if (pulseTimer) clearTimeout(pulseTimer);
      unlisten?.();
    };
  }, []);

  async function toggle() {
    if (busy) return;
    setBusy(true);
    const next = !capturing;
    // Optimistic flip; the indicator event confirms the real state. On OFF the
    // detail will show the <3s teardown ("releasing… sidecars down").
    setCapturing(next);
    try {
      const confirmed = await toggleCapture(next);
      setCapturing(confirmed);
    } catch {
      setCapturing(!next); // revert on failure
    } finally {
      setBusy(false);
    }
  }

  return { capturing, detail, pulse, busy, toggle };
}
