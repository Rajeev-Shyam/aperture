//! OverlayRoot (doc 11 §2). The single React root for the transparent overlay.
//  Responsibilities:
//    - subscribe the global degrade-under-load watch (`gpu_busy`) once;
//    - render the bottom-right bubble stack, the voice surfaces, the capture
//      indicator, and (when opened) the Context-Preview panel;
//    - own the "is a preview panel open" state, since the preview is the one
//      modal-ish surface and counts toward the ≤3 glass-surface cap (doc 14 §5).
//
//  The body stays transparent (doc 11 §2); each surface opts back into pointer
//  events via `.surface-interactive`.

import { useEffect, useState } from "react";

import {
  getConsent,
  onVoiceSurface,
  requestPreview,
  type ConsentState,
  type ContextPayload,
  type Intent,
  type UnlistenFn,
  type VoiceSurfaceEvent,
} from "./lib/ipc";
import { startGpuBusyWatch } from "./state/gpuBusy";

import { BubbleContainer } from "./components/BubbleContainer";
import { CaptureIndicator } from "./components/CaptureIndicator";
import { ContextPreviewPanel } from "./components/ContextPreviewPanel";
import { FirstRunConsent } from "./components/FirstRunConsent";
import { PrivacyPanel } from "./components/PrivacyPanel";
import { VoiceSurfaces } from "./components/VoiceSurfaces";

export default function App() {
  // The previewed payload, or null when no panel is open. Editing this object
  // in the panel IS editing what will ship (doc 11 §4 invariant).
  const [preview, setPreview] = useState<ContextPayload | null>(null);

  // Latest voice surface event (listening pill / transcript chip / answer).
  const [voice, setVoice] = useState<VoiceSurfaceEvent>({ surface: "hidden" });

  // Consent (doc 13 §8). `null` = not yet read; the first-run flow renders only
  // once we KNOW first-run is incomplete, so a slow read never flashes it at a
  // user who already consented.
  const [consent, setConsent] = useState<ConsentState | null>(null);
  const [privacyOpen, setPrivacyOpen] = useState(false);

  // Wire the global gpu_busy degrade watch + the voice surface stream once.
  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let cancelled = false;

    void startGpuBusyWatch().then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    void onVoiceSurface(setVoice).then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    // A failed consent read must not strand the user in a blank overlay; treat
    // it as "first-run already done" and let the normal surfaces render.
    void getConsent()
      .then((c) => {
        if (!cancelled) setConsent(c);
      })
      .catch(() => {
        if (!cancelled) {
          setConsent({
            first_run_completed: true,
            capture_enabled: false,
            voice_opt_in: false,
            capture_opt_in_ts: null,
            db_encrypted: false,
          });
        }
      });

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);

  /**
   * Open the Context-Preview panel for an "Ask Claude" affordance (doc 11 §4).
   * Asks the core to BUILD the payload; the returned object is rendered + edited
   * in place. `seedActionRef` ties the preview to the bubble/answer it came from.
   *
   * TODO(M7:) surface build errors (size warnings come from the panel footer).
   */
  async function openPreview(intent: Intent, seedActionRef?: string): Promise<void> {
    const payload = await requestPreview(intent, seedActionRef);
    setPreview(payload);
  }

  // First run owns the whole overlay: privacy setup precedes every other surface
  // (doc 13 §8), and capture is OFF behind it regardless.
  if (consent && !consent.first_run_completed) {
    return (
      <FirstRunConsent
        dbEncrypted={consent.db_encrypted}
        onDone={(captureEnabled) =>
          setConsent({ ...consent, first_run_completed: true, capture_enabled: captureEnabled })
        }
      />
    );
  }

  return (
    <>
      {/* Bottom-right suggestion stack (≤3 visible, queue overflow). */}
      <BubbleContainer onAskClaude={(actionRef) => void openPreview("explain_pattern", actionRef)} />

      {/* Voice surfaces: listening pill / transcript chip / answer bubble. */}
      <VoiceSurfaces
        event={voice}
        onAskClaude={(actionRef) => void openPreview("answer_query", actionRef)}
        onDismiss={() => setVoice({ surface: "hidden" })}
        onRun={(_transcript) => {
          // TODO(M6-followup): a `voice_run_transcript` core command re-issues the
          // confirmed transcript through the query path. For now, confirming clears
          // the chip so the low-confidence surface is never a stuck dead-end (review #8).
          setVoice({ surface: "hidden" });
        }}
      />

      {/* Capture state + activity pulse; OFF reflects VRAM->~0 (<3s). */}
      <CaptureIndicator />

      {/* Activity & Privacy (doc 13 §7, ADR-040): the audit trail, exclusions,
          and Purge All. Reachable from the capture indicator. */}
      <button
        className="privacy-open surface-interactive"
        aria-label="Open activity and privacy"
        title="Activity & Privacy"
        onClick={() => setPrivacyOpen(true)}
      >
        🛡
      </button>
      {privacyOpen && (
        <PrivacyPanel
          dbEncrypted={consent?.db_encrypted ?? false}
          onClose={() => setPrivacyOpen(false)}
        />
      )}

      {/* The trust surface. Edits mutate `preview`; Send transmits exact bytes. */}
      {preview && (
        <ContextPreviewPanel
          payload={preview}
          onChange={setPreview}
          onClose={() => setPreview(null)}
        />
      )}
    </>
  );
}
