# Doc 07 — Voice / Push-to-Talk STT

## 1. Interface
| | |
|---|---|
| **Inputs** | Global PTT hotkey state; microphone audio **only while held**; mutex grants (Doc 12) |
| **Outputs** | `voice_utterance` event (always — telemetry role); a retrieval result → answer bubble (query role, Doc 11); optional escalation request → gateway (Doc 09) |
| **Resource cost** | faster-whisper small ~1 GB VRAM (or CPU fallback); audio buffers + Silero VAD negligible |

## 2. Capture path
- **Hotkey:** `RegisterHotKey` global hotkey (default e.g. `Ctrl+Win+Space` [ASSUMPTION], configurable). Two user-selectable capture modes: **press-and-hold** — key-down starts capture (visual "listening" pill, Doc 11), key-up stops it; and **click-to-toggle** — a key press (or a click on the pill) toggles listening on, and a second press/click stops it. Toggle mode stays bounded by the same 30 s cap + always-visible listening pill, so "no always-listening" (NG2) still holds. Max utterance 30 s [ASSUMPTION].
- **Audio:** WASAPI shared-mode, default mic, resampled to 16 kHz mono PCM.
- **VAD:** Silero trims leading/trailing silence; < 300 ms of speech ⇒ discard (accidental tap).
- Mic permission denied ⇒ PTT disabled with a one-time explanatory notice.

## 3. Transcription (a GPU job under the mutex)
- **GPU path — faster-whisper (CTranslate2):** default **faster-whisper small** (≈95 % of large-v3 quality at a fraction of the cost — the right default for an 8 GB card). **Opt-in: faster-whisper distil-large-v3 int8** (~1.48 GB measured). Served by the `stt-host` sidecar. [VERIFY latencies on hardware → SC4.]
- Job spec: `{kind: STT, priority: 100 (highest), payload: wav_ref}`. Voice preempts queued VLM jobs; in **L2** (7B resident) the manager performs unload→load swap (Doc 12 §3), which adds latency — the UI shows "thinking" rather than failing.
- **CPU fallback:** **whisper.cpp base (default) / tiny** (a separate, cheap GGML build) when the GPU is unavailable (driver issue, projection refused) — slower but functional; **SC4 (<2 s) is not promised on the CPU path** [VERIFY real-time factor].
- Output: transcript + avg token confidence + duration. Always written as `voice_utterance{intent}` and embedded (telemetry role is unconditional — locked decision B).

## 4. Intent classification (query vs telemetry-only)
Local, adaptive, and explainable — a lightweight classifier head plus a deterministic override, no separate resident model:
1. **Primary — a lightweight classifier head over the nomic embedding** we already compute for every utterance (~0 extra resident RAM — it reuses the embedder). The head scores the transcript across **query / escalation / telemetry-only** with a calibrated confidence.
2. **Deterministic lexicon fast-path (overrides the head for clear cases):** a leading-verb match against a command lexicon (`open, reopen, continue, find, show, resume, search, ask…`) or interrogatives (`what, where, when, which`) forces **query**; an `"ask claude …"` prefix forces **escalation intent** (still goes through the transparency gate; never auto-sends). Obvious commands therefore never depend on the model.
3. Otherwise the head's top class stands — **telemetry-only** ⇒ stored + embedded, no UI.
4. Confidence < 0.6 [ASSUMPTION] ⇒ show a transcript chip ("Did you say: …?") with *Run* / *Dismiss* — never act on a guess.
5. The head **refines online from confirm-chip corrections**: each *Run* / *Dismiss* is a labeled example, so accuracy improves over time with no cloud call.

## 5. Query execution
```
transcript ─► embed ─► retrieval SQL (Doc 03 §5, temporal phrases set the time window)
           ─► re-rank ─► top hit ─► answer bubble {title, when, source, [Resume], [Ask Claude]}
```
- *Resume* dispatches Critical Path B (Doc 02 §5) on the hit's `connector_state`.
- *Ask Claude* assembles a payload whose items are: the transcript (`user_addition`), the top-k hits' summaries (`event_trail`) — then the standard preview→Send gate (Doc 13). Nothing about voice bypasses the gate.
- No hit above score floor ⇒ honest empty-state bubble ("Nothing matching in your history") with *Ask Claude* offered.

## 6. Failure modes
| Failure | Behavior |
|---|---|
| GPU busy (mutex held by VLM) | STT priority 100 preempts at the next cancellation point; worst case queued ≤ 2 s [VERIFY], else CPU fallback |
| stt-host crash | Resource Manager restarts with backoff; this utterance falls back to CPU |
| Hotkey conflict with another app | Registration failure surfaces a settings prompt to rebind |
| Noisy transcript | Confidence path (§4.4) — confirm before acting |
| 7B-mode swap latency | UI "thinking" state; past a **user-configurable swap threshold** (default 6 s [ASSUMPTION]), fall back to CPU automatically |

---
## Implementation status (2026-07-08) — M6 (software)

Built + CPU-tested in `crates/voice` + `crates/stt-host`:
- `VoiceSubsystem::process_utterance`: VAD trim → priority-100 STT `GpuJob` → **unconditional** `voice_utterance` store+embed → deterministic intent → query(retrieval) / escalation(preview draft, never auto-sent) / telemetry branch, with the `<0.6` confirm chip. End-to-end tested against a fake scheduler + in-memory DB.
- Intent: the `ask claude` escalation prefix now requires a following word boundary ("ask claudes plan" classifies as a query, not an escalation).
- **Decision — VAD backend:** M6 ships a deterministic **energy-gate** trim (RMS, tested) behind the `frame_is_speech` seam; **Silero VAD (ONNX, §2) is the on-hardware quality upgrade**, not yet wired.
- **Flags (UNVERIFIED, best-effort — compile against the real crate APIs, not run without a mic/GPU):** cpal WASAPI capture, `global-hotkey` PTT, the `stt-host` whisper.cpp child. **SC4 latency is `#[ignore]`** pending the RTX 5060.
- **Deferred:** the confirm-chip **Run** action needs a `voice_run_transcript` core command (Dismiss + Escape work today); composition-root wiring (PTT hotkey thread, capture-toggle→enable/disable, warm-keep→`set_warm_kept`) is not yet constructed in the shell.

Full session detail: `docs/handoff/session-bridge-2026-07-08-m6-m8.md`.

> **R2 amendments applied** (see docs/19–21): Q49 (click-to-toggle PTT), ADR-024 (faster-whisper GPU / whisper.cpp CPU), ADR-034 (embedding-head intent + lexicon fast-path), Q52 (configurable CPU-fallback threshold).
