# Doc 18 — Coherence & Connection Review (R1)

> **⚠️ Superseded for the R2 set by [Doc 21](21-coherence-review-r2.md).** This is
> the **R1** coherence review, run against Docs 00–17 before the R2 refinement pass.
> Doc 21 re-runs coherence over the amended set. Where they differ, **Doc 21 wins.**
> Specifically R2 changed: **C4** (≤2 glass + opaque 3rd, final cap at M8),
> **§4 budget** (re-checked with the faster-whisper ~2 GB figure and the 7.0 GB cap),
> and **§3 FIX 2** (L1 co-residency refined to *conditional* co-residency, ADR-030).
> Those three items are annotated inline below; the rest stands as historical record.

Run against the split document set (00–17). Verdicts: **PASS** or **FIX (applied)**.

## 1. Requirement coverage — PASS
| Goal | Components | Story | Flow docs |
|---|---|---|---|
| G1 | 05, 06, 08, 11 | US1 | 02 §4 |
| G2 | 10, 11 | US1/US5 | 02 §5 |
| G3 | 10 (trait + registry) | — | 10 §1 |
| G4 | 08 (trigger rule) | US1/US5 | 08 §6 |
| G5 | 07, 11 | US2 | 02 §6-C |
| G6 | 08 (no-cloud), 09, 12 | US3 | 09 §1 |
| G7 | 03 §4, 09, 11 §4, 13 §3 | US3 | 13 §3 |
| G8 | 05 §5, 12 §6 | US4 | 05/12 |
| G9 | 04, 12 | — | 04 |
| G10 | 14 | — | 14 |
No orphan goals; no orphan components (every component doc is reachable from ≥ 1 goal).

## 2. Data-flow integrity — PASS
- **Path A** (capture→OCR→store→pattern→bubble): every hop has a named source/sink and a latency line item summing under SC2 (02 §4). No GPU/network on the path; the optional VLM explicitly cannot gate a bubble (02 §4 invariant ↔ 06 §4 ↔ 08 §8 — consistent in all three).
- **Path B** (click→connector_state→reconstruct→ShellExecute→outcome): closed loop incl. failure feedback into `suggestions.outcome` (02 §5, 10 §6).
- **Voice** (hotkey→STT→telemetry **and** retrieval→bubble): both roles land in defined sinks (07 §3–5, 03 §5).
- **Cloud** (trigger→payload→redaction→preview→gateway→structured output→bubble): single gated chain; the MCP pull variant places the same gate inside the tool handler (09 §3, 13 §3).

## 3. Interface consistency — PASS (2 fixes applied during the split)
- Local candidates and cloud results flatten to the same `StructuredSuggestions` shape before the UI (09 §4 ↔ 15 §5) — source-agnostic confirmed.
- Connector `capture` writes what `reconstruct` reads; cloud-suggested payloads must pass `validate()` to gain a button (10 §1 ↔ 09 §4 ↔ 15 §3).
- **FIX 1 (carried):** embedding dimension pinned to **768** (nomic-embed-text-v1.5) in both the `ctx_vec` DDL (03) and the RAM budget (04) — no schema/model drift possible.
- **FIX 2 (new in the split):** the L1 loadout means VLM+Whisper may be **co-resident** while the mutex still serializes **execution**; Docs 04 §4, 07 §3, 12 §3 now state the same residency-vs-execution distinction, and L2 is uniformly described as residency-exclusive with swap. **[R2 refinement — ADR-030/Doc 21 §7.2]:** with faster-whisper (~2 GB) and the 7.0 GB cap, co-residency is now **conditional** — the BudgetEnforcer counts co-resident weights and makes STT the swap victim under image-VLM pressure; in practice L1 behaves mostly as fast-swapping single-heavyweight (Doc 04 FIX 4.1).

## 4. Budget check — PASS
L1: 3B (~5–6 GB) + Whisper small (~1 GB) + framework (~1 GB) ≈ 7–8 GB worst case, ~6.5 GB typical, under the ceiling with the 7.2 GB projection enforcing admission [VERIFY at M5]. L2: exclusive by rule everywhere it appears. RAM: shell 30–50 MB + core 150–300 MB + OCR + embedder ~520 MB ⇒ SC1 < 1.5 GB plausible [VERIFY at M0/M5]. The mutex rule reads identically in 04 §4, 06 §3, 07 §3, 09 (n/a — no GPU), 12 §3, 14 §5.

> **[R2 re-check — see Doc 21 §4]:** re-run with **faster-whisper ~2 GB** (ADR-024) and the **7.0 GB cap** (ADR-030), a co-resident STT + image-VLM would breach, so the enforcer **unloads STT** before admitting the image job. Co-residency is opportunistic (FIX 4.1); admission never exceeds 7.0 GB by construction. RAM is *tighter* with the Python faster-whisper host (~300–600 MB) but plausible, with the nomic→MiniLM fallback as the release valve.

## 5. Constraint honor check — PASS
- **Capture toggle:** mechanism (05 §5), owner (12 §6), kill-switch role (13 §4), gate (16 M1) — one consistent story incl. the 3 s SLA and sidecar kill.
- **Local-DB-never-leaves:** single file (03 §1), two-emitter rule + CI lint (13 §2), SC5 in CI (16 M3/M7).
- **Context transparency:** one serialized object across 03 §4, 09 §3, 11 §4, 13 §3; preview==wire is a data-flow property, hash-audited.
- **Privacy boundary:** exclusion at the earliest gate (05 §4), raw frames never persisted (05 §2/13 §4), redaction before preview (13 §5).

## 6. Contradiction sweep — RESOLVED
- **C1 (VLM size):** 3B default / 7B opt-in stated identically in 04/06/12. Resolved.
- **C2 (WGC border vs always-on):** accepted + tracked as RK5 with an investigation path; indicator remains the truth surface. Resolved.
- **C3 (multimodal voice vs local-only proactivity):** voice is local telemetry + local retrieval; cloud only via explicit escalation through the same gate (07 §5). Resolved.
- **C4 (new, found in the split):** Doc 11 said "max 3 visible bubbles" while Doc 14 said "≤ 3 glass surfaces" — these were reconciled as the same cap. **[R2 — ADR-039/Doc 21 §6]:** C4 **re-opened and re-resolved** — refraction is deferred post-v1 and the caps are now **decoupled**: **≤2 glass surfaces + an opaque 3rd bubble** (3 visible total), with the final cap set at the M8 PresentMon test.

## Final statement
The split set is internally consistent. The binding consequence of the hardware — *an 8 GB card cannot hold a 7B VLM and Whisper simultaneously* — is carried as a first-class force (L1 default, mutex, projection cap, degrade ladder, degrade-under-load UI). The three invariants (VRAM ceiling, transparency gate, capture toggle) each appear in mechanism, ownership, enforcement, and gate form across at least four documents, and every unmeasured number is tagged [VERIFY] with a named milestone that will replace it with a measurement.
