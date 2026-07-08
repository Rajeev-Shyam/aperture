<!-- Handoff/process doc (not an architecture doc). Bridges the session that built
     M6 (voice), M7 (reasoning gateway), and M8 (design-system hardening), ran a
     multi-agent review, and fixed every confirmed finding — into M9 + v1 close-out.
     Authoritative design remains Docs 00–21; this captures state, decisions, flags,
     and carry-forward. Supersedes docs/handoff/m6-m7-session-bridge.md (2026-07-06). -->

# 🔄 CLAUDE SESSION BRIDGE — M6 / M7 / M8 done → M9 + v1 — read this first

**Session date: 2026-07-08**
**Bridges into: M9 (privacy hardening, Doc 13) + the v1 close-out (on-hardware gates, composition-root wiring)**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`

You are a fresh Claude session picking up **Aperture** — a local-first, privacy-preserving,
proactive desktop assistant for Windows 11 (Tauri v2 + a Rust workspace + a React/Vite
WebView overlay). Read this whole file first. Apply **"How this user works"** (bottom) from
your very first line. Ask at most ONE clarifying question, and only if genuinely blocked.

---

## Where the build is

**M0–M8 are landed in software and green** (per-crate `cargo test` passes; the UI typechecks
via `tsc`). Nothing from this session is committed yet — check `git status` (Rajeev commits).

| Milestone | State |
|---|---|
| M0–M4 | Landed + committed. Contracts/schema/shell, capture+toggle, OCR/embed/store, pattern engine + overlay, 4 connectors + browser extension + native-messaging bridge. |
| M5 | Landed (committed `6e8e8ea`). Orchestration resource manager + on-demand VLM enrichment; software gates pass, on-hardware `#[ignore]`. |
| **M6** | **Landed in software (uncommitted).** Voice/PTT/STT + the 3 deferred lifecycle pieces, all wired. |
| **M7** | **Landed in software (uncommitted).** Reasoning gateway + transparency gate + 3 transports; redaction pulled forward from M9. |
| **M8** | **Landed in software (uncommitted).** Glass-surface budget, degrade-under-load, multi-monitor overlay. |
| Review | 5-role multi-agent review + adversarial verify; **21 confirmed findings, all fixed** this session. |
| M9 → | Next. Privacy hardening (Doc 13). Not started. |

**The three invariants (NEVER re-open):** ① 8 GB VRAM ceiling — BudgetEnforcer admits only
≤ **7.0 GB** projected (ADR-030, co-resident weights counted); ② two-emitter transparency gate
— ONLY the `reasoning-gateway` crate opens a socket / spawns the Claude CLI, and ONLY on a
user-approved payload; ③ capture toggle — OFF releases capture + kills sidecars, VRAM→~0 in < 3 s.

---

## What this session did — in depth

### M6 — Voice / PTT / STT (Doc 07)
- **`crates/voice` per-utterance pipeline** (`VoiceSubsystem::process_utterance`, CPU-tested with a
  `FakeScheduler` + in-memory DB): VAD trim → accidental-tap gate (<300 ms) → priority-100 STT
  `GpuJob` → **unconditional** `voice_utterance` store+embed (locked decision B — happens *before* any
  intent branch) → deterministic intent → branch: query→`retrieval::run`, escalation→preview draft
  (never auto-sent), telemetry→stored-silently; `<0.6` confidence → confirm chip.
- **`retrieval`**: query-prefix embed → temporal-phrase window → KNN → re-rank
  `(1-dist)·recency·resumable-boost` → answer bubble / honest empty state.
- **`intent_classifier`** + **`stt_job`** were already implemented; intent got a word-boundary fix (below).
- **Orchestration — the 3 deferred lifecycle pieces, now wired:**
  - `ModelLifecycle::l2_swap_to_stt` (evict the exclusive VLM; `SwapOutcome::Swapped{thrash_risk}`)
    **AND** the scheduler now invokes it (`gpu_scheduler::admit_and_run`): an L2 STT arrival refused
    because the 7B is resident evicts it + re-admits — voice is never starved (doc 12 §3).
  - Crash-restart ladder: `gpu_scheduler::acquire_endpoint` routes a cold-load failure through
    `handle_crash` (backoff + 3-strike + Degraded fallback).
  - `warm_keep::PttWarmKeep` (≥2 PTT/5 min pins STT); idle sweep honors it.
- **CONN-M2 fully fixed** (the top deferred item from the prior bridge): the decay/**mute** ladder now
  survives a restart. Root cause was deeper than documented — `MuteState` wasn't persisted at all — so
  it needed **migration `0002_pattern_mute_persist.sql`** (`patterns.muted_until` + `recent_dismissals`),
  `Token::decode` + `ngram::parse_signature`, and `PatternEngine::hydrate` (seed the cache by signature
  at boot). Flush persists the mute columns; hydrate reloads them.
- **Hardware bodies — best-effort/UNVERIFIED** (compile against the real crate APIs; not run without a
  mic/GPU): cpal WASAPI capture + WAV/resample (the pure parts ARE tested), `global-hotkey` PTT (chord
  parse tested), Silero VAD → shipped an **energy-gate** trim instead (tested; Silero is the on-hardware
  upgrade), the `stt-host` whisper.cpp child (`is_wav`/confidence-parse tested).
- **Gate:** `crates/gates/tests/m6_l2_swap.rs` (L1 co-residency + L2-swap admission); **SC4 `#[ignore]`.**

### M7 — Reasoning gateway + transparency gate (Docs 09, 13)
- **`crates/reasoning-gateway`**: `payload_builder` (gather → redact-before-preview → cap `event_trail`
  → truncate-oldest-first → `cloud_send` hash); `Gateway::send_with_preview` = the single egress
  chokepoint (re-checks `user_approved`; picks first-healthy-**push** transport; transmits; audits AFTER
  success over the transport's REAL wire bytes; re-validates suggestions per-connector); `preview`
  (sole setter of `user_approved`); `suggestion_validator` (keep connector-validated, degrade rest to text).
- **Transports:** `api` (HTTPS Messages, best-effort), `cli` (`claude -p … --output-format json`,
  best-effort), `mcp` (**pull-only**: `supports_push()==false`; config-register + submit done; stdio
  JSON-RPC server + `aperture_get_context` gate deferred).
- **Privacy pulled forward from M9:** `redaction::Redactor` (ordered rules + Luhn + JSON-walk incl.
  numeric values) and `audit_log::sha256_hex`. Rest of privacy stays M9.
- **SC5:** the CPU-checkable half is proven in the gateway crate (preview==wire by hash; zero egress
  until approved; audit hashes the transport's actual bytes; egress primitives self-guard). The
  ETW/mitmproxy byte-monitor half stays `#[ignore]` (`crates/gates/tests/sc5_network_monitor.rs`).

### M8 — Design-system hardening (Docs 14, 11)
- **Glass-surface budget** (`ui/src/state/glassBudget.ts`): ≤2 glass from `ui.max_glass_surfaces`
  (default 2), opaque 3rd. Degrade-under-load: `gpuBusy.ts` body-class → CSS swaps glass→opaque.
- **Multi-monitor** (`src-tauri/src/overlay.rs`): `plan_overlays` (pure, tested) + `create_overlays`
  (best-effort per-monitor + `harden`); DPI re-anchor is on-hardware.
- **Gate:** PresentMon frame-drop + the final glass cap are `#[ignore]`/on-hardware.

### Multi-agent review + fixes (the second half of the session)
A 5-role review (senior architect / SWE / security / QA / UX) each swept all 32 changed files, then an
adversarial synthesizer verified each finding against the code (rejecting speculative/by-design ones).
**21 confirmed findings — all fixed + re-verified.** Highlights:

| Sev | Fix |
|---|---|
| HIGH | **L2 swap was orphaned** — `l2_swap_to_stt` had no non-test caller, so L2 voice hit `BudgetRefused`. Wired it into `admit_and_run` + added a scheduler-level test. |
| HIGH | **Bubble overflow menu clipped** by `contain:strict` — the 3 actions were unreachable. Portalled it to `<body>` as opaque chrome (also fixes it counting toward the glass cap). |
| MED | **Audit hash ≠ wire bytes** — the gateway hashed `serde_json(payload)`, not what transports send. Added `ReasoningTransport::wire_bytes` (default = payload; api/cli override with their real body); gateway hashes that. |
| MED | **Push/pull selection** — a Ready MCP (pull) dead-ended push Sends. Added `supports_push()` (MCP=false); the push picker skips it. |
| MED | **Audit before send** → phantom egress on failure. Moved the `cloud_send` record to AFTER a successful send; it now records the transport that actually egressed. |
| MED | **Egress primitives unguarded** — `CliTransport::send`/`ApiTransport::send` now self-guard on `user_approved`. |
| MED | **KNN recency recall-cliff** — the floor was applied after vec-truncation. `db::knn` now oversamples (k×8, capped) before the floor+LIMIT. |
| MED | **UI a11y dead-ends** — the voice confirm chip (Run/Dismiss unwired) and the preview modal (no focus/trap/Escape) now implement their contracts. |
| MED | **Test gaps** — added the resumable/stale-connector retrieval path + the STT-failure (store-nothing) branch. |
| LOW ×10 | contracts "CI-lint enforced" comment softened; CLI transport now carries `SYSTEM_FRAMING`; MCP `register` refuses to clobber an unparseable config; numeric-JSON redaction; IBAN/phone/EventTrail redaction tests; intent escalation word-boundary; reduced-motion `!important`; overflow menu opaque. |

One review finding was **rejected** (verified as by-design, not a bug): the voice/gateway subsystems
aren't constructed in the shell yet — that's expected milestone staging (see carry-forward).

---

## Decisions + flags (fold into docs as the on-hardware numbers land)

- **Redaction + `sha256_hex` pulled M9 → M7** — the gateway needs redaction-before-preview; the *rest*
  of privacy (consent, DPAPI keys, audit-row DB persistence, exclusion, Purge All) stays M9. (doc 13 amended.)
- **VAD = energy-gate, not Silero** in M6 — deterministic + tested; Silero (ONNX) is the on-hardware
  upgrade behind the `frame_is_speech` seam. (doc 07 amended.)
- **Cross-crate API changes** (additive, compatibility-law-safe):
  - `contracts::ReasoningTransport` gained defaulted `supports_push()` + `wire_bytes()`.
  - `db::KnnHit` gained `connector_id` + `stale_after_ts`; `db::knn` oversamples before the recency floor.
  - Migration `0002` (patterns mute columns). `record_cloud_send` takes the actual transport target.
- **UNVERIFIED (best-effort) bodies** — validate all on the RTX 5060 + a mic + a live Claude:
  cpal capture, `global-hotkey`, `stt-host` whisper child, `api`/`cli` cloud egress, `create_overlays`
  multi-monitor. They compile; they are not exercised.

---

## Carry-forward / what's left for v1

**1. Composition-root wiring (`src-tauri`) — the biggest remaining integration.** `VoiceSubsystem` and
`Gateway` are built + tested but **not constructed in the shell**. Needed: capture-toggle→voice
`enable/disable`; the PTT hotkey loop on a dedicated OS thread (both are `!Send`); warm-keep→`set_warm_kept`;
the gateway↔`ContextPreviewPanel`↔MCP-server wiring; the `voice_run_transcript` command for the confirm
chip's Run. Inherently on-hardware/UI — do it with the hardware in the loop.

**2. M9 — Privacy hardening (Doc 13).** DB at-rest encryption (SQLCipher + DPAPI-wrapped key, `key_manager`),
retention/Purge All (`db::purge_all`, `AuditLog` DB persistence — the `cloud_send` hash is already computed,
just not persisted), exclusion defaults + `exclusion_manager`, first-run consent sequence, and the scoped
CI lint for the two-emitter rule (remote-egress APIs denied outside the gateway, loopback allow-listed).
**Gate:** DB unreadable without the wrapped key; Purge All verified; excluded apps never captured
(frame-level); the audit answers "what left the machine."

**3. On-hardware gates (RTX 5060 + mic + live Claude).** SC4 (STT < 2 s), SC3 + measured co-resident VRAM
(M5), PresentMon overlay frame-drop + final glass cap (M8), the SC5 ETW/mitmproxy byte-monitor, and
validating every UNVERIFIED body. Gate results overwrite the `[VERIFY]` figures in Docs 01/04/16.

**4. CONN-M1 (coalesce monotonicity)** — a later position-less navigation can clobber a known media
position ("resume from start"). `pipeline.rs` `TODO(CONN-M1)`.

**5. Deferred M7 pieces** — the MCP stdio JSON-RPC server + `aperture_get_context` payload-store gate, and
the gated `aperture_search_history` tool (ADR-037) whose **UX is still an open question** (decide with Rajeev).

**Seed-table note (unchanged):** under the doc 04 §2 seeds, 7B (L2) projects ≥ 7.03 GB and is inadmissible
until remeasured — L1 (3B) is the only admitted VLM loadout pre-hardware-gate (staged rec #2).

---

## How this user works (apply from your FIRST response)

Rajeev is **AuDHD (autistic + ADHD) — an accessibility requirement, not a preference.**
- **Answer first.** Conclusion in the first line; reasoning after.
- **Short chunks, bullets over paragraphs.** Bold the one load-bearing line. No walls of text.
- **One question at a time, max.** No filler, no throat-clearing, no apology spirals.
- **Recommendation, then alternatives** — not a flat menu. End with a concrete next step.
- Competent peer (Rust, Python, agentic AI, MCP, RAG, llama.cpp) — don't over-explain; push back directly
  when he's wrong. Swearing / dry humour fine. Follow tangents, then offer to refocus.
- **Workflows:** when creating dynamic workflows, be stingy with tokens. Ultracode/xhigh is the session default.

---

## Practical notes

- **Windows linker flakiness:** parallel `cargo test` relinks intermittently throw `LNK1104: cannot open
  file …exe` — Windows Defender holding the freshly-linked binary, NOT a code error. Retry or run per-crate.
  A cross-crate contract change (e.g. this session's `ReasoningTransport` additions) forces a wide rebuild —
  budget time for it.
- **UI verification:** no test runner is wired; `tsc --noEmit -p ui/tsconfig.json` (run from `ui/`) is the
  TS check. Pure UI logic (`glassBudget`) is written to be trivially correct + typechecked.
- **Test the pure parts:** the pattern this session followed for hardware/egress code — factor the pure
  logic (WAV/resample, energy-VAD, chord parse, prompt render, redaction, `plan_overlays`, the payload
  pipeline) out of the I/O and test it; leave only the true I/O UNVERIFIED.
- Authoritative design = Docs 00–21 (R2). Each affected doc (03/07/09/11/12/13/14/16) now carries a dated
  "Implementation status (2026-07-08)" note pointing here.
