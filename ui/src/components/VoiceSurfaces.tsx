//! Voice surfaces (doc 07's UI, doc 11 §5). A single component that renders the
//  current voice surface driven by the `voice_surface` event:
//    - listening pill  — while PTT is held (waveform, release-to-stop);
//    - thinking         — model swap latency (L2): "thinking", never a failure;
//    - transcript chip  — low confidence (<0.6): "Did you say …?" + Run/Dismiss;
//    - answer bubble    — text + source line + optional Resume + Ask Claude (gated);
//    - empty            — honest empty state when retrieval finds nothing.
//
//  PTT key handling lives in the Rust shell (global hotkey, doc 07); this
//  component also wires `voice_ptt_down/up` to a pointer-held button so the pill
//  can be summoned by mouse. Nothing here bypasses the preview->Send gate: the
//  answer bubble's "Ask Claude" opens the Context-Preview panel (doc 07 §5).

import {
  bubbleClick,
  voicePttDown,
  voicePttUp,
  type VoiceSurfaceEvent,
} from "../lib/ipc";

interface Props {
  event: VoiceSurfaceEvent;
  /** Open the Context-Preview panel for the answer's resumable hit (gated). */
  onAskClaude: (actionRef: string) => void;
}

export function VoiceSurfaces({ event, onAskClaude }: Props) {
  switch (event.surface) {
    case "hidden":
      return null;

    case "listening":
      return (
        <div className="voice voice--pill surface-glass surface-interactive" role="status">
          <Waveform level={event.level ?? 0} />
          <span>Listening… release to stop</span>
          {/* Mouse-summon PTT: hold to talk. The global hotkey path is owned by
              the Rust shell; this is the optional summon affordance (doc 11 §2). */}
          <button
            className="btn btn--icon"
            aria-label="Hold to talk"
            onPointerDown={() => void voicePttDown()}
            onPointerUp={() => void voicePttUp()}
            onPointerLeave={() => void voicePttUp()}
          >
            ●
          </button>
        </div>
      );

    case "thinking":
      return (
        <div className="voice voice--pill surface-glass surface-interactive" role="status">
          <span className="voice__spinner" aria-hidden />
          <span>Thinking…</span>
        </div>
      );

    case "transcript":
      // Confidence < 0.6 => never act on a guess; confirm first (doc 07 §4).
      return (
        <div className="voice voice--chip surface-glass surface-interactive" role="alertdialog">
          <span className="voice__chip-q">Did you say:</span>
          <span className="voice__chip-text">“{event.text}”</span>
          <div className="voice__chip-actions">
            {/* TODO(M6:) Run re-issues the confirmed transcript as a query. */}
            <button className="btn btn--primary">Run</button>
            <button className="btn">Dismiss</button>
          </div>
        </div>
      );

    case "answer":
      return (
        <div className="voice voice--answer surface-glass surface-interactive" role="dialog">
          <div className="voice__answer-title">{event.title}</div>
          {event.source && <div className="voice__answer-source">{event.source}</div>}
          <div className="voice__answer-actions">
            {event.action_ref && (
              <button
                className="btn btn--primary"
                onClick={() => void bubbleClick("voice-answer", event.action_ref as string)}
              >
                Resume
              </button>
            )}
            {/* "Ask Claude" is gated: it opens the preview->Send trust surface. */}
            <button
              className="btn"
              disabled={!event.can_ask_claude}
              onClick={() => onAskClaude(event.action_ref ?? "")}
            >
              Ask Claude
            </button>
          </div>
        </div>
      );

    case "empty":
      // Honest empty state when retrieval finds nothing (doc 07 §5).
      return (
        <div className="voice voice--chip surface-glass surface-interactive" role="status">
          <span className="voice__empty">{event.message}</span>
        </div>
      );
  }
}

/** Minimal level-driven waveform. Animates `transform` only (doc 14 §2);
 *  the actual level comes from the listening event. */
function Waveform({ level }: { level: number }) {
  const bars = [0.4, 0.7, 1, 0.7, 0.4];
  return (
    <span className="voice__wave" aria-hidden>
      {bars.map((base, i) => (
        <span
          key={i}
          className="voice__wave-bar"
          style={{ transform: `scaleY(${Math.max(0.15, base * Math.min(1, level || 0.5))})` }}
        />
      ))}
    </span>
  );
}
