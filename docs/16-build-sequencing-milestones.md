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
