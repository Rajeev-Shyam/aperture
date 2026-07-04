# Doc 02 — System Architecture Overview

## 1. The tiered model
| Tier | Residency | Contents | Cost profile | Failure isolation |
|---|---|---|---|---|
| **Tier 0 — always-on local (CPU/RAM)** | Runs whenever capture is ON | WGC sampler, Win32/UIA event hooks, cheap OCR, embedding writer, SQLite + sqlite-vec, behavior/pattern engine, suggestion generator, connector capture, **the browser extension + native-messaging host** (ADR-027: primary browser URL + video-position source) | Idles cheaply; no GPU model resident | A Tier-0 fault degrades suggestions, never the OS |
| **Tier 1 — on-demand GPU (8 GB, mutex)** | Loaded on trigger, idle-unloaded | VLM (Qwen2.5-VL **3B default / 7B opt-in**), **faster-whisper** STT — run as **sidecar processes** | VRAM-bound; one heavyweight model at a time (conditional co-residency, ADR-030) | Sidecar crash ≠ app crash; kill = guaranteed VRAM release |
| **Tier 2 — cloud (explicit-only)** | Never resident; per-call | Claude via swappable transport, **MCP-primary** (Desktop MCP → Code CLI → Messages API; ADR-025) | Network + tokens, only on explicit Send / scoped allow | Offline ⇒ product still fully works (NG5) |

## 2. Process model **[ASSUMPTION, with rationale]**
- **Main process:** Tauri (Rust core + WebView2 UI). Hosts the Event Bus, SQLite, pattern engine, connectors, orchestration, and the overlay window.
- **Model-host sidecars:** `vlm-host` (llama.cpp server with the Qwen2.5-VL mmproj) and `stt-host` (whisper.cpp / faster-whisper server), spawned and killed by the Resource Manager (Doc 12).
- *Why sidecars:* process termination is the only **guaranteed** way to return VRAM to the driver, which is what makes SC6 (full release < 3 s on toggle-OFF) and the OOM degrade ladder enforceable. In-process bindings make unload best-effort. [VERIFY exact server binaries/flags.]
- **Native-messaging host:** a small stdio bridge (no socket) between the browser extension and the Rust core (ADR-028); an authenticated 127.0.0.1 loopback is the fallback only. Its forwarding is gated by the capture toggle (FIX 2.1).
- **Browser extension (ours, but sandboxed by the browser):** a Manifest V3 extension for Chrome + Opera GX, reading URLs + `video.currentTime` only — never page DOM/content (ADR-029).
- **External processes (not ours):** Claude Desktop (MCP, primary) or the `claude` CLI, when those transports are configured; and the Tauri app-updater (framework path, no user data — ADR-036).

## 3. Labeled component map
```
                      ┌──────────────── TIER 2 (cloud, explicit-only) ────────────────┐
                      │   Reasoning Gateway ──(swappable transport)── Claude          │
                      │        ▲  gated by the Context-Transparency gate (Doc 13)     │
                      └────────┼───────────────────────────────────────────────────────┘
                               │
┌──────────────────────────────┼──────────── TIER 1 (on-demand GPU, single mutex) ─────┐
│   Orchestration & Resource Manager ── GPU Job Queue ── GPU Mutex (single permit)      │
│        │ spawns/kills                  │                                              │
│   [vlm-host sidecar]            [stt-host sidecar]   ← only ONE heavyweight resident   │
└────────┼───────────────────────────────┼─────────────────────────────────────────────┘
         │ structured scene JSON         │ transcript
┌────────┼───────────────────────────────┼──────────── TIER 0 (always-on local) ───────┐
│ Capture & Event Subsystem ─→ Event Bus ─→ Behavior & Pattern Engine ─→ Suggestion Gen │
│      │            │                          │  ▲ feedback events            │        │
│ Cheap OCR    Connector State Capture     SQLite + sqlite-vec            Bubble UI /    │
│ (CPU)             │                          ▲                          Overlay        │
│                   └──────────────────────────┘                              │          │
│ Deep-Link Connectors (browser/video/document/IDE) ◄──── bubble click ───────┘          │
└───────────────────────────────────────────────────────────────────────────────────────┘
 Cross-cutting: Privacy/Consent (13) · Design System (14) · Interface Contracts (15)
```

## 4. Critical Path A — proactive suggestion generation (fully local; latency budget 2 s)
| Step | Component | Data in → out | Budget |
|---|---|---|---|
| 1 | Capture (05) detects a meaningful OS event (focus/open/navigation) via WinEvent/UIA | OS event → normalized `Event` on the bus | ~10 ms |
| 2 | Capture samples one frame (WGC) for that event; excluded apps are skipped here | frame (ephemeral) | ~50 ms |
| 3 | Cheap OCR (06) extracts text on CPU | frame → `ocr_text` + confidence | ≤ 400 ms [VERIFY] |
| 4 | Connector capture (10) snapshots the resumable handle if `can_capture` | event → `connector_state` row | ~10 ms |
| 5 | Store (03): event + screen_context written; embedding computed (nomic-embed CPU) and inserted into `ctx_vec` | rows + 768-d vector | ≤ 300 ms [VERIFY] |
| 6 | Pattern engine (08) updates signatures incrementally; trigger rule evaluates | event tail → `SuggestionCandidate{action, connector_id, confidence}` | ≤ 200 ms |
| 7 | Suggestion generator formats a `BubbleSpec` (title, glyph, action_ref) | candidate → spec | ~5 ms |
| 8 | Bubble UI (11) renders the glass bubble; `suggestion_shown` event recorded | spec → pixels | ≤ 200 ms |
**Invariants:** no GPU job and no network on this path. The VLM may be invoked here *optionally* (Doc 06 gating) but a bubble must never wait on it — VLM output enriches the *next* cycle, not this one. [ASSUMPTION: keeps SC2 honest.]

## 5. Critical Path B — bubble click → state resumption
| Step | Component | Data |
|---|---|---|
| 1 | Bubble UI resolves `action_ref` → `connector_id` | click event |
| 2 | SQLite returns `connector_state` (type + `reconstruct_payload` JSON) | row |
| 3 | Connector `reconstruct()` builds the `ResumeArtifact` — YouTube `…&t=754s`; IDE `vscode://file/C:/p/x.rs:120:5`; browser stored URL; document file path + app hint | artifact |
| 4 | Connector `open()` dispatches via `ShellExecuteW` / registered protocol handler | OS launch |
| 5 | Result returns to Bubble UI; failure ⇒ graceful fallback (open without precise state) + `suggestion_clicked{outcome}` recorded | feedback |

## 6. Secondary paths
- **Path C — voice:** PTT hotkey (07) → WASAPI capture while held → VAD trim → STT GPU job via the mutex → transcript stored as `voice_utterance` (telemetry) **and**, if classified a query, run §3.5 retrieval (Doc 03) → answer bubble with optional resume action. Escalation to Claude only via the gate.
- **Path D — explicit cloud:** enrichment click (11) → payload assembled (03 §4) → redaction (13) → **preview panel** → user Send → Reasoning Gateway (09) → transport → structured suggestions → Bubble UI renders identically to local output.

## 7. State ownership (single-writer rule)
| State | Owner | Readers |
|---|---|---|
| Capture toggle (ON/OFF) | Orchestration Manager (12) | Capture, UI indicator, sidecars |
| GPU mutex + job queue | Orchestration Manager | VLM/STT callers |
| Durable history | SQLite (03), written by Tier-0 pipeline | pattern engine, retrieval, payload builder |
| Context payload (per request) | Payload builder (03/13) | preview UI, gateway — same object |
| Suggestion lifecycle | Bubble UI (11) | pattern engine (feedback) |

## 8. Master connection rule
Tier 0 talks over the in-process Event Bus with SQLite as the durable backbone. **Tier 0→1 passes exclusively through the Orchestration Manager** (no component touches the GPU directly). **Anything→2 passes exclusively through the Reasoning Gateway after the transparency gate** — including the opt-in, aggregate-only diagnostics path (ADR-036); the app-updater is the one framework-level exception, carrying no user data. The five contracts that make components independently buildable are specified in Doc 15.

---
> **R2 amendments applied** (see docs/19–21): ADR-027 (extension + native-messaging host as a Tier-0 capture source), ADR-028 (native-messaging transport + loopback fallback), ADR-029 (extension URL/position-only), ADR-025 (MCP-primary transport), ADR-030 (conditional co-residency), ADR-036 (diagnostics via gateway; updater carve-out).
