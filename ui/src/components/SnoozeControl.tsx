//! Global bubble snooze control (doc 11 §6, ADR-040/Q95) — the HUD's 🔕.
//
//  Snooze quiets bubble EMISSION while capture + learning continue — the exact
//  thing ADR-040 forbids conflating with the capture toggle. The backend
//  (set_snooze / queue-while-snoozed) shipped at M3; this control was the
//  missing UI (2026-08-15 review).
//
//  The button reflects the TRUTH (get_snooze on mount + after every change),
//  not its last click; while snoozed it renders filled with a title naming the
//  deadline.

import { useEffect, useRef, useState } from "react";

import { getSnooze, setSnooze } from "../lib/ipc";

const FOREVER_THRESHOLD = 8_640_000_000_000_000; // ≈ i64::MAX ms ⇒ "until re-enabled"

const OPTIONS: { mode: "off" | "15m" | "1h" | "forever"; label: string }[] = [
  { mode: "15m", label: "Snooze 15 minutes" },
  { mode: "1h", label: "Snooze 1 hour" },
  { mode: "forever", label: "Snooze until I turn it back on" },
  { mode: "off", label: "Turn snooze off" },
];

export function SnoozeControl() {
  const [until, setUntil] = useState(0);
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    void getSnooze().then(setUntil).catch(() => {});
  }, []);

  // Close on outside click so the popover can't strand itself.
  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("pointerdown", close);
    return () => window.removeEventListener("pointerdown", close);
  }, [open]);

  const snoozed = until > Date.now();
  const title = !snoozed
    ? "Snooze suggestions — quiets bubbles, capture and learning continue"
    : until >= FOREVER_THRESHOLD
      ? "Suggestions snoozed until you turn them back on"
      : `Suggestions snoozed until ${new Date(until).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;

  async function choose(mode: "off" | "15m" | "1h" | "forever") {
    setOpen(false);
    try {
      await setSnooze(mode);
      setUntil(await getSnooze());
    } catch (e) {
      console.error("set_snooze failed:", e);
    }
  }

  return (
    <div className="hud__snooze" ref={rootRef}>
      <button
        className={`privacy-open ${snoozed ? "hud__snooze-btn--on" : ""}`}
        aria-label={title}
        aria-pressed={snoozed}
        aria-haspopup="menu"
        aria-expanded={open}
        title={title}
        onClick={() => setOpen((v) => !v)}
      >
        🔕
      </button>
      {open && (
        <div className="hud__snooze-menu surface-opaque surface-interactive" role="menu">
          {OPTIONS.filter((o) => (o.mode === "off" ? snoozed : true)).map((o) => (
            <button key={o.mode} role="menuitem" onClick={() => void choose(o.mode)}>
              {o.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
