# Doc 07 — Voice / Push-to-Talk STT

## 1. Interface
| | |
|---|---|
| **Inputs** | Global PTT hotkey state; microphone audio **only while held**; mutex grants (Doc 12) |
| **Outputs** | `voice_utterance` event (always — telemetry role); a retrieval result → answer bubble (query role, Doc 11); optional escalation request → gateway (Doc 09) |
| **Resource cost** | Whisper small ~1 GB VRAM (or CPU fallback); audio buffers + Silero VAD negligible |

## 2. Capture path
- **Hotkey:** `RegisterHotKey` global hotkey (default e.g. `Ctrl+Win+Space` [ASSUMPTION], configurable). Press-and-hold semantics: key-down starts capture (visual "listening" pill, Doc 11), key-up stops it. Max utterance 30 s [ASSUMPTION].
- **Audio:** WASAPI shared-mode, default mic, resampled to 16 kHz mono PCM.
- **VAD:** Silero trims leading/trailing silence; < 300 ms of speech ⇒ discard (accidental tap).
- Mic permission denied ⇒ PTT disabled with a one-time explanatory notice.

## 3. Transcription (a GPU job under the mutex)
- **Default model: Whisper small** (~1 GB; ≈95 % of large-v3 quality at a fraction of the cost — the right default for an 8 GB card). **Opt-in: faster-whisper distil-large-v3 int8** (~1.48 GB measured). Served by the `stt-host` sidecar. [VERIFY latencies on hardware → SC4.]
- Job spec: `{kind: STT, priority: 100 (highest), payload: wav_ref}`. Voice preempts queued VLM jobs; in **L2** (7B resident) the manager performs unload→load swap (Doc 12 §3), which adds latency — the UI shows "thinking" rather than failing.
- **CPU fallback:** whisper small on CPU when the GPU is unavailable (driver issue, projection refused) — slower but functional [VERIFY real-time factor].
- Output: transcript + avg token confidence + duration. Always written as `voice_utterance{intent}` and embedded (telemetry role is unconditional — locked decision B).

## 4. Intent classification (query vs telemetry-only)
Deterministic, local, explainable — no model required in v1:
1. Leading-verb match against a command lexicon (`open, reopen, continue, find, show, resume, search, ask…`) or interrogatives (`what, where, when, which`) ⇒ **query**.
2. `"ask claude …"` prefix ⇒ **escalation intent** (still goes through the transparency gate; never auto-sends).
3. Otherwise ⇒ **telemetry-only** (stored + embedded, no UI).
4. Confidence < 0.6 [ASSUMPTION] ⇒ show a transcript chip ("Did you say: …?") with *Run* / *Dismiss* — never act on a guess.

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
| 7B-mode swap latency | UI "thinking" state; if > 6 s [ASSUMPTION], offer CPU fallback automatically |
