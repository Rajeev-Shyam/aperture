# Doc 01 — Product Requirements & Scope (PRD)

## 1. Vision
Aperture watches how you work (only while you let it), learns your recurring patterns locally, and offers one-click resumption of the things you keep coming back to — rendered as proactive glass bubbles. Voice (push-to-talk) feeds the same behavioral model and can query it. Claude is the optional heavy-reasoning tier, invoked only when you explicitly ask, and you always see and approve the exact bytes that leave the machine.

## 2. Goals
| ID | Goal | Primary docs |
|---|---|---|
| G1 | Screen understanding → proactive shortcut suggestions as bubbles (must work well first) | 05, 06, 08, 11 |
| G2 | Bubbles are actions: click performs deep-link / state resumption | 10, 11 |
| G3 | Bounded connector set (browser URL, video timestamp, document, IDE file), expandable | 10 |
| G4 | Suggestions appear proactively on local pattern detection | 08 |
| G5 | Push-to-talk voice as telemetry **and** query channel answered with text | 07, 11 |
| G6 | Reasoning split: local capture/patterns; Claude for heavy reasoning, explicit-only | 08, 09, 12 |
| G7 | Context transparency: exact-payload preview + one-click "make richer" before any cloud call | 09, 11, 13 |
| G8 | Capture toggle: clean ON/OFF, resource release, clear indicator | 05, 12 |
| G9 | Fit 8 GB VRAM / 16 GB RAM / Ryzen; idle cheaply | 04, 12 |
| G10 | Chromemorphism & Liquid Meta as a documented design system | 14 |

## 3. Non-goals (v1) — with rationale
- **NG1 Arbitrary-app resumption.** Generic UI replay is unbounded and fragile; the bounded connector set covers the high-value cases and the interface (Doc 10) leaves the door open.
- **NG2 Always-listening voice.** Privacy posture and idle cost; PTT only.
- **NG3 Text-to-speech.** Adds a model + latency on a constrained GPU; answers render as text.
- **NG4 Cloud-resident history / sync.** The local-DB boundary is a product promise, not a setting.
- **NG5 Cloud-dependent proactivity.** The proactive loop must work offline; Claude never gates a bubble.
- **NG6 Multi-user / multi-device.** Single user, single machine.
- **NG7 Non-Windows platforms.**
- **NG8 Hard-coded Claude model/headers.** Transport and model are swappable; specifics [VERIFY at build time].

## 4. Primary user stories — with acceptance criteria

### US1 — Proactive YouTube resume (the flagship)
*Narrative:* While working, the user has repeatedly returned to a paused tutorial. The pattern engine detects the recurrence and surfaces: **"Continue your YouTube video — 12:34."** Clicking reopens the exact video at the captured timestamp.
**Accept when:** (a) the pattern fires after ≥3 observed returns [ASSUMPTION: min support, Doc 08]; (b) bubble appears < 2 s after the trigger event (SC2); (c) click opens the default browser at `…watch?v=<id>&t=<s>s` within 2 s; (d) if no timestamp was capturable, the bubble says "from the start" and still opens the video (graceful degrade, Doc 10); (e) zero network egress occurred to produce the suggestion (SC5).

### US2 — Voice query over history
*Narrative:* User holds the PTT hotkey: *"reopen that article from yesterday."* Local STT transcribes; the retrieval path searches the history DB; the best match renders as a text bubble with a clickable resume action.
**Accept when:** (a) transcription of a 5–10 s utterance completes < 2 s (SC4); (b) the answer bubble shows title + source + a resume action; (c) if confidence is low, the transcript is shown for confirmation before acting (Doc 07); (d) no cloud call occurs unless the user explicitly escalates ("Ask Claude"); (e) the utterance is stored as a `voice_utterance` event and embedded (telemetry role).

### US3 — Context enrichment to Claude
*Narrative:* A bubble offers "Ask Claude to summarize this." The user clicks; a panel shows the **exact** payload (OCR text, event trail, connector item, redactions applied, transport target) with "make context richer" affordances; only on **Send** does anything leave the machine.
**Accept when:** (a) the previewed serialized object is byte-identical to the wire payload (Doc 13 invariant); (b) every redaction is listed with rule + count; (c) the user can remove any item before sending; (d) Cancel results in zero egress; (e) the structured answer renders in a bubble using the same suggestion schema as local output (Doc 09).

### US4 — Capture toggle
*Narrative:* User clicks the tray indicator OFF. Capture stops, recording stops, GPU models unload, indicator flips to inactive.
**Accept when:** (a) no events are written after OFF; (b) GPU VRAM for Aperture models returns to ~0 within 3 s (SC6); (c) the WGC session and UIA hooks are released (verifiable: no capture border, handle count drops); (d) ON restores capture without restart; (e) both transitions are recorded as `capture_toggle` audit events.

### US5 — Workflow shortcut
*Narrative:* The engine notices the user always opens a specific spreadsheet after a specific email and offers a one-click resume bubble for the spreadsheet.
**Accept when:** the 2-step sequence pattern (email-app → document) reaches threshold and the bubble's click opens the exact file path via the document connector.

## 5. Success criteria — measurable, with method
| ID | Criterion | Target | Measurement method |
|---|---|---|---|
| SC1 | Idle footprint (capture ON, no GPU job) | < 1.5 GB system RAM; ~0 model VRAM [VERIFY] | Task Manager / `nvidia-smi` over 30 min idle |
| SC2 | Pattern trigger → bubble latency (local path) | < 2 s [VERIFY] | Instrumented timestamps event→render |
| SC3 | VLM cold load + 1-frame understanding | < 6 s cold, < 2 s warm [VERIFY] | Harness on RTX 5060 |
| SC4 | PTT transcription, 5–10 s utterance | < 2 s [VERIFY] | Harness, Whisper small |
| SC5 | **Zero egress on the proactive path; cloud only after explicit Send** | 0 bytes | Network monitor (e.g. mitmproxy/ETW) in CI + manual |
| SC6 | Capture OFF → full GPU release | < 3 s [VERIFY] | `nvidia-smi` delta on toggle |
| SC7 | Suggestion usefulness (dogfood) | ≥ 50 % rated useful [VERIFY] | click/dismiss telemetry in `suggestions` table |

## 6. v1 boundary
**In:** single user, single Windows 11 machine; the four connectors; fully local proactivity; explicit-only Claude with transparency gate; capture toggle; glass UI; at-rest encryption; exclusion lists; retention/purge.
**Out:** everything in §3.

## 7. Traceability
Every goal maps to components (table in §2); every story maps to flows verified in Doc 18. Orphan check lives in Doc 18 §1.
