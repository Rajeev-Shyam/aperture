# Doc 20 — Document Amendments (Pass R2)

Precise, section-by-section deltas to apply to Docs 00–18 from the R2 refinement pass. Each entry: **location · change**. ADR-driven changes cite the ADR (Doc 19); standalone value tweaks are marked `[value]`. Apply these to the living docs; Doc 21 re-runs coherence over the amended set.

**Legend:** ⟶ means "becomes." `[VERIFY]`/`[ASSUMPTION]` tags carry over unless stated.

---

## Doc 00 — README
- **Invariant 2 (transparency gate / two-emitter rule).** Reword per **ADR-036**: "exactly two code paths may emit network traffic" ⟶ **"Raw user data leaves only via the Reasoning Gateway crate, only after the user approves the exact payload. The gateway crate is the only code that opens application network sockets — including the opt-in, off-by-default, aggregate-only diagnostics path. The Tauri app-updater is a separate framework path carrying no user-derived data."**
- **Invariant 2, scoped-allow.** Add per **ADR-026**: approval may be an explicit Send **or** an active user-granted scoped allow under which the exact payload is still displayed, a cancel window precedes egress, and the SHA-256 is audit-logged.
- **Locked decisions / "Clarified" block.** Add a pointer: transport primary is now **MCP** (ADR-025); the **browser extension is a v1 component** (ADR-027).

## Doc 01 — Product Requirements
- **US3 acceptance (MCP path).** Add the MCP-primary variant per **ADR-025**: trigger = *"Ask Claude" stages the approved payload + shows a handoff*; the gate fires inside the `aperture_get_context` tool handler; the answer returns via `aperture_submit_suggestions` and renders in a bubble on the same schema. Criterion (a) (preview bytes == wire bytes, hash-equal) is unchanged.
- **US3 acceptance, scoped-allow.** Note that a user-granted scoped allow may replace the per-call Send, with the payload still displayed + cancel window + audit (ADR-026).
- **SC5.** Reword per **ADR-026 + ADR-036**: ⟶ **"Zero *user-data* egress on the proactive path; cloud egress only via an explicit Send *or* an active scoped allow (payload displayed + cancel window + audit). The app-updater path carries no user data and is excluded from this test; the harness distinguishes it."**
- **SC7.** Method addition per **Q81**: keep target ≥50 % useful; add an explicit **"useful?" thumbs** on bubbles as a cleaner signal than inferred click/dismiss (also feeds the dismissal-decay loop, Doc 08 §7).
- **NG2.** Unchanged, but note the PTT **click-to-toggle** alternative (Q49) stays bounded (30 s cap + visible pill) so "no always-listening" holds.

## Doc 03 — Data Model & Schema
- **`exclusion_list.match_kind`** `[ADR-040]`: add **`'url_pattern'`** to the enum (`process | window_class | title_regex | url_pattern`).
- **`ctx_vec`.** Unchanged — **768-dim confirmed** (Q2 affirms FIX 1).
- **`suggestions`.** Add a column/field for explicit usefulness rating (Q81) — e.g. `useful_rating TEXT` (`up|down|null`) alongside `outcome`.
- **Context-payload schema.** Note `event_trail` cap stays 50 but is **user-adjustable in the enrichment panel within that cap** (Q71). Cache layout note (Q84) is a Doc 09 concern; no schema change.
- **§5 retrieval SQL.** Now also invoked by the **gated `aperture_search_history` MCP tool** (ADR-037) — same KNN+filter, results redacted + exclusion-filtered + previewed before return.
- **thumb_phash** `[Q72]`: gains an active consumer — a **near-duplicate-frame gate** (skip OCR/embed when a new frame's pHash is within a Hamming threshold of the last). Threshold `[ASSUMPTION]`, tuned at M2.
- **Retention TTLs (§6).** Unchanged (Q73): events/ctx_vec 90 d, OCR 30 d, voice 30 d, suggestions/patterns 180 d, audit-survives-purge 30 d.

## Doc 04 — Resource & Performance Budget
- **STT line items** `[ADR-024]`: GPU `stt-host` = **faster-whisper (CTranslate2)**; carry the **1–2 GB range** for L1 worst-case (faster-whisper small ≈ 2 GB), not a single ~1 GB. CPU fallback = **whisper.cpp base/tiny** (separate, cheap GGML).
- **§3 L1 loadout** `[ADR-030]`: redefine ⟶ **"3B VLM + faster-whisper co-resident *when memory allows*; under image-VLM pressure faster-whisper is the swap victim (reloaded on next PTT)."** L1 worst case re-stated with the ~2 GB STT figure (see Doc 21 §4).
- **§4 mutex / projection** `[ADR-030]`: projection now **counts co-resident weights**: `projected = active(weights+mmproj+kv+img_act) + framework + co_resident_weights`.
- **R1 projection cap** `[ADR-030]`: **7.2 GB ⟶ 7.0 GB** (1.0 GB margin).
- **R2 image cap** `[ADR-032]`: fixed ≤1024 px ⟶ **adaptive 768 px (under pressure) / 1024 px (headroom)**, chosen at admission.
- **R3 degrade ladder** `[ADR-030]`: ⟶ **7B→3B → shrink ctx → unload co-resident STT → drop image → queue → refuse.**
- **§5 idle-unload** `[value, Q32]`: 90 s ⟶ **60 s** (range note retained).
- **§5 warm-keep** `[value, Q36]`: ≥3 PTT/10 min ⟶ **≥2 PTT/5 min** (note the higher churn vs ~2 GB STT, bounded by 20 s min-residency).

## Doc 05 — Capture & Event Subsystem
- **§2 / browser URL** `[ADR-027]`: the **browser extension is the primary URL source** (tabs API); **UIA address-bar reading is the no-extension fallback.** RK4 mitigation is now "ship the extension," not "spike then decide."
- **§4 heartbeat** `[ADR-032]`: fixed 10 s ⟶ **adaptive ~5–20 s** (input-activity + event-density modulated; 10 s default).
- **§4 sampling** `[Q72]`: insert a **pHash near-duplicate gate** before OCR/embed (skip redundant work on static screens).
- **§4 scope / §debounce / idle** `[Q42, Q78]`: unchanged — foreground-window crop, **primary-monitor only in v1**, 300 ms debounce, 60 s idle threshold.
- **WGC border (RK5)** `[Q40]`: unchanged plan — per-monitor capture + truthful indicator; investigate non-UWP suppression at M1.

## Doc 06 — Vision & OCR Pipeline
- **§2 OCR engine** `[Q43]`: unchanged — `Windows.Media.Ocr` default, RapidOCR/Tesseract fallback behind the `OcrEngine` trait; ≤1600 px, drop <0.5 confidence (Q46).
- **§3 VLM image** `[ADR-032]`: ⟶ **adaptive 768/1024 px** (one image, q85).
- **§3 VLM context / repair** `[Q48]`: unchanged — 4K ctx cap in L2, one JSON-repair retry then discard.
- **§4 wake gate** `[ADR-032]`: target ⟶ **adaptive ~3–10/hr, value-driven** (raise when VLM-enriched suggestions out-click; **hard ceiling protects voice**; attribution proxy required). `should_wake_vlm` keys off the four-tier priority (ADR-031).
- **§5 flow:** note the upstream pHash gate (Doc 05) may suppress a frame before OCR.
- **§6 language packs** `[Q47]`: unchanged — auto-detect installed packs, en fallback + notice.

## Doc 07 — Voice / Push-to-Talk STT
- **§2 capture** `[Q49]`: add a **click-to-toggle** alternative to press-and-hold (key/pill toggles listening on/off); bounded by the 30 s cap + visible pill (NG2 preserved). Hotkey default Ctrl+Win+Space, configurable. Max 30 s, VAD 300 ms (Q51) unchanged.
- **§3 transcription** `[ADR-024]`: GPU path = **faster-whisper** (small / opt-in distil-large-v3 int8). **CPU fallback = whisper.cpp base (default) / tiny** — SC4 not promised on the CPU path.
- **§4 intent** `[ADR-034]`: rewrite — **lightweight classifier head over the nomic embedding** (primary) + **deterministic lexicon fast-path** (override for clear cases); `<0.6` confirm chip retained; head refines from confirm-chip corrections.
- **§6 CPU-fallback trigger** `[Q52]`: the **6 s swap threshold is user-configurable** (auto-fallback otherwise).

## Doc 08 — Behavior & Pattern Engine
- **§3 sessionization** `[ADR-032]`: 15 min fixed ⟶ **rolling idle-gap distribution** (forward-applied, 15 min cold-start default).
- **§4 half-life** `[ADR-033]`: single 7 d ⟶ **temporal ~5 d / sequence ~14 d.**
- **§4 temporal buckets / §9 prune** `[Q76]`: unchanged — 2 h buckets, weekly prune at weighted support <0.5.
- **§5 semantic substitution** `[Q30]`: unchanged — cosine ≥0.75 (eval M3).
- **§5 novelty** `[ADR-033]`: extend ⟶ suppress the foreground resource **and** any resource focused in the last **~10 min.**
- **§6 trigger rule:**
  - τ_conf `[ADR-033]`: 0.6 ⟶ **0.7.**
  - min support `[Q23]`: unchanged at **3.**
  - cooldown `[Q26]`: unchanged at **30 min.**
  - global cap `[ADR-032]`: fixed ≤4/hr ⟶ **adaptive 2→8/hr (click-through-driven).**
- **§7 feedback** `[ADR-033]`: dismissal ⟶ **1st dismiss: cooldown ×2 + decay ×0.8; 2nd: cooldown ×4 + decay ×0.6; mute at 3rd** (click ×1.25 / expire ×0.9 unchanged). Add: explicit **"useful?" thumbs** (Q81) feeds this loop (up ≈ strong click, down ≈ dismiss-with-signal).

## Doc 09 — Reasoning & Claude Integration
- **§2/§3 transport** `[ADR-025]`: **MCP is primary**, CLI fallback, API third. Default order **MCP → CLI → API.** Remove "primary candidate: CLI" framing. US3 = stage + handoff (Doc 01).
- **§3 MCP tools** `[ADR-037]`: tool set ⟶ `aperture_get_context`, `aperture_list_recent`, `aperture_submit_suggestions`, **`aperture_search_history` (gated)**. Search returns are redacted + exclusion-filtered + previewed before return; audited. UX shape at M7.
- **§3 fallback UX** `[Q83]`: unchanged — fall through with a visible notice; offline ⟶ local answer stands; never queue silently.
- **§4 validation** `[ADR-035]`: ⟶ **validate-on-click** (button shows optimistically; connector validates before execution; graceful failure). Unifies with the local action model.
- **§5 caching** `[Q84]`: cache the stable prefix **plus the connector schema + redaction-rules block** (more of the stable prefix cached); benefits CLI/API-fallback paths mainly (MCP is primary). Pricing/TTLs `[VERIFY]`.
- **Diagnostics path** `[ADR-036]`: the gateway crate also carries the opt-in anonymized diagnostics payload type (aggregate-only, audited).

## Doc 10 — Deep-Link / Connectors
- **§1 trait / registry:** note the **v2 expansion target is communication-app threads (Slack/Teams/Discord)** (Q90) — the trait must accommodate thread/channel deep-links (`slack://…`, etc.).
- **§2 browser** `[ADR-027]`: URL via the **extension (tabs API)** primary; UIA fallback. Validation now **on-click** (ADR-035).
- **§3 YouTube** `[ADR-027]`: position via the **extension content script (`video.currentTime`)** primary — RK3 resolved; URL-`t=` and `null`→"from the start" remain as fallbacks. **Built first at M4** (Q75). TTL 7 d unchanged.
- **§4 documents** `[Q62]`: add **per-app MRU registry reads** to the path-resolution ladder (more robust than title-parse; `[VERIFY]`/version-fragile per app); "unresolved ⇒ no capture" still the floor; opt-in `app_hint` unchanged. TTL 7 d.
- **§5 IDE (VS Code)** `[Q56]`: method (title-parse vs `state.vscdb` vs `code -g`) **decided at the M4 spike per VS Code version**; `vscode://` deep-link retained. TTL 7 d.
- **Browser TTL** `[Q57]`: unchanged 24 h.

## Doc 11 — Bubble UI / Overlay
- **§2 windowing** `[Q42]`: primary-monitor only in v1; multi-monitor at M8.
- **§3 lifecycle** `[values]`: dwell 12 s ⟶ **20 s** (Q65, hover still pauses); max **3 visible**, with **≤2 glass + opaque 3rd** under ADR-039/C4; placement **user-configurable corner, default bottom-right** (Q66), draggable + persisted.
- **§3 anatomy** `[Q81]`: add an explicit **"useful?" thumbs** affordance (SC7 signal + feedback loop).
- **§4 enrichment panel** `[Q71]`: "Add more history" controls **event_trail length up to the 50 cap.** "Add screenshot" downscale unchanged.
- **§4 / preview** `[ADR-026]`: under a scoped allow, the panel still renders the exact payload + a **cancel-window countdown** (default 3 s, configurable) before auto-send.
- **§5 voice surfaces** `[Q49]`: the listening pill is **clickable to stop** (toggle mode).
- **New surfaces** `[ADR-040]`: **global snooze** control (15 min / 1 h / until re-enabled; distinct from the capture toggle); **Activity & Privacy** view (audit log incl. diagnostics sends); **tiered settings** (simple + Advanced).
- **§6 degrade hook** `[Q80]`: unchanged — glass→opaque + reduced motion while `gpu_busy`, restore on release.

## Doc 12 — Orchestration & Resource Manager
- **§3 priorities** `[ADR-031]`: ⟶ **STT 100 > user-VLM 80 > enrichment-VLM 70 > pattern-VLM 50.**
- **§3 deadlines** `[Q33]`: fixed VLM 10 s / STT 15 s ⟶ **set by M5/M6 measured cold-load + inference times** (carry the old figures as interim).
- **§4 projection** `[ADR-030]`: include **co_resident_weights**; cap **7.0 GB**; STT is the swap victim under image-VLM pressure; warm-kept STT protected from pattern-VLM (degrades to OCR-text-only) but yields to user/enrichment image-VLM.
- **§5 sidecar mgmt** `[Q93]`: unchanged — restart w/ backoff (max 3) then degrade; **one-time "reduced mode" notice on persistent failure**; health visible in the Activity & Privacy view.
- **§7 min-residency** `[Q37]`: unchanged at **20 s.**
- **Diagnostics** `[ADR-036]`: telemetry counters may be sent only via the gateway, opt-in, aggregate-only, audited (Q89).

## Doc 13 — Privacy, Security & Consent
- **§1 principles** `[ADR-029]`: **reword "data minimization."** With empty default exclusions + broad extension reach, "minimization by default" is an overclaim ⟶ **"minimal *defaults* + user-driven minimization + transparent disclosure."** Silent exfiltration remains architecturally impossible; default *collection*-minimization is not claimed.
- **§2 two-emitter rule** `[ADR-036]`: make precise (raw user data via gateway only; diagnostics via gateway, opt-in/aggregate/audited; updater = separate no-user-data path, excluded from the SC5 user-data test). Loopback IPC (extension native-messaging fallback) is exempt-but-scoped (127.0.0.1 + authenticated; SC5 whitelists loopback) `[ADR-028]`.
- **§3 transparency** `[ADR-026]`: add the scoped-allow path (payload still displayed + cancel window + audit). Add the **gated `aperture_search_history`** path `[ADR-037]`.
- **§4 minimization at source** `[ADR-029, Q15]`: **remove the "shipped defaults: password managers, banking" line** — defaults ship **empty.** The **extension reads URLs + video position only, never page DOM/content**; exclusions + incognito gate it. Incognito title-heuristic exclusion unchanged (Q14).
- **§4 exclusion kinds** `[ADR-040]`: add **`url_pattern`** + the "exclude this domain" bubble action.
- **§5 redaction** `[Q21]`: unchanged — 6 deterministic rules + per-item remove backstop.
- **§6 at-rest** `[ADR-038]`: SQLCipher (crate `[VERIFY]`); DPAPI wrap **plus an optional Argon2id passphrase second-unwrap path** (off by default); "unrecoverable" softened accordingly.
- **§7 audit** `[ADR-040]`: surfaced via the **Activity & Privacy view**; diagnostics sends are audited too. 30 d post-purge survival unchanged (Q18).
- **§8 consent / first-run** `[ADR-040]`: **sequence ⟶ consent → detect-and-suggest sensitive apps → extension install → enable capture.** Per-call consent now also offers the **scoped allow** (ADR-026). Cold-start: one-time "learning your patterns" note (Q92).

## Doc 14 — Design System
- **§2 blur scale** `[ADR-039]`: ceiling 16 ⟶ **12 px** (scale 8/12).
- **§2 accent** `[Q68]`: chrome-cyan ⟶ **cooler steel/silver-blue** OKLCH ramp (exact values `[VERIFY]`).
- **§3 liquid refraction** `[ADR-039]`: **deferred post-v1**; recipe retained as a documented future enhancement. v1 = static glass.
- **§5 caps** `[ADR-039]`: **≤2 glass surfaces** (interim) + opaque 3rd bubble; final cap at M8 PresentMon. Degrade-under-load contract unchanged (Q80).

## Doc 15 — Interface Contracts
- **Contract 3 (Connector trait)** `[ADR-035]`: law reworded — `validate()` is mandatory before an action **executes** (at click), not before its **button renders**; the safety guarantee (nothing executes unvalidated; only connectors act) is preserved.
- **Contract 2 (Context Payload):** add that `user_approved` may be set by an active scoped allow (ADR-026), with payload-display + cancel-window + audit still required.
- **Contract 5 (Gateway):** transport order MCP→CLI→API (ADR-025); the gateway is also the sole emitter of opt-in diagnostics (ADR-036).

## Doc 16 — Build Sequencing
- **M4** `[ADR-027, Q74, Q75]`: scope now includes the **browser extension + native-messaging host**; gate adds "extension installs, native messaging works (loopback whitelisted in SC5), exclusions honored through it." **YouTube connector built first.** RK3/RK4 "spikes" ⟶ "build the extension."
- **M5** `[ADR-030]`: projection table replaced by measured numbers **including co-resident weights**; admission never exceeds **7.0 GB**.
- **M7** `[ADR-025, ADR-037]`: MCP is the pre-declared primary (still build both transports); add the **`aperture_search_history` gated-tool UX** decision; SC5 strict = zero **user-data** egress (updater excluded).
- **SC5 harness** `[ADR-036, ADR-028]`: distinguish updater traffic; whitelist loopback for the extension fallback.
- **Staged rec #2** `[Q39]`: confirmed — 3B default, 7B opt-in flag.

## Doc 17 — Risk Register
- **RK3 (video position):** **downgraded** — extension content-script `currentTime` is reliable (ADR-027); residual risk = extension install friction / store policy.
- **RK4 (browser URL via UIA):** **downgraded** — extension tabs API primary; UIA is fallback.
- **RK7 (transport):** reframed — **MCP-primary** (ADR-025); CLI caveat off the critical path.
- **RK10 (MCP pull-UX friction):** **upgraded** to central — MCP is now the default cloud UX.
- **New RK13 — Browser-extension lifecycle:** Manifest V3 / store-review / cross-browser (Chrome + Opera GX) / native-messaging-host registration friction; **L Med · I Med**; mitigated by one Chromium codebase + guided install; validated at M4. Owner: Doc 10/13.
- **New RK14 — Broad extension host access (privacy surface):** broad permission with narrow use + empty default exclusions; **L Med · I Med**; mitigated by URL-only use, exclusions/incognito gating, install-time disclosure, `url_pattern` + "exclude this domain"; the residual exposure is **accepted** (Q61) and the docs reworded honestly (ADR-029). Owner: Doc 13.
- **Decision-threshold list (§2):** update changed values — τ_conf **0.7**; projection cap **7.0 GB**; idle-unload **60 s**; suggestion cap **adaptive 2→8/hr**; VLM wake **adaptive ~3–10/hr**; warm-keep **≥2 PTT/5 min**; blur **≤12 px**; glass surfaces **≤2**; dwell **20 s**; deadlines **measured at M5/M6**.

## Doc 18 — Coherence Review
- Superseded for the R2 set by **Doc 21** (re-run). Specifically: **C4** updated (≤2 glass + opaque 3rd; final at M8); **§4 budget** re-checked with the ~2 GB STT figure and conditional L1; **§3 FIX 2** (L1 co-residency) refined to *conditional* co-residency (ADR-030).
