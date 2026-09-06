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
  onAuditAlert,
  onDashboardOpen,
  onPreviewClaimed,
  onPreviewRequest,
  onPrivacyOpen,
  onVoiceSurface,
  openDashboard,
  openPrivacy,
  previewCancel,
  requestPreview,
  resetOverlayInteractivity,
  setPreviewHost,
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

import { AgentSurface } from "./components/AgentSurface";
import { BubbleContainer } from "./components/BubbleContainer";
import { ContextPreviewPanel } from "./components/ContextPreviewPanel";
import { Dashboard } from "./components/Dashboard";
import { FirstRunConsent } from "./components/FirstRunConsent";
import { Hud } from "./components/Hud";
import { PrivacyPanel } from "./components/PrivacyPanel";
import { SnoozeControl } from "./components/SnoozeControl";
import { VoiceSurfaces } from "./components/VoiceSurfaces";

// Which monitor's overlay is this root running in? The primary window keeps
// the config label `overlay`; per-monitor clones are `overlay-1`, `overlay-2`…
// (overlay.rs `plan_overlays`).
//
// Surface split (decision #13 — controls follow the user):
//   - Dashboard / Privacy / Context-Preview open on WHICHEVER window summoned
//     them (bubble ⋯ menu, HUD button, "Ask Claude") or was routed the summon
//     by the core (tray/second-launch → cursor's monitor; MCP staged preview →
//     preview-host window, else cursor). Broadcast `{target}` events keep
//     exactly one instance alive across monitors.
//   - The HUD (indicator + buttons), audit banners, and first-run stay on the
//     primary ALONE: they are persistent/ambient, not summoned — duplicating
//     them per monitor meant duplicate indicators, dialogs, and double
//     settings writes (doc 11 §2, M8 cleanup). The HUD's buttons still follow
//     the summon rule trivially: the cursor is on the HUD when they're
//     clicked. From a secondary monitor, the Dashboard/Privacy routes are the
//     tray and any bubble's ⋯ menu — both cursor-routed.
const WINDOW_LABEL = getCurrentWindow().label;
const IS_PRIMARY_OVERLAY = WINDOW_LABEL === "overlay";

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

  // Audit-trail failure warnings (decision #41): each stays until the user
  // dismisses it — the trail is the sole record of what left this machine, and
  // a gap in it must not fade away on its own.
  const [auditAlerts, setAuditAlerts] = useState<string[]>([]);

  // Consent (doc 13 §8). `null` = not yet read; the first-run flow renders only
  // once we KNOW first-run is incomplete, so a slow read never flashes it at a
  // user who already consented.
  const [consent, setConsent] = useState<ConsentState | null>(null);
  const [privacyOpen, setPrivacyOpen] = useState(false);
  const [dashboardOpen, setDashboardOpen] = useState(false);
  // v2 (Doc 22 §9.1): the "New agent task" composer, opened from the HUD.
  const [agentComposing, setAgentComposing] = useState(false);

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
    // The tray, a second app launch, and the HUD ◎ all land here, broadcast
    // with a target (decision #13): this window opens the dashboard iff it is
    // the target, and CLOSES its copy otherwise — one instance, everywhere.
    void onDashboardOpen((target) => setDashboardOpen(target === WINDOW_LABEL)).then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    // The core staged a payload (Claude Desktop's gated search, ADR-037):
    // open the trust surface on it so the user decides what leaves. Targeted
    // at the preview-host window (else the cursor's monitor) — if a panel is
    // already open here (possibly mid-edit), the request queues; the trust
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
    // Another window's preview panel opened (decision #13): fold ours —
    // cancelling the displaced sessions core-side, same policy as an in-window
    // user open replacing the panel — so exactly one preview exists. With
    // host-routing, a queue here is a rare race; cancel is its honest end.
    void onPreviewClaimed((target) => {
      if (target === WINDOW_LABEL) return;
      previewQueue.current.forEach((p) => void previewCancel(p.payload_id).catch(() => {}));
      previewQueue.current = [];
      setPreview((cur) => {
        if (cur) void previewCancel(cur.payload_id).catch(() => {});
        return null;
      });
    }).then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    // A bubble's "Exclusions…" / the HUD 🛡 opens the privacy panel on the
    // target window; every other window closes its copy (decision #13).
    void onPrivacyOpen((target) => setPrivacyOpen(target === WINDOW_LABEL)).then((u) => {
      if (cancelled) u();
      else unlisteners.push(u);
    });
    // Audit-write failures (decision #41). The event broadcasts; only the
    // primary renders the banner (same split as the HUD) so one failure never
    // yields one banner per monitor.
    void onAuditAlert((a) => setAuditAlerts((cur) => [...cur, a.message])).then((u) => {
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

  // Mirror this window's panel-open state to the core (decision #13): open
  // claims the preview-host slot (MCP requests route + queue here; other
  // windows fold their panels via `preview_claimed`), close releases it.
  // Transition-gated: closing into a queued payload keeps the panel open, so
  // no release fires mid-queue.
  const wasPreviewOpen = useRef(false);
  useEffect(() => {
    const open = preview !== null;
    if (open === wasPreviewOpen.current) return;
    wasPreviewOpen.current = open;
    void setPreviewHost(open).catch((e) =>
      console.error("set_preview_host failed; multi-monitor preview routing may drift", e),
    );
  }, [preview]);

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

  // First run is the ONLY surface the overlay renders until it completes:
  // privacy setup precedes every other surface (doc 13 §8), and capture is OFF
  // behind it regardless (`ConsentState` defaults + `complete_first_run` is
  // the only enable path). It is answered ONCE, on the primary monitor — a
  // copy per monitor was N dialogs for one question. Non-exclusive since
  // decision #12: the card is focused and clickable, the rest of the monitor
  // stays click-through.
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

      {/* Primary-only: the persistent/ambient chrome (HUD, audit banners).
          The summonable panels below render on ANY window — routing keeps
          each to exactly one instance (decision #13). */}
      {IS_PRIMARY_OVERLAY && (
      <>
      {/* Audit-trail failure banners (decision #41): opaque, dismissible,
          honest — the send happened, the record of it did not. */}
      {auditAlerts.length > 0 && (
        <div className="audit-alerts" role="region" aria-label="Audit warnings">
          {auditAlerts.map((msg, i) => (
            <div key={`${i}-${msg}`} className="audit-alert surface-opaque surface-interactive" role="alert">
              <span aria-hidden>⚠</span>
              <span className="audit-alert__msg">{msg}</span>
              <button
                className="btn btn--icon"
                aria-label="Dismiss audit warning"
                onClick={() => setAuditAlerts((cur) => cur.filter((_, j) => j !== i))}
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}
      {/* v2 agent surface (Doc 22 §9, decision #53): status bar + live log
          while a task exists, and the pause cards. Primary-only — one task
          at a time, one place to stop it. */}
      <AgentSurface composing={agentComposing} onCloseComposer={() => setAgentComposing(false)} />
      {/* The HUD orb: the capture dot, expanding on click into one row —
          capture toggle (owned by the Hud) + these App-owned controls + hide.
          Draggable to any corner or edge (persisted in ui.hud_anchor),
          hideable (ui.hud_hidden, the tray brings it back). */}
      <Hud>
        {/* Global snooze (doc 11 §6, ADR-040): quiet bubbles, keep learning. */}
        <SnoozeControl />
        <button
          className="privacy-open"
          aria-label={dashboardOpen ? "Close the Aperture dashboard" : "Open the Aperture dashboard"}
          aria-pressed={dashboardOpen}
          title="Dashboard — history, patterns, everything it knows"
          onClick={() => {
            // Open optimistically AND through the core: the broadcast is
            // what closes a copy open on another monitor (decision #13).
            if (dashboardOpen) setDashboardOpen(false);
            else {
              setDashboardOpen(true);
              void openDashboard().catch(() => {});
            }
          }}
        >
          ◎
        </button>
        <button
          className="privacy-open"
          aria-label={agentComposing ? "Close the agent task composer" : "New agent task"}
          aria-pressed={agentComposing}
          title="New agent task — Claude drives this PC, step by step, with you in control"
          onClick={() => setAgentComposing((v) => !v)}
        >
          ✦
        </button>
        <button
          className="privacy-open"
          aria-label={privacyOpen ? "Close activity and privacy" : "Open activity and privacy"}
          aria-pressed={privacyOpen}
          title="Activity & Privacy"
          onClick={() => {
            if (privacyOpen) setPrivacyOpen(false);
            else {
              setPrivacyOpen(true);
              void openPrivacy().catch(() => {});
            }
          }}
        >
          🛡
        </button>
      </Hud>
      </>
      )}

      {/* Summonable control surfaces — rendered on WHICHEVER window the routed
          open event targeted (decision #13); the broadcasts above guarantee at
          most one instance of each across all monitors. */}
      {privacyOpen && (
        <PrivacyPanel
          dbEncrypted={consent?.db_encrypted ?? false}
          onClose={() => setPrivacyOpen(false)}
        />
      )}
      {dashboardOpen && (
        <Dashboard
          onClose={() => setDashboardOpen(false)}
          onOpenPrivacy={() => {
            // Routed like every other open, so a panel elsewhere converges.
            setPrivacyOpen(true);
            void openPrivacy().catch(() => {});
          }}
        />
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
