//! FirstRunConsent — the first-run sequence (doc 13 §8, ADR-040).
//
//  Order is load-bearing: consent → detect-and-suggest sensitive apps → browser
//  extension → enable capture. The safety setup is surfaced at the moment it
//  makes sense, without being forced; capture stays OFF until the last step.
//
//  Two rules this surface must never break:
//    1. **Nothing is auto-excluded** (ADR-029/Q15). The scan produces candidates;
//       each one is confirmed or skipped by the user, and the default is skip.
//    2. **Declining is a real outcome.** "Not now" completes first-run with
//       capture OFF — it never re-nags, and it never silently enables anything.
//
//  Rendered as OPAQUE chrome, not glass: it is the largest surface in the app,
//  so a backdrop-filter here would both blow the ≤2 glass budget (doc 14 §5)
//  and cost the most. Non-exclusive since decision #12 — the card owns focus,
//  not the whole monitor.

import { useEffect, useRef, useState } from "react";

import {
  addExclusion,
  completeFirstRun,
  suggestExclusions,
  type SuggestedExclusion,
} from "../lib/ipc";
import { useModalSurface } from "../state/useModalSurface";

interface Props {
  /** Whether the core reports at-rest encryption is actually in force. Shown
   *  truthfully — we never claim encryption the build did not apply. */
  dbEncrypted: boolean;
  /** Called once first-run completes, with the capture decision. */
  onDone: (captureEnabled: boolean) => void;
}

type Step = "consent" | "suggest" | "extension" | "enable";

const STEP_ORDER: Step[] = ["consent", "suggest", "extension", "enable"];

export function FirstRunConsent({ dbEncrypted, onDone }: Props) {
  const [step, setStep] = useState<Step>("consent");
  const [suggestions, setSuggestions] = useState<SuggestedExclusion[] | null>(null);
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  // `aria-modal` below is a real contract: focus enters here and stays here.
  // No Escape handler — first-run has no safe "dismiss"; the way out is an
  // explicit decision ("Not now" also completes it).
  //
  // Non-exclusive (decision #12): the card is clickable via its own published
  // rect and takes OS keyboard focus on mount, but the rest of the monitor
  // stays click-through — first-run must be prominent, not hold the user's
  // whole screen hostage. It cannot be missed or half-completed regardless:
  // it is the ONLY surface the overlay renders until consent completes
  // (App.tsx), and capture stays OFF until `complete_first_run` (doc 13 §8).
  const onKeyDown = useModalSurface(rootRef, { exclusive: false });

  // Each step swaps the whole <section>, unmounting the button that was just
  // clicked — focus would fall to <body> and a screen reader would announce
  // nothing. Move focus to the new step's heading so the change is both
  // announced and keyboard-continuous.
  useEffect(() => {
    headingRef.current?.focus();
  }, [step]);

  // Kick the local scan when the user reaches that step (not before — scanning
  // the filesystem before consent would be exactly the surprise we promise not
  // to be). A failed scan degrades to "no suggestions", never a blocked flow.
  useEffect(() => {
    if (step !== "suggest" || suggestions !== null) return;
    let cancelled = false;
    setBusy(true);
    suggestExclusions()
      .then((s) => {
        if (!cancelled) setSuggestions(s);
      })
      .catch(() => {
        if (!cancelled) setSuggestions([]);
      })
      .finally(() => {
        if (!cancelled) setBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [step, suggestions]);

  function toggle(pattern: string) {
    setChosen((prev) => {
      const next = new Set(prev);
      if (next.has(pattern)) next.delete(pattern);
      else next.add(pattern);
      return next;
    });
  }

  function advance() {
    const i = STEP_ORDER.indexOf(step);
    setStep(STEP_ORDER[Math.min(i + 1, STEP_ORDER.length - 1)]);
  }

  /** Apply only the CONFIRMED suggestions, then advance. */
  async function applySuggestions() {
    setBusy(true);
    setError(null);
    try {
      for (const s of suggestions ?? []) {
        if (chosen.has(s.pattern)) await addExclusion(s.match_kind, s.pattern);
      }
      advance();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  /** Finish. `enable` is the user's explicit capture decision. */
  async function finish(enable: boolean) {
    setBusy(true);
    setError(null);
    try {
      await completeFirstRun(enable);
      onDone(enable);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }

  return (
    <div
      className="firstrun surface-opaque surface-interactive"
      role="dialog"
      aria-modal="true"
      aria-label="Welcome to Aperture — privacy setup"
      ref={rootRef}
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      <ol className="firstrun__steps" aria-label="Setup progress">
        {STEP_ORDER.map((s, i) => (
          <li
            key={s}
            className={`firstrun__step ${s === step ? "firstrun__step--active" : ""}`}
            aria-current={s === step ? "step" : undefined}
          >
            {i + 1}
          </li>
        ))}
      </ol>

      {step === "consent" && (
        <section className="firstrun__body">
          <h2 ref={headingRef} tabIndex={-1}>Aperture watches your screen — locally.</h2>
          <ul className="firstrun__facts">
            <li>Screenshots are <strong>never saved</strong>. Frames are read, turned into text, and dropped.</li>
            <li>Nothing reaches the internet unless you press <strong>Send</strong> on a preview showing the exact bytes.</li>
            <li>
              Your history is stored on this machine
              {dbEncrypted ? (
                <>, <strong>encrypted</strong> with a key held by your Windows account.</>
              ) : (
                <>. <strong>This build does not encrypt it at rest</strong> — it is readable by anything running as you.</>
              )}
            </li>
            <li>Capture is <strong>off</strong> right now, and stays off until you turn it on.</li>
          </ul>
          <div className="firstrun__actions">
            <button className="btn" onClick={() => void finish(false)} disabled={busy}>
              Not now
            </button>
            <button className="btn btn--primary" onClick={advance} disabled={busy}>
              Continue
            </button>
          </div>
        </section>
      )}

      {step === "suggest" && (
        <section className="firstrun__body">
          <h2 ref={headingRef} tabIndex={-1}>Anything you'd rather it never saw?</h2>
          <p className="firstrun__lede">
            We looked for apps on this machine where capture is usually unwelcome.
            Nothing is excluded unless you tick it.
          </p>
          <p className="firstrun__note" role="status" aria-live="polite">
            {busy ? "Scanning locally…" : ""}
          </p>
          {!busy && suggestions?.length === 0 && (
            <p className="firstrun__note">
              Nothing obvious found. You can always exclude an app later from any
              bubble's menu.
            </p>
          )}
          <ul className="firstrun__suggestions">
            {(suggestions ?? []).map((s) => (
              <li key={s.pattern}>
                <label>
                  <input
                    type="checkbox"
                    checked={chosen.has(s.pattern)}
                    onChange={() => toggle(s.pattern)}
                  />
                  <span className="firstrun__suggestion-label">{s.label}</span>
                  <span className="firstrun__suggestion-reason">{s.reason}</span>
                </label>
              </li>
            ))}
          </ul>
          <div className="firstrun__actions">
            {/* Deliberately NOT disabled while scanning: a local filesystem
                walk must never trap the user in the step. */}
            <button className="btn" onClick={advance}>
              Skip
            </button>
            <button className="btn btn--primary" onClick={() => void applySuggestions()} disabled={busy}>
              {chosen.size > 0 ? `Exclude ${chosen.size}` : "Continue"}
            </button>
          </div>
        </section>
      )}

      {step === "extension" && (
        <section className="firstrun__body">
          <h2 ref={headingRef} tabIndex={-1}>Browser extension (optional)</h2>
          <p className="firstrun__lede">
            It reads the <strong>URL and video position only</strong> — never page
            content — so "resume that video" works. Private/incognito windows are
            ignored. You can install it later.
          </p>
          <p className="firstrun__note">
            The extension ships in the <code>extension</code> folder where Aperture is
            installed: open <code>chrome://extensions</code>, turn on Developer mode, click
            "Load unpacked", and pick that folder.
          </p>
          <div className="firstrun__actions">
            <button className="btn" onClick={advance} disabled={busy}>
              Skip for now
            </button>
            <button className="btn btn--primary" onClick={advance} disabled={busy}>
              Done
            </button>
          </div>
        </section>
      )}

      {step === "enable" && (
        <section className="firstrun__body">
          <h2 ref={headingRef} tabIndex={-1}>Turn on capture?</h2>
          <p className="firstrun__lede">
            The indicator always tells the truth about whether it is watching, and
            turning it off releases everything within three seconds.
          </p>
          <div className="firstrun__actions">
            <button className="btn" onClick={() => void finish(false)} disabled={busy}>
              Leave it off
            </button>
            <button className="btn btn--primary" onClick={() => void finish(true)} disabled={busy}>
              Turn on capture
            </button>
          </div>
        </section>
      )}

      {error && (
        <p className="firstrun__error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
