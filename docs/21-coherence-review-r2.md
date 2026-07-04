# Doc 21 — Coherence & Connection Review (Pass R2)

Run against the **amended** set (Docs 00–18 + the R2 ADRs in Doc 19 + the deltas in Doc 20). Supersedes Doc 18 for the R2 design. Verdicts: **PASS**, or **FIX (applied)** with the fix stated. The R2 pass changed ~50 things, several touching named invariants — those get individual verification in §7.

---

## 1. Requirement coverage — PASS
Every goal still maps to components; R2 added surfaces but removed no coverage.
- **G2/G3 (bubbles-as-actions, connectors)** strengthened: the browser extension (ADR-027) makes US1's video-position resume reliable rather than degrade-only.
- **G7 (context transparency)** survives the scoped-allow relaxation (ADR-026) — see §7.1.
- **G5 (voice telemetry + query)** unchanged; the embedding-head intent model (ADR-034) is additive.
- **G6 (local proactivity / explicit-only Claude)** unchanged — the proactive loop still never calls Claude.
No orphan goals; the extension is reachable from G2/G3; diagnostics + Activity-&-Privacy from G7/Doc 13.

## 2. Data-flow integrity — PASS, 2 FIXES
- **Path A (proactive):** unchanged and still GPU/network-free; the new **pHash near-duplicate gate** (Q72) sits *before* OCR and only *removes* work — it cannot delay a bubble. The adaptive heartbeat/cap (ADR-032) modulate volume, not the per-event latency budget (SC2 intact).
- **Path B (resume):** now extension-backed for browser/YouTube; validation moved **on-click** (ADR-035). Closed loop preserved — failure still feeds `suggestions.outcome`.
- **FIX 2.1 — toggle must gate the extension.** The browser extension is a *new capture source*. The toggle-OFF sequence (Doc 12 §6 / Doc 05 §5) is amended to **signal the native-messaging host to stop forwarding extension data** on OFF, so capture-OFF truly halts *all* capture (not just WGC/UIA). Without this, the extension could feed URLs while Aperture shows "inactive" — an invariant breach. *Applied to Doc 12 §6 and Doc 05 §5.*
- **FIX 2.2 — extension data through the same gates.** Extension-sourced URLs must traverse the **same exclusion + redaction pipeline** as UIA-sourced ones, and respect `url_pattern` exclusions (ADR-040) and incognito. *Applied to Doc 13 §4 and Doc 05 §4.*
- **Cloud paths:** the gated `aperture_search_history` (ADR-037) and the opt-in diagnostics (ADR-036) are both new sinks-that-can-emit; both land in defined, gated/audited flows (§7.3).

## 3. Interface consistency — PASS, 1 FIX
- **Source-agnostic suggestions** (local == cloud `StructuredSuggestions`) unchanged; the **"useful?" thumbs** (Q81) is a new feedback input, flattened into the same telemetry as click/dismiss.
- **Contract 3 reworded** (validate-on-click) is consistent across Doc 09 §4, Doc 10 §1/§6, Doc 15 §3 — and now *matches* the local connector show-then-fail model rather than diverging from it.
- **FIX 3.1 — four-tier priorities propagated.** ADR-031's `enrichment-VLM (70)` tier must appear identically in Doc 12 §3 and the `should_wake_vlm`/job-spec references in Doc 06 §4/§5. *Applied (Doc 20: Doc 06, Doc 12).*
- **Transport order** (MCP→CLI→API) reads identically in Doc 09 §2/§3 and Doc 15 §5.

## 4. Budget check — PASS, with an honest re-statement (FIX 4.1)
Re-run with the **faster-whisper ~2 GB** figure (ADR-024) and the **7.0 GB cap** (ADR-030):

- **3B VLM working set, image job:** weights+mmproj 1.93+1.34 = 3.27 · image activation ~1.2 (or **~0.7 at 768 px**, ADR-032) · KV @4K ~1.0 · framework ~0.75–1.0 → **~6.2–6.5 GB.** Fits ≤7.0 alone. ✓
- **Add a co-resident faster-whisper (~2 GB):** ~8.2–8.7 GB → **breaches.** Therefore the BudgetEnforcer **unloads STT** before admitting the image-VLM job (ADR-030). ✓ admission stays ≤7.0.
- **Co-resident *text-only* VLM + STT:** 3.27 + framework ~0.75 + KV ~1.0 + STT 2.0 ≈ **~7.0 GB — at the cap.** Often inadmissible once any image or larger KV is involved.

**FIX 4.1 (honesty).** The arithmetic shows **true co-residency during VLM execution is the exception, not the rule**, under faster-whisper + a 7.0 GB cap. "L1 co-resident when memory allows" is *correct* (the enforcer guarantees ≤7.0 and swaps STT out under pressure), but in practice L1 behaves mostly as **fast-swapping single-heavyweight**: STT resident while the VLM is idle/unloaded; STT the swap victim during VLM image jobs; warm-keep (≥2 PTT/5 min) holds STT during voice-heavy spells at the cost of forcing VLM unloads. *Doc 04 §3 and Doc 21 carry this framing so "co-resident" is not over-read.* The adaptive 768 px downscale (ADR-032) and 60 s idle-unload (Q32) widen the windows where co-residency *is* admissible. **No admission ever exceeds 7.0 GB — the ceiling holds by construction** (measured at M5).
- **RAM:** shell 30–50 MB + core 150–300 + OCR + nomic ~520 + the **Python faster-whisper host** (~300–600 MB when loaded) ⇒ SC1 <1.5 GB is **tighter** but plausible; the nomic-vs-MiniLM fallback (ADR-005, affirmed Q1) is the release valve. `[VERIFY M2/M5]`.

## 5. Constraint-honor check — PASS
- **Capture toggle:** still kills both sidecars (incl. the Python STT process — kill reclaims VRAM) within 3 s, **plus** now halts extension forwarding (FIX 2.1). One consistent story across Doc 05 §5, Doc 12 §6, Doc 13, M1.
- **Local-DB-never-leaves:** intact. The new egress paths are bounded: raw user data only via the gate; diagnostics opt-in/aggregate/audited via the same crate; updater carries no user data. Loopback IPC is on-device, 127.0.0.1-bound + authenticated.
- **Context transparency:** preserved under both the scoped-allow (payload still displayed + cancel + audit) and the gated search (results previewed before return). See §7.

## 6. Contradiction sweep — RESOLVED
- **C1–C3 (from Doc 18):** still resolved.
- **C4 (bubble cap vs glass cap):** **re-opened by ADR-039** (≤2 glass) and **re-resolved**: the caps are **decoupled** — ≤2 glass surfaces + an **opaque 3rd bubble** (3 visible total), final cap at the M8 PresentMon test. Stated identically in Doc 11 §3 and Doc 14 §5.
- **C5 (new) — STT figure vs L1 co-residency:** the docs once assumed ~1 GB Whisper enabling easy co-residency; ADR-024 (faster-whisper ~2 GB) contradicted that. **Resolved** by ADR-030's co-resident-accounting + conditional co-residency + FIX 4.1's honest framing — consistent in Docs 04, 07, 12, 21.
- **C6 (new) — "two emitters" vs updater/diagnostics:** the literal claim contradicted the existence of an updater and an opt-in diagnostics path. **Resolved** by ADR-036's precision (raw-user-data-via-gateway-only; diagnostics-via-gateway; updater-carved-out) — consistent in 00, Doc 13 §2, Doc 01 SC5, Doc 16.
- **C7 (new) — "data minimization" vs empty defaults + broad extension:** an overclaim. **Resolved** by ADR-029's reframe to "minimal *defaults* + user-driven minimization + transparent disclosure."

## 7. Invariant verification (the high-stakes R2 changes)
### 7.1 Transparency gate — PASS (relaxed deliberately)
ADR-026 (scoped allow) keeps decision 9's substance: the **exact payload is still rendered**, a **cancel window precedes egress**, and the **SHA-256 is audit-logged** — only the manual *Send click* is automated. SC5 reworded to admit "explicit Send **or** active scoped allow (payload displayed + cancel + audit)." ADR-037 (gated search) likewise previews results before return. *Nothing reaches the cloud unseen.* The gate is narrower in friction, not in visibility.

### 7.2 8 GB VRAM ceiling — PASS (enforced, co-residency conditional)
ADR-030: projection counts co-resident weights, cap 7.0 GB, STT swap-victim under image-VLM pressure. §4 shows admission never exceeds 7.0 by construction; FIX 4.1 records that co-residency is opportunistic under VLM load. The ceiling holds; the "L1 = dual-resident" mental model is softened to "fast-swapping with opportunistic co-residency." Measured at M5.

### 7.3 Cloud boundary / emitter rule — PASS (made precise)
ADR-036: the gateway crate remains the **only** opener of app sockets (diagnostics routed through it; opt-in, aggregate-only, audited); the **updater** is a framework path with no user data, excluded from the SC5 *user-data* test but visible to the monitor; loopback IPC (ADR-028) is on-device and scoped. The strong claim ("silent exfiltration is architecturally impossible") **survives**; the weaker literal claim ("exactly two paths emit network") is corrected to the accurate statement.

### 7.4 Capture toggle — PASS (extended to the extension)
FIX 2.1 brings the browser extension under the toggle: OFF halts WGC/UIA, kills both sidecars (incl. Python STT), **and** stops native-messaging forwarding — all within the 3 s SLA. The indicator remains the truth surface.

## 8. New risks logged (Doc 17 §1)
- **RK13 — browser-extension lifecycle** (MV3 / store-review / Chrome+Opera GX / native-messaging registration): Med/Med; mitigated by one Chromium codebase + guided install; M4.
- **RK14 — broad extension host access**: Med/Med; broad permission, **narrow use** (URLs + position only), exclusions/incognito gating, install-time disclosure, `url_pattern` controls; residual exposure **accepted** (Q61), docs reworded honestly (ADR-029). M4/M9.
- **RK10 upgraded** (MCP pull-UX now the default cloud path); **RK3/RK4 downgraded** (extension resolves them).

## Final statement
The R2-amended set is **internally consistent**. The five fixes (toggle-gates-extension, extension-through-the-gates, four-tier propagation, budget honesty, C4 re-resolution) are applied in Doc 20. The three invariants survive the refinement, each *deliberately* reconciled rather than silently bent: the **transparency gate** is relaxed in friction but not in visibility (payload still shown, cancel window, audit); the **8 GB ceiling** is enforced by an enforcer that now counts co-resident weights and treats co-residency as opportunistic; the **cloud boundary** is restated precisely so it stops overclaiming while keeping silent exfiltration impossible. Two genuinely new forces entered in R2 — the **browser extension** (a new capture source and privacy surface, now toggle-gated and exclusion-gated) and **MCP-primary** (making the pull/handoff the default cloud UX) — and both are carried through mechanism, ownership, enforcement, and gate form across the affected documents. Every unmeasured R2 value remains `[VERIFY]`/`[ASSUMPTION]` with a named M-gate.

**Net posture shift to record honestly:** v1 now ships with **minimal *defaults* rather than minimal *collection*** (empty exclusions + broad extension reach, by the user's choice), compensated by transparent disclosure, detect-and-suggest onboarding, the audit view, and one-click domain/app exclusion — and the docs say so plainly rather than claiming more.
