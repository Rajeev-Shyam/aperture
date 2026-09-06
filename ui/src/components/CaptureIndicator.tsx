//! CaptureIndicator (doc 11, doc 12 §6, doc 13 §8). The capture state label +
//  the user's one-click toggle, rendered inside the HUD's expanded row. The
//  state itself lives in `useCaptureState` (2026-09-06) so the collapsed orb
//  can show the same dot without a second subscription. Presentational only.

import type { CaptureState } from "../state/useCaptureState";

interface Props extends Omit<CaptureState, "toggle"> {
  onToggle: () => void;
}

export function CaptureIndicator({ capturing, detail, pulse, busy, onToggle }: Props) {
  return (
    <div className="capture">
      <button
        className="capture__toggle"
        role="switch"
        aria-checked={capturing}
        aria-label={capturing ? "Capture on — click to turn off" : "Capture off — click to turn on"}
        title={capturing ? "Capturing — click to turn off" : "Capture off — click to turn on"}
        onClick={onToggle}
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
