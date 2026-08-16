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

import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import {
  getConsent,
  onDashboardOpen,
  onPreviewRequest,
  onPrivacyOpen,
  onVoiceSurface,
  previewCancel,
  requestPreview,
  resetOverlayInteractivity,
  voiceDismiss,
  voiceRunTranscript,
  type ConsentState,
  type ContextPayload,
  type Intent,
  type UnlistenFn,
  type VoiceSurfaceEvent,
} from "./lib/ipc";
import { startGpuBusyWatch } from "./state/gpuBusy";
import { useHitTestRects } from "./state/useHitTestRects";

import { BubbleContainer } from "./components/BubbleContainer";
import { CaptureIndicator } from "./components/CaptureIndicator";
import { ContextPreviewPanel } from "./components/ContextPreviewPanel";
import { Dashboard } from "./components/Dashboard";
import { FirstRunConsent } from "./components/FirstRunConsent";
import { Hud } from "./components/Hud";
import { PrivacyPanel } from "./components/PrivacyPanel";
import { SnoozeControl } from "./components/SnoozeControl";
import { VoiceSurfaces } from "./components/VoiceSurfaces";

// Which monitor's overlay is this root running in? The primary window keeps
// the config label `overlay`; per-monitor clones are `overlay-1`, `overlay-2`…
// (overlay.rs `plan_overlays`). Secondary monitors render only the passive
// ambient surfaces (bubbles + voice); the HUD, panels, preview, and first-run
// belong to the primary alone — duplicating controls per monitor meant
// duplicate dialogs and double settings writes (doc 11 §2, M8 cleanup).
const IS_PRIMARY_OVERLAY = getCurrentWindow().label === "overlay";

export default function App() {
  // The previewed payload, or null when no panel is open. Editing this object
  // in the panel IS editing what will ship (doc 11 §4 invariant).
  const [preview, setPreview] = useState<ContextPayload | null>(null);
  // Core-staged payloads that arrived while a panel was already open: they
  // QUEUE instead of clobbering the user's mid-review edits (2026-08-15
  // review); each opens when the current panel closes.
  const previewQueue = useRef<ContextPayload[]>([]);

  // Latest voice surface event (listening pill / transcript chip / answer).
  const [voice, setVoice] = useState<VoiceSurfaceEvent>({ surface: "hidden" });

  // Consent (doc 13 §8). `null` = not yet read; the first-run flow renders only
  // once we KNOW first-run is incomplete, so a slow read never flashes it at a
  // user who already consented.
  const [consent, setConsent] = useState<ConsentState | null>(null);
  const [privacyOpen, setPrivacyOpen] = useState(false);
  const [dashboardOpen, setDashboardOpen] = useState(false);

  // Publish interactive-surface rects so the click-through overlay accepts
  // input over bubbles/indicator/voice chips (doc 11 §2). Modal surfaces keep
  // their own coarser `useModalSurface` switch; the core composes both.
  useHitTestRects();

  // Wire the global gpu_busy degrade watch + the voice surface stream once.
  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let cancelled = false;

    // A fresh mount means no modal is open: reset any interactivity state a
    // previous page (crashed/reloaded WebView) left behind (doc 11 §7).
    void resetOverlayInteractivity().catch(() => {});

    void startGpuBusyWatch().then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    void onVoiceSurface(setVoice).then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    // The tray (left-click / "Open Dashboard") and a second app launch both
    // land here — the overlay is the only surface that can show the dashboard.
    void onDashboardOpen(() => setDashboardOpen(true)).then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    // The core staged a payload (Claude Desktop's gated search, ADR-037):
    // open the trust surface on it so the user decides what leaves. If a panel
    // is already open (possibly mid-edit), the request queues — the trust
    // surface must never swap its contents under the user's cursor.
    void onPreviewRequest((p) => {
      setPreview((cur) => {
        if (cur) {
          previewQueue.current.push(p);
          return cur;
        }
        return p;
      });
    }).then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    // A bubble's "Exclusions…" (any monitor) opens the privacy panel here on
    // the primary (the event is targeted, never broadcast).
    void onPrivacyOpen(() => setPrivacyOpen(true)).then((u) => {
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
   * An explicit user open REPLACES an open panel — but cancels the displaced
   * session core-side first, so nothing lingers staged (2026-08-15 review).
   *
   * TODO(M7:) surface build errors (size warnings come from the panel footer).
   */
  async function openPreview(intent: Intent, seedActionRef?: string): Promise<void> {
    const payload = await requestPreview(intent, seedActionRef);
    setPreview((cur) => {
      if (cur) void previewCancel(cur.payload_id).catch(() => {});
      return payload;
    });
  }

  /** Close the panel and surface the next queued core-staged request, if any. */
  function closePreview(): void {
    setPreview(previewQueue.current.shift() ?? null);
  }

  // First run owns the whole overlay: privacy setup precedes every other surface
  // (doc 13 §8), and capture is OFF behind it regardless. It is answered ONCE,
  // on the primary monitor — a copy per monitor was N dialogs for one question.
  if (consent && !consent.first_run_completed) {
    if (!IS_PRIMARY_OVERLAY) return null;
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
        onDismiss={() => {
          // Hide locally NOW, then converge every monitor's clone through the
          // core broadcast (2026-08-15 review: local-only dismissal left live
          // copies on the other overlays).
          setVoice({ surface: "hidden" });
          void voiceDismiss().catch(() => {});
        }}
        onRun={(transcript) => {
          // Re-issue the confirmed transcript through the query path (doc 07
          // §4.4); the resulting answer/empty surface arrives via voice_surface.
          void voiceRunTranscript(transcript).catch((e) =>
            console.error("voice_run_transcript failed:", e),
          );
        }}
      />

      {/* Everything below is the primary monitor's alone: the HUD's controls,
          the panels, and the preview must exist exactly once. */}
      {IS_PRIMARY_OVERLAY && (
      <>
      {/* The HUD cluster: capture indicator + dashboard + privacy, draggable
          to any corner or edge (persisted in ui.hud_anchor). */}
      <Hud>
        <CaptureIndicator />
        <div className="hud__buttons">
          {/* Global snooze (doc 11 §6, ADR-040): quiet bubbles, keep learning. */}
          <SnoozeControl />
          <button
            className="privacy-open"
            aria-label={dashboardOpen ? "Close the Aperture dashboard" : "Open the Aperture dashboard"}
            aria-pressed={dashboardOpen}
            title="Dashboard — history, patterns, everything it knows"
            onClick={() => setDashboardOpen((v) => !v)}
          >
            ◎
          </button>
          <button
            className="privacy-open"
            aria-label={privacyOpen ? "Close activity and privacy" : "Open activity and privacy"}
            aria-pressed={privacyOpen}
            title="Activity & Privacy"
            onClick={() => setPrivacyOpen((v) => !v)}
          >
            🛡
          </button>
        </div>
      </Hud>
      {privacyOpen && (
        <PrivacyPanel
          dbEncrypted={consent?.db_encrypted ?? false}
          onClose={() => setPrivacyOpen(false)}
        />
      )}
      {dashboardOpen && (
        <Dashboard
          onClose={() => setDashboardOpen(false)}
          onOpenPrivacy={() => setPrivacyOpen(true)}
        />
      )}
      </>
      )}

      {/* The trust surface — rendered on WHICHEVER monitor opened it (a bubble
          or voice "Ask Claude" click sets this window's local state). Edits
          mutate `preview`; Send transmits exact bytes. */}
      {preview && (
        <ContextPreviewPanel
          payload={preview}
          onChange={setPreview}
          onClose={(result) => {
            // Cancel (no result): tell the core to drop its in-process session
            // — zero residue (doc 13 §3). After a Send the session is consumed;
            // after an MCP approve the panel passes `{}` so the approval stays.
            if (!result) void previewCancel(preview.payload_id).catch(() => {});
            closePreview();
          }}
        />
      )}
    </>
  );
}
