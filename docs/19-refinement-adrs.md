# Doc 19 — Refinement ADRs (Pass R2)

These ADRs record the **load-bearing** decisions made during the R2 architecture-refinement pass (clickable-MCQ session, Q1–Q96). Minor parameter tweaks (e.g. dwell 12→20 s, blur 16→12 px, idle-unload 90→60 s) are *not* given ADRs — they live in Doc 20 (Amendments). Format follows Doc 06's ADR convention: **Decision · Status · Context/forces · Rationale · Consequences · Supersedes/relates.** Every `[VERIFY]` figure remains an estimate until its owning M-gate measures it.

> **Invariant note.** Three R2 decisions touch the project's named invariants and were reconciled deliberately, not silently: ADR-026 (transparency gate relaxed but preserved), ADR-030 (8 GB ceiling — L1 co-residency made conditional), ADR-036 (the "two-emitter rule" made precise). Each is called out below.

---

### ADR-024 — STT backend: faster-whisper (CTranslate2) on GPU; whisper.cpp small-model on CPU
**Status:** [DECIDED] R2 (Q5, Q54). **Supersedes/amends:** ADR-019 (STT VRAM/backend open question); amends ADR-003's "native-only sidecar" framing for `stt-host`.
**Context/forces:** §8.2 left the STT backend unresolved; the L1 co-residency math depends on it. The GPU path wants quality + features; the CPU fallback wants real-time factor on a Ryzen.
**Rationale:** Split by tier. GPU `stt-host` = **faster-whisper / CTranslate2 Python service** (richer, opt-in distil-large-v3 int8 path); CPU fallback = **whisper.cpp base (default) / tiny (if base is too slow)**, a cheap GGML bundle. Each engine suits its tier.
**Consequences:** (a) STT planning figure for L1 now carries the **1–2 GB range** (faster-whisper small ≈ 2 GB), not the single ~1 GB — this is what forces ADR-030. (b) The GPU sidecar runs a Python runtime targeting the **3.13 env** (CTranslate2 needs no PyTorch for inference, but pin 3.13 for wheel compatibility). (c) SC6 still holds — killing the Python process reclaims VRAM. (d) SC4 (<2 s) is **not** promised on the CPU path. (e) Two STT models ship.
**Rejected:** whisper.cpp small on GPU (the ~1 GB assumption that eased L1 but the user chose the richer GPU backend).

---

### ADR-025 — Claude transport: MCP-primary (Claude Desktop), CLI fallback, API third
**Status:** [DECIDED] R2 (Q8, Q10). **Refines:** ADR-010 (CLI/MCP redirect) — settles *which of the two leads*. **Relates:** RK7, RK10.
**Context/forces:** ADR-010 made CLI/MCP primary over the raw API but left CLI as the "primary candidate." Desktop-MCP ubiquity argues for MCP.
**Rationale:** Most target users have Claude Desktop installed; MCP is the lowest-friction reach. Default fallback order is now **MCP → CLI → API**.
**Consequences:** (a) The **pull/handoff** UX becomes the *default* cloud loop — MCP cannot be pushed a prompt. US3 is therefore: *"Ask Claude" stages the approved payload + shows a handoff; the transparency gate fires inside the `aperture_get_context` tool handler; the answer returns via `aperture_submit_suggestions` and renders in a bubble on the same schema* (Q10). (b) **RK10 (pull-UX friction) is elevated** from Low to a central concern. (c) The CLI large-stdin caveat (RK7) drops off the critical path — it's now the fallback (Q9 deferred its handling to M7). (d) Doc 09 §3's "primary candidate: CLI" note is reversed; Doc 16 M7 "declare primary" is pre-decided (MCP), though both transports are still built at M7.
**Rejected:** CLI-primary (push fits the UX, but most users won't have the CLI); API-primary (the user redirected away from a metered key — ADR-010).

---

### ADR-026 — Scoped "always-allow" with the transparency gate preserved  ⚠️ INVARIANT
**Status:** [DECIDED] R2 (Q12, Q13, Q17). **Supersedes:** ADR-012's "per-call approval only, no always-allow in v1." **Amends:** SC5; the transparency invariant.
**Context/forces:** Per-call approval is the safest reading of locked decision 9, but adds friction for repeated, trusted sends. A naïve "always-allow" would let cloud egress happen with no preview — breaking decision 9 / ADR-012 / SC5.
**Rationale:** Relax *deliberately and minimally*. A scoped allow is **per app+intent**, but under it the system **still renders the exact payload, still shows a cancel window (default 3 s, user-configurable), and still writes the `cloud_send` audit row**. Only the manual *Send click* is skipped; the auto-send fires when the cancel window elapses unless cancelled.
**Consequences:** SC5 is reworded: *"zero egress on the proactive path; cloud egress only via an explicit Send **or** an active user-granted scoped allow — under a scoped allow the exact payload is still displayed, a cancel window precedes egress, and the SHA-256 is audit-logged."* Decision 9 ("display the exact payload before any cloud call") survives intact; only the explicit-Send half is relaxed. Doc 13 §8's "no always-allow in v1" assumption is removed.
**Rejected:** Fully-silent scoped allow (no preview/cancel) — breaks decision 9; revert to strict per-call (rejected for friction).

---

### ADR-027 — Browser extension is a committed v1 component  (de-risks RK3 + RK4)
**Status:** [DECIDED] R2 (Q55, Q58, Q74, Q75). **New.** **Relates:** RK3, RK4, supersedes Q6's "decide URL approach at M4."
**Context/forces:** Doc 10 §2 named a companion browser extension as the *v2* fallback for the flaky UIA URL read (RK4) and the unreliable video-position capture (RK3). The flagship US1 depends on reliable video position.
**Rationale:** Promote it to **v1**. A content script reads `video.currentTime` directly (reliable position) and the tabs API gives the current URL (reliable URL) — far more robust than UIA scraping. This collapses both RK3 and RK4 from "open risk" to "engineered."
**Consequences:** (a) The extension becomes the **primary** source for video position *and* browser URL; **UIA address-bar reading is demoted to the no-extension fallback** (retro-resolving Q6 toward the extension). (b) New surfaces: a Manifest V3 extension targeting **Chrome + Opera GX** (one Chromium codebase; Edge/Firefox fast-follow — Edge trivial); a **native-messaging host**; an install step in onboarding. (c) Native-messaging host manifests register per-browser (Chrome's and Opera's host dirs) and `allowed_origins` must list **both** extension IDs `[VERIFY]`. (d) Built at **M4 with the connectors** (Q74), **YouTube connector first** (Q75) since it exercises the whole extension+native-messaging path earliest. (e) Touches Docs 02, 05, 10, 13, 16, 17. **Scope increase acknowledged** (store/sideload, cross-browser variance, native-messaging setup) — but it lives in the connector seam (locked decision 4), so no locked decision breaks.
**Rejected:** Ship the degrade-only `null`→"from the start" path for v1 (rejected — the user chose reliability for the flagship).

---

### ADR-028 — Extension↔core transport: native messaging primary, authenticated loopback fallback
**Status:** [DECIDED] R2 (Q59). **New.** **Relates:** ADR-027, Doc 13 §2.
**Context/forces:** The extension must talk to the Rust core locally. Native messaging uses a stdio pipe (no socket); a localhost socket is simpler but opens a port.
**Rationale:** **Native messaging primary** (no port — keeps the "no socket outside the gateway" property clean), **localhost fallback** only if needed.
**Consequences:** A loopback socket is **not** cloud egress (it never leaves the device), so it does not violate the two-emitter rule — but the fallback must bind **127.0.0.1 only**, be **authenticated** (a per-install token; no other local process may connect), and the **SC5 network monitor must whitelist loopback** so the fallback cannot false-trip the zero-egress test. Doc 13 §2 clarified: the two-emitter rule governs *external user-data* egress; loopback IPC between our own components is exempt-but-scoped.
**Rejected:** Localhost-primary (opens a port unnecessarily); WebSocket to a bound interface (broader exposure).

---

### ADR-029 — Broad extension host access, narrow use; data-minimization reframed as honest  ⚠️ (privacy posture)
**Status:** [DECIDED] R2 (Q60, Q61, Q15). **New.** **Amends:** Doc 13 §1, §4.
**Context/forces:** The user chose **broad host permissions** (all-site URL capture) over the minimal youtube-only scope, **empty default exclusions** (Q15), and to **accept the resulting first-run exposure** (Q61) — consistent with a "max user control" stance.
**Rationale:** Honour the choice, but constrain *use* and correct the docs' *claims*. **Broad permission, narrow use:** the extension reads **URLs + video position only — never page DOM/content.** Exclusions + incognito still gate it; URLs run through redaction; the broad permission is disclosed plainly at install.
**Consequences:** (a) **Doc 13 §1's "data minimization" claim is reworded** — with empty default exclusions *and* broad extension reach, "minimization by default" is an overclaim. The honest framing is: **minimal *defaults* + user-driven minimization + transparent disclosure.** (The architecture still makes *silent* exfiltration impossible; it no longer claims aggressive default *collection*-minimization.) (b) Doc 13 §4's "shipped defaults: password managers, banking" line is **removed**; defaults ship empty. (c) Onboarding compensates (ADR-040): consent → detect-and-suggest sensitive apps → extension install → enable. (d) The `url_pattern` exclusion kind + bubble "exclude this domain" (Q94) are the ongoing controls.
**Rejected:** Minimal youtube-only scope; auto-blocking sensitive domains by default (both rejected by the user's choices, with the risk flagged and accepted).

---

### ADR-030 — VRAM cap → 7.0 GB; co-resident weights counted; L1 co-residency made conditional  ⚠️ INVARIANT (8 GB ceiling)
**Status:** [DECIDED] R2 (Q31, Q34, Q38, Q45). **Amends:** Doc 04 R1/R3, §3–§4; Doc 12 §4; Doc 18 §4.
**Context/forces:** Q5/ADR-024 set STT to faster-whisper (~2 GB co-resident). Combined with the (tightened) cap, L1's "both models loaded" worst case during a VLM-with-image job ≈ 3B+mmproj (~3.3) + image act (~1.2) + KV (~1–2) + framework (~1) + faster-whisper (~2) ≈ **8–9 GB → breaches 8 GB.** The old projection formula counted only the *active* model.
**Rationale:** (a) Tighten the projection cap to **7.0 GB (1.0 GB margin)**. (b) The **BudgetEnforcer now counts co-resident weights**: `projected = active_model(weights+mmproj+kv+img_act) + framework + co_resident_weights`. (c) Under image-VLM memory pressure, **faster-whisper is the swap victim** (unloaded to admit the job, reloaded on next PTT). **L1 is redefined: co-resident when memory allows, swaps under pressure.**
**Consequences:** (a) New degrade-ladder order (Q38): **7B→3B → shrink ctx → unload co-resident STT → drop image → queue → refuse** (voice warmth yields before image quality). (b) Conflict rule: **warm-kept STT is protected from pattern-VLM (prio 50), which degrades to OCR-text-only; but yields to a user/enrichment image-VLM.** (c) The adaptive image downscale (Q45, ADR-032) *eases* this — a 768 px prefill under pressure fits without unloading STT more often. (d) Idle-unload 60 s (Q32) also reduces co-residency windows. (e) Doc 18 §4's budget check is re-run in Doc 21 with the ~2 GB STT figure.
**Rejected:** Keep 7.2 GB / count only the active model (rejected — would silently breach with faster-whisper).

---

### ADR-031 — Four-tier GPU job priorities
**Status:** [DECIDED] R2 (Q35). **Amends:** Doc 12 §3, Doc 06 §5.
**Context/forces:** The old three tiers (STT 100 > user-VLM 80 > pattern-VLM 50) conflated a user *waiting on a result* with a user *composing a cloud payload*.
**Rationale:** Split them: **STT 100 > user-VLM 80 > enrichment-VLM 70 > pattern-VLM 50.** "user-VLM" = the user is waiting on the answer now; "enrichment-VLM" = the "Add screen summary" affordance while composing a payload (useful, slightly less latency-critical).
**Consequences:** Preemption/co-residency rules (ADR-030) key off these four tiers. Doc 12 §3 job-priority table updated.

---

### ADR-032 — Adaptive control parameters (cap, wake budget, session gap, heartbeat, image downscale)
**Status:** [DECIDED] R2 (Q24/Q25, Q44, Q28, Q41, Q45). **Amends:** Doc 08 §3/§6, Doc 06 §3/§4, Doc 05 §4, Doc 12 §4.
**Context/forces:** Several fixed thresholds were better expressed as bounded, self-adjusting values that start conservative and open up as evidence accrues.
**Rationale & specifics (all bounded, all with cold-start defaults, all M-gate-tuned):**
- **Suggestion cap:** fixed 4/hr → **adaptive 2/hr floor → 8/hr ceiling**, click-through-driven (Q25).
- **VLM wake budget:** fixed <6/hr → **adaptive ~3/hr floor → ~10/hr ceiling**, raised when VLM-enriched suggestions out-click un-enriched (Q44). **Hard ceiling is non-negotiable** so a "valuable" VLM never starves voice; requires a defensible attribution proxy.
- **Session gap:** fixed 15 min → **rolling idle-gap distribution**, applied forward (never retro-sessionizing), **15 min cold-start default** (Q28).
- **Heartbeat:** fixed 10 s → **~5–20 s**, modulated by input activity + event density, **10 s default** (Q41).
- **VLM image downscale:** fixed 1024 px → **768 px under pressure / 1024 px with headroom**, chosen by the BudgetEnforcer at admission (Q45) — also eases ADR-030.
**Consequences:** More implementation complexity (esp. the wake-attribution proxy) and more M-gate tuning, in exchange for a quieter conservative start that earns presence. Hard bounds protect the GPU/voice/UX guarantees.
**Rejected:** Leaving all five fixed (simpler, but the user chose adaptivity).

---

### ADR-033 — Pattern-engine precision posture: conservative-to-fire, patient-to-suppress
**Status:** [DECIDED] R2 (Q22, Q27, Q29, Q77). **Amends:** Doc 08 §4–§6.
**Context/forces:** SC7 (≥50 % useful) is the central pattern risk (RK8). The levers are firing threshold, dismissal response, decay, and novelty.
**Rationale:** Tune toward **fire rarely, suppress gently** — the two combine into a system that is cautious about interrupting but doesn't over-punish a single mis-fire.
- **τ_conf 0.6 → 0.7** (fewer, higher-confidence bubbles; still `[VERIFY]` at M3).
- **Dismissal curve softened:** 1st dismiss → cooldown ×2 + decay ×0.8; 2nd → cooldown ×4 + decay ×0.6; **mute only at the 3rd dismiss** (click ×1.25 / expire ×0.9 unchanged).
- **Half-life split by pattern type:** temporal **~5 d** (time-of-day habits shift fast) vs sequence **~14 d** (workflows are stable).
- **Novelty extended:** never suggest the foreground resource *and* suppress a resource focused within the last **~10 min** (avoid "I just closed that").
**Consequences:** Doc 08 §4–§6 updated; all values `[ASSUMPTION]`, tuned at M3 against SC7. Pairs with ADR-032's adaptive cap (quiet start).

---

### ADR-034 — Intent classification via an embedding-head model + deterministic lexicon fast-path
**Status:** [DECIDED] R2 (Q50, Q53). **Supersedes:** Doc 07 §4's "deterministic, no model required in v1."
**Context/forces:** The pure lexicon misclassifies edge cases; a separate intent model would add a resident model.
**Rationale:** Add a **lightweight classifier head over the nomic embedding we already compute** for every utterance — **~0 extra resident RAM** (reuses the embedder). The deterministic lexicon stays as an **overriding fast-path** for clear cases; the `<0.6` confirm chip still gates action; the head can **refine from confirm-chip corrections** (the user's Run/Dismiss are labels).
**Consequences:** Slightly less "perfectly explainable" than pure-deterministic, mitigated by the lexicon fast-path owning the obvious cases. Doc 07 §4 rewritten; CPU-fallback threshold made configurable (Q52).
**Rejected:** A separate resident intent model (RAM cost); pure-deterministic-forever (edge-case misses).

---

### ADR-035 — Contract 3: validate-on-click (not validate-before-button)
**Status:** [DECIDED] R2 (Q63). **Amends:** Doc 15 §3, Doc 09 §4.
**Context/forces:** The old contract required a cloud-suggested action to pass `validate()` *before its button renders*. This pre-filters but diverges from how *local* connectors already show-then-fail.
**Rationale:** Relax to **validate-before-execution (at click), fail gracefully.** The safety property that matters — *nothing executes unvalidated; only connectors act* — is **preserved** (validation runs before any `ShellExecute`/protocol dispatch).
**Consequences:** This *unifies* cloud-suggested actions with the local connector failure model (both show optimistically, validate/reconstruct at click, fall back on failure — Doc 10 §6). Doc 15 §3 contract law reworded; the graceful-failure copy must be clear (a button may occasionally resolve to "couldn't resume — video is now private"). A small UX cost (a button that can fail) traded for consistency + responsiveness.
**Rejected:** Keep validate-before-button (rejected for the inconsistency with local actions).

---

### ADR-036 — The "two-emitter rule" made precise (updater + opt-in diagnostics)  ⚠️ INVARIANT (cloud boundary)
**Status:** [DECIDED] R2 (Q88, Q89). **Amends:** 00-README invariant 2, Doc 13 §2, Doc 01 SC5, Doc 16.
**Context/forces:** A literal "exactly two code paths emit network traffic" cannot survive an **app updater** or an **opt-in diagnostics** path. Stating it literally is an overclaim.
**Rationale:** Make the rule precise about *what* may leave and *through what crate*:
- **Raw user data** (history, OCR text, payloads, titles, URLs) leaves **only via the gateway crate, only after approval.** Unchanged.
- **Opt-in anonymized diagnostics** (Q89, **off by default**) is **routed through the gateway crate itself**, so "only the gateway crate opens app sockets" stays *literally true*. It sends **aggregate counters only** (wake rate, queue waits, VRAM peaks, click-through *rates*) — **never content.** Each send is **audited like `cloud_send`** and shown in the Activity & Privacy view (ADR-040).
- **The Tauri updater** (Q88) is a **separate framework-level path** carrying only app version/binary requests and **no user-derived data.** It is **documented and excluded from the SC5 *user-data*-egress test** (but still visible to the network monitor). Mechanism finalized at packaging.
**Consequences:** The SC5 test definition is sharpened: it asserts **zero *user-data* egress on the proactive path; user data leaves only after Send**, and the harness distinguishes updater traffic (to the release host) from any user-data egress. Invariant 2 in 00-README is reworded accordingly.
**Rejected:** Manual-updates-only to keep "literally two emitters" (rejected — the user deferred the updater but accepted carving it out of the data-egress audit); a separate diagnostics emitter crate (rejected — routing through the gateway keeps the single-emitter-crate property).

---

### ADR-037 — `aperture_search_history`: a gated MCP tool
**Status:** [DECIDED] R2 (Q82, Q85). **New.** **Relates:** ADR-025, ADR-026, Doc 13 §3.
**Context/forces:** The base MCP tool set (`get_context`, `list_recent`, `submit_suggestions`) only lets Claude receive a pre-built payload. Letting Claude *query* the local history is more powerful but a bigger egress surface.
**Rationale:** Add a **gated search**: Claude proposes a query → the tool handler runs **Doc 03 §5's KNN+filter retrieval**, applies **redaction + exclusions**, and **shows the user the matched results before anything returns**; each return is **audit-logged**. Claude can pull relevant history, but **nothing leaves unseen.** The exact gating *shape* (per-query approval vs the ADR-026 scoped-allow cancel-window) is decided at the **M7 MCP spike** — but the invariant-preserving constraints (gated preview, redaction, exclusions, audit) are **locked now.**
**Consequences:** Doc 09 §3 tool set grows to four; Doc 13 §3 covers the gated-search path; the existing retrieval SQL is reused.
**Rejected:** An ungated search tool (would breach the transparency invariant).

---

### ADR-038 — Optional recovery passphrase as a second key-unwrap path
**Status:** [DECIDED] R2 (Q20). **Amends:** Doc 13 §6.
**Context/forces:** DPAPI (current-user) wrap means losing the Windows account ⇒ DB unrecoverable. Some users want a recovery path.
**Rationale:** Add an **optional user passphrase** (off by default to keep the frictionless DPAPI flow) deriving a second key-encryption-key via **Argon2id**. When set, it provides recovery if the Windows account is lost.
**Consequences:** Doc 13 §6's "unrecoverable by design" softens to **"unrecoverable without your Windows account *or your recovery passphrase, if you set one*."** A passphrase is a second (opt-in, Argon2-hardened) attack surface, documented. SQLCipher remains the page-encryption choice (Q19, exact crate `[VERIFY]`).
**Rejected:** DPAPI-only with no recovery (the default if the user doesn't opt in); exportable recovery key (an alternative the user didn't pick).

---

### ADR-039 — Liquid refraction deferred to post-v1; v1 ships static glass
**Status:** [DECIDED] R2 (Q64, Q67, Q70). **Amends:** Doc 14 §3/§5, Doc 11 §3, Doc 18 §6 (C4).
**Context/forces:** The SVG `feDisplacementMap` refraction is the most GPU-costly effect on a card shared with inference.
**Rationale:** **Defer refraction to post-v1; v1 ships static glass only.** Locked decision 11 is honored — Doc 14 still documents the *full* system (the refraction recipe stays as a post-v1 enhancement), and "Liquid Meta" in v1 is carried by the **static glass** (specular sweep, lens-lip highlight, blur). Caps tighten to **blur ≤12 px, ≤2 glass surfaces** (Q67); accent shifts to a cooler **steel/silver-blue** (Q68, exact OKLCH `[VERIFY]`).
**Consequences:** **C4 re-opens** — the glass-surface cap (≤2) now diverges from the 3-visible-bubble cap. **Interim reconciliation (Q70):** ≤2 glass surfaces + a **3rd visible bubble renders in the opaque fallback class** (2 glass + 1 opaque = 3 visible); the final cap is set at the **M8 PresentMon test.** Doc 18 §6 C4 is updated to this decoupled-with-fallback rule.
**Rejected:** Ship refraction in v1 (GPU risk); lower max-visible bubbles to 2 (the user kept 3).

---

### ADR-040 — Onboarding, controls, and surfaces bundle
**Status:** [DECIDED] R2 (Q16, Q79, Q86, Q91, Q94, Q95). **Amends:** Doc 13 §8, Doc 11, Doc 03 (`exclusion_list`).
**Context/forces:** R2 added many user-facing surfaces and several controls that need to cohere, especially given empty default exclusions + broad extension access.
**Rationale (the bundle):**
- **First-run sequence (Q79):** capture stays OFF until consented; onboarding pre-opens **detect-and-suggest sensitive apps (Q16)** then **extension install** *before* first enable — surfacing the safety setup at the right moment without forcing it.
- **Detect-and-suggest (Q16):** scan installed password managers / banking apps **locally**, present as **suggestions the user confirms** (never auto-excluded) — reconciles empty defaults with safety.
- **`url_pattern` exclusion kind (Q94):** add to `exclusion_list.match_kind`; plus a one-click **"exclude this domain"** from any browser bubble — the ongoing control for broad extension URL capture.
- **Global suggestion snooze (Q95):** quiet **all** bubbles for 15 min / 1 h / until re-enabled. **Distinct from the capture toggle** — snooze silences *bubbles* while capture + pattern-learning continue; the toggle stops *everything*.
- **Activity & Privacy view (Q86):** a settings view rendering the audit log (`capture_toggle` + `cloud_send` + diagnostics sends) — the concrete answer to "when was it watching / what left the machine."
- **Tiered settings (Q91):** a simple default view + an **Advanced** panel for the proliferating tunables (TTLs, thresholds, cancel-window, hotkey, corner, dwell, diagnostics).
- **Cold-start (Q92):** a subtle one-time "learning your patterns" note, then silence until floors are met.
**Consequences:** Doc 13 §8 first-run flow rewritten; Doc 03 `exclusion_list` gains `url_pattern`; Doc 11 gains the snooze control, the Activity & Privacy view, and the explicit "useful?" thumbs (Q81, feeding SC7 + the dismissal-decay loop).
**Rejected:** Forcing exclusions before enable (the user chose "present, don't force"); a flat settings list.

---

## ADR index (R2)
| ADR | Topic | Supersedes/amends | Invariant-touching |
|---|---|---|---|
| 024 | STT backend split | ADR-019; amends ADR-003 | — |
| 025 | MCP-primary transport | refines ADR-010 | — |
| 026 | Scoped always-allow | ADR-012; SC5 | ⚠️ transparency |
| 027 | Browser extension in v1 | Q6; RK3/RK4 | — |
| 028 | Native-messaging transport | Doc 13 §2 | (clarifies) |
| 029 | Broad host access + honesty reframe | Doc 13 §1/§4 | (privacy posture) |
| 030 | 7.0 GB cap + conditional L1 | Doc 04 R1/R3, Doc 12 §4 | ⚠️ 8 GB ceiling |
| 031 | Four-tier priorities | Doc 12 §3 | — |
| 032 | Adaptive parameters | Docs 05/06/08/12 | — |
| 033 | Pattern precision posture | Doc 08 §4–§6 | — |
| 034 | Embedding-head intent model | Doc 07 §4 | — |
| 035 | Validate-on-click | Doc 15 §3, Doc 09 §4 | — |
| 036 | Two-emitter rule precision | inv. 2, Doc 13 §2, SC5 | ⚠️ cloud boundary |
| 037 | Gated search MCP tool | Doc 09 §3, Doc 13 §3 | (preserves gate) |
| 038 | Optional passphrase | Doc 13 §6 | — |
| 039 | Refraction deferred + C4 | Doc 14, Doc 18 §6 | — |
| 040 | Onboarding/controls bundle | Doc 13 §8, Doc 11, Doc 03 | — |
