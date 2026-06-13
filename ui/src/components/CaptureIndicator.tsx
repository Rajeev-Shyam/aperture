//! CaptureIndicator (doc 11, doc 12 §6, doc 13 §8). The always-visible master
//  capture state + activity pulse, and the user's one-click toggle.
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

export function CaptureIndicator() {
  const [capturing, setCapturing] = useState(false);
  const [detail, setDetail] = useState<string | null>(null);
  // Brief pulse on activity events (frame captured / event written).
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
      // Pulse on each indicator tick while capturing (activity heartbeat).
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

  return (
    <div className="capture surface-glass surface-interactive">
      <button
        className="capture__toggle"
        role="switch"
        aria-checked={capturing}
        aria-label={capturing ? "Capture on — click to turn off" : "Capture off — click to turn on"}
        onClick={() => void toggle()}
        disabled={busy}
      >
        <span
          className={`capture__dot ${capturing ? "capture__dot--on" : "capture__dot--off"} ${
            pulse ? "capture__dot--pulse" : ""
          }`}
          aria-hidden
        />
        <span className="capture__label">{capturing ? "Capturing" : "Capture off"}</span>
      </button>
      {detail && <span className="capture__detail">{detail}</span>}
    </div>
  );
}
