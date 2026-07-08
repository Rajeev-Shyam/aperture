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
  onVoiceSurface,
  requestPreview,
  type ContextPayload,
  type Intent,
  type UnlistenFn,
  type VoiceSurfaceEvent,
} from "./lib/ipc";
import { startGpuBusyWatch } from "./state/gpuBusy";

import { BubbleContainer } from "./components/BubbleContainer";
import { CaptureIndicator } from "./components/CaptureIndicator";
import { ContextPreviewPanel } from "./components/ContextPreviewPanel";
import { VoiceSurfaces } from "./components/VoiceSurfaces";

export default function App() {
  // The previewed payload, or null when no panel is open. Editing this object
  // in the panel IS editing what will ship (doc 11 §4 invariant).
  const [preview, setPreview] = useState<ContextPayload | null>(null);

  // Latest voice surface event (listening pill / transcript chip / answer).
  const [voice, setVoice] = useState<VoiceSurfaceEvent>({ surface: "hidden" });

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

  return (
    <>
      {/* Bottom-right suggestion stack (≤3 visible, queue overflow). */}
      <BubbleContainer onAskClaude={(actionRef) => void openPreview("explain_pattern", actionRef)} />

      {/* Voice surfaces: listening pill / transcript chip / answer bubble. */}
      <VoiceSurfaces
        event={voice}
        onAskClaude={(actionRef) => void openPreview("answer_query", actionRef)}
      />

      {/* Capture state + activity pulse; OFF reflects VRAM->~0 (<3s). */}
      <CaptureIndicator />

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
