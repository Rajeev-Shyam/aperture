# Doc 16 — Build Sequencing & Milestone Plan

Every milestone has a **validation gate**: a measured/proven condition on the real target (RTX 5060 / 16 GB / Ryzen) that must pass before the next stage starts. Gate results overwrite the corresponding [VERIFY] figures in Docs 01/04.

| M | Scope | Depends | Gate (measured) |
|---|---|---|---|
| **M0** | Contracts crate (Doc 15), SQLite schema + migrations (Doc 03), Tauri shell skeleton, CI with the SC5 network monitor harness | — | Shell idle RAM within the Doc 04 line item; schema round-trips all event types; fakes compile against every contract |
| **M1** | Capture & Event subsystem + the toggle (Doc 05) | M0 | **SC6:** OFF releases WGC/UIA and (later) sidecars, VRAM delta to ~0 in < 3 s; idle CPU < 2 %; exclusion list provably yields metadata-only events |
| **M2** | Cheap OCR + embeddings + store (Docs 06-A, 03) | M1 | OCR ≤ 400 ms/frame at target res; embedding ≤ 300 ms; KNN returns sane neighbors on seeded data |
| **M3** | Pattern engine + suggestion generator + minimal overlay (Docs 08, 11 partial) | M2 | **SC2:** scripted recurring workflow → correct bubble < 2 s; caps/cooldowns/decay behave per Doc 08 on the scripted stream; **SC5 holds** (zero egress) |
| **M4** | The four connectors + Critical Path B (Doc 10) + **the browser extension + native-messaging host** (ADR-027); **YouTube connector built first** (Q75) | M3 | **US1 end-to-end:** YouTube reopens at the right timestamp via the **extension content-script `currentTime`**, and the "from the start" degrade is honest; document/IDE/browser resume each pass on 3 real apps; **extension installs, native messaging works (loopback whitelisted in SC5), exclusions honored through it** |
| **M5** | vlm-host sidecar + GpuScheduler + BudgetEnforcer (Docs 06-B, 12) | M3 | Projection table replaced by **measured** VRAM numbers **including co-resident weights** (ADR-030); wake gate holds within its **adaptive ~3–10/h** band with the hard ceiling protecting voice; **no admission ever exceeds 7.0 GB** projected; SC3 load times met |
| **M6** | stt-host + PTT + intent + retrieval (Doc 07) | M5 | **SC4** (< 2 s for 5–10 s utterance, GPU path); **US2 end-to-end** incl. low-confidence confirm path; L1 **conditional** co-residency and L2 swap both proven (STT swap-victim under image-VLM pressure) |
| **M7** | Reasoning Gateway + transparency gate, **MCP (pre-declared primary) and CLI transports both implemented** (Docs 09, 13); the **gated `aperture_search_history` UX** decided here (ADR-037) | M3 | **SC5 strict:** zero **user-data** bytes until Send/scoped-allow, preview bytes == wire bytes (hash compare); **updater traffic distinguished, loopback whitelisted** (ADR-036/028); transport fallback works with each transport disabled in turn; **US3 end-to-end** |
| **M8** | Design-system hardening: glass tokens, degrade-under-load wiring, multi-monitor (Docs 14, 11) | M5–M7 | **≤ 2 glass surfaces + opaque 3rd bubble** enforced (final cap set here via **PresentMon**, ADR-039); no overlay frame drops during a VLM job; glass↔fallback swap clean |
| **M9** | Privacy hardening: encryption, retention/purge, exclusion defaults, audit, first-run consent (Doc 13) | M7 | DB unreadable without the wrapped key; Purge All verified; excluded apps never captured (frame-level test); audit answers "what left the machine" |

## Implementation status (as of 2026-07-08)

**M0–M4 landed** (code + gates): contracts/schema/shell, capture + toggle, OCR/embed/store, pattern engine + overlay, the four connectors + the browser extension + native-messaging bridge. Gates live under `crates/gates/tests/` (`m0_*`, `m4_us1_resume`, plus the permanent `sc5_*`/`sc6_*`).

**M5 landed in software.** The orchestration resource manager (`GpuScheduler` single-mutex/priority/preempt/deadline, `BudgetEnforcer` R1+R3 ladder, `ModelLifecycle` spawn/kill/health/idle-sweep, `TierRouter` wake gate, `Telemetry`) + the on-demand VLM enrichment path. `m5_budget_ceiling` + `m5_wake_band` pass in CI; measured-VRAM + SC3 are `#[ignore]` in `m5_load_times` pending the RTX 5060.

**M6 landed in software (2026-07-08).** Voice/PTT/STT (doc 07): the per-utterance pipeline (`VoiceSubsystem::process_utterance` — VAD trim → priority-100 STT `GpuJob` → **unconditional** `voice_utterance` store+embed → deterministic intent → retrieval/escalation/telemetry branch, all CPU-tested against `FakeScheduler` + in-memory DB), intent classification, and local KNN+temporal-window retrieval. The three deferred lifecycle pieces are wired: **`ModelLifecycle::l2_swap_to_stt`** (+ the scheduler now *invokes* it — an L2 STT arrival evicts the resident 7B instead of refusing, doc 12 §3), the **crash-restart ladder** (`gpu_scheduler::acquire_endpoint` routes cold-load failure through `handle_crash`), and the **warm-keep tracker** (`orchestration::warm_keep::PttWarmKeep`, ≥2 PTT/5 min). Gate: `m6_l2_swap` (L1 co-residency + L2-swap admission); **SC4 latency is `#[ignore]`** (on-hardware). Hardware bodies — cpal WASAPI capture, `global-hotkey`, Silero (see amendment below), the `stt-host` whisper child — are **best-effort/UNVERIFIED** (compile against the real crate APIs; not exercised without a mic/GPU).

**M7 landed in software (2026-07-08).** Reasoning gateway + transparency gate (docs 09/13): `payload_builder` (gather → **redact-before-preview** → cap `event_trail` → truncate-oldest-first → `cloud_send` hash), the `Gateway` egress chokepoint (`send_with_preview` re-checks `user_approved`, picks the first healthy **push** transport, audits **after** a successful send over the transport's **real wire bytes**, re-validates suggestions per-connector), the preview consent gate, and all three transports (`api`/`cli` best-effort real egress, `mcp` pull-only: config-register + submit). SC5 CPU-half is proven in the gateway crate (preview==wire by hash; zero egress until approved; audit hashes the transport's actual bytes); the byte-monitor half stays `#[ignore]` (ETW/mitmproxy). **Deferred:** the gated `aperture_search_history` MCP tool (ADR-037 — the gated-search **UX is still an open question** and it rides on the MCP stdio server), and the MCP stdio JSON-RPC server itself.

**M8 landed in software (2026-07-08).** Design-system hardening (docs 14/11): the **glass-surface budget** (`ui/src/state/glassBudget.ts`, ≤2 glass from `ui.max_glass_surfaces`, opaque 3rd), degrade-under-load (the `gpu_busy` body-class CSS swap), and **multi-monitor overlays** (`overlay::plan_overlays` pure+tested; `create_overlays` best-effort per-monitor + `harden`). Gate: **PresentMon frame-drop + the final glass cap are `#[ignore]`** (on-hardware). The overflow menu is portalled out of the `contain:strict` bubble (was clipped/unreachable); the confirm chip + preview modal got real focus/Escape a11y.

**Multi-agent review (2026-07-08).** A 5-role review (architect / SWE / security / QA / UX) + adversarial verify produced **21 confirmed findings, all fixed** — 2 high (the orphaned L2-swap scheduler wiring; the overflow-menu clipping), 9 medium (audit-hash-≠-wire-bytes, push/pull transport selection, audit-before-send ordering, unguarded egress primitives, KNN recency recall-cliff, two UI-a11y dead-ends, two test-coverage gaps), 10 low. Full detail + rationale in `docs/handoff/`.

**Milestone-boundary amendments (this session):**
- **Redaction pulled forward M9 → M7.** The gateway structurally needs redaction-before-preview (doc 09 §5, doc 13 §5), so `aperture_privacy::redaction::Redactor` (+ `audit_log::sha256_hex`) is implemented + tested now. The *rest* of privacy (consent, DPAPI key manager, audit-row DB persistence, exclusion manager) remains M9.
- **VAD backend.** M6 ships a deterministic energy-gate trim (tested); Silero VAD (ONNX, doc 07 §2) is the on-hardware quality upgrade behind the same per-frame seam.

**Carry-forward (deferred; each has an in-code `TODO`):**
- **Composition-root wiring (src-tauri).** `VoiceSubsystem` and `Gateway` are built + tested but **not yet constructed in the shell** — the capture-toggle→voice enable/disable, the PTT hotkey thread, warm-keep→`set_warm_kept`, and the gateway↔preview↔MCP-server wiring. Inherently on-hardware/UI integration.
- **CONN-M1 (coalesce monotonicity)** — a later position-less navigation can clobber a known media position ("resume from start"). `pipeline.rs` `TODO(CONN-M1)`.
- **On-hardware gates** — SC4 (STT latency), SC3/measured-VRAM (M5), PresentMon (M8), the SC5 byte-monitor, and validating every UNVERIFIED body (mic/whisper/hotkey/cloud transports/multi-monitor) on the RTX 5060.
- **Seed-table note:** under the conservative doc 04 §2 seeds, 7B (L2) projects 7.03 GB at minimum and is inadmissible until remeasured — L1 (3B) is the only admitted VLM loadout pre-hardware-gate (staged rec #2).
- **RESOLVED this session:** CONN-M2 (decay/mute ladder now survives restart — migration `0002` + `PatternEngine::hydrate`); the L2 swap, crash-ladder, and warm-keep deferrals above.
## Implementation status (as of 2026-07-06)

**M0–M4 landed** (code + gates): contracts/schema/shell, capture + toggle, OCR/embed/store, pattern engine + overlay, the four connectors + the browser extension + native-messaging bridge. Gates live under `crates/gates/tests/` (`m0_*`, `m4_us1_resume`, plus the permanent `sc5_*`/`sc6_*`).

**M5 landed in software.** The orchestration resource manager (`GpuScheduler` single-mutex/priority/preempt/deadline, `BudgetEnforcer` R1+R3 ladder, `ModelLifecycle` spawn/kill/health/idle-sweep, `TierRouter` wake gate, `Telemetry`) and the on-demand VLM enrichment path (`VlmLayer` → `vlm-host` → `screen_context.vlm_summary`, wired off the bubble path in the shell). The two **software-checkable** M5 exit criteria pass in CI: `m5_budget_ceiling` (no admission ever projects > 7.0 GB; STT is the swap victim, ADR-030) and `m5_wake_band` (the ~3–10/h hard ceiling protects voice, ADR-032). The **on-target** criteria — measured co-resident VRAM replacing the seed table, and SC3 cold-load SLAs — are `#[ignore]`-gated in `m5_load_times` pending the RTX 5060.

**M5→M6 carry-forward** (deferred, tracked in `docs/handoff/` bridge; each has an in-code `TODO`):
- **L2 STT swap + 20 s min-residency** — `ModelLifecycle::l2_swap_to_stt` is a `todo!("M6")`; `loaded_at_ms` is stamped ready for it.
- **Crash-restart ladder** — `handle_crash` (backoff + 3-strike + Degraded fallback) is implemented + tested but not yet wired into the production runner (a load failure currently maps flat to `SidecarDown`; safe soft-degrade, not resilient).
- **Decay ladder does not survive restart** (CONN-M2) — the pattern engine re-mines cold on boot and the flush clobbers persisted `dismiss_decay`; needs engine hydration by `signature`.
- **Connector coalesce monotonicity** (CONN-M1) — a later position-less navigation can clobber a known media position ("resume from start").
- **Seed-table note:** under the conservative doc 04 §2 seeds, 7B (L2) projects 7.03 GB at minimum and is therefore inadmissible until remeasured on hardware — L1 (3B) is the only admitted VLM loadout pre-M5-hardware-gate (consistent with staged rec #2).

## Gate-failure protocol
A failed gate stops forward progress on that path; the fix lands, the gate re-runs, and the affected doc is amended (the docs are living: measured numbers replace estimates).

## Staged recommendations (carried from the architecture pass)
1. **M0→M3 before any GPU work.** The product's primary value (G1/G4) is entirely Tier 0; if SC2/SC7 can't be met locally, the GPU and cloud tiers are premature. *Pivot threshold:* if heuristic precision can't reach SC7, pull M5 ahead of M4 to enrich the signal with the VLM.
2. **Commit to the L1 (3B-default) loadout; 7B stays a feature flag.** *Pivot threshold:* only if measured 3B scene-quality is insufficient for the pattern engine **and** SC3 still holds under L2 swapping.
3. **SC5 and SC6 are permanent CI tests from M1/M7 on** — the two trust foundations (zero silent egress; the toggle truly releases) are regression-protected forever.
4. **RK3 (video position) and RK4 (URL via UIA) are resolved by *building the extension*, not spiking** (ADR-027): the extension is a committed v1 component at M4 (content-script `currentTime` + tabs API URL are reliable), with UIA demoted to the no-extension fallback. Residual risk is extension lifecycle (RK13) and broad host access (RK14).
5. **Implement both the MCP and CLI transports at M7; MCP is the pre-declared primary** (ADR-025) — the CLI's documented headless caveats put it on the fallback path, but the abstraction stays honest only if both exist. The SC5 harness distinguishes updater traffic and whitelists loopback.

**Milestone→SC map:** SC1 (M0/M5), SC2 (M3), SC3 (M5), SC4 (M6), SC5 (M1→ every gate, strict at M7), SC6 (M1), SC7 (M3 dogfood, ongoing).

> Note: once the extension ships at M4, its native-messaging forwarding falls under the capture toggle (FIX 2.1) — the M4 gate and every later SC6 run assert OFF halts it within 3 s.

---
> **R2 amendments applied** (see docs/19–21): ADR-027/Q74/Q75 (extension + native-messaging host at M4, YouTube first, RK3/RK4 built-not-spiked), ADR-030 (M5 measured co-resident weights, 7.0 GB), ADR-032 (M5 adaptive wake band), ADR-025 (M7 MCP pre-declared primary), ADR-037 (M7 gated-search UX), ADR-036/028 (SC5 harness: updater distinguished, loopback whitelisted), ADR-039 (M8 ≤2 glass + opaque 3rd, PresentMon final cap). Staged rec #2 (3B default / 7B flag) confirmed (Q39).
