# Doc 04 — Resource & Performance Budget

## 1. Hardware envelope (the facts the budget is built on)
- **GPU: RTX 5060** — 8 GB GDDR7, 128-bit bus, **448 GB/s** bandwidth, GB206, 3,840 CUDA cores, ~145 W TGP. The 8 GB is the binding constraint.
- **System RAM: 16 GB** — tight once the user's browser/IDE are open; every resident megabyte of ours competes with their work.
- **CPU: AMD Ryzen** (model unspecified) — assume ≥ 6 cores [ASSUMPTION]; Tier 0 must stay within "a few %" steady-state.

## 2. VRAM line items (estimates from current GGUF listings/measurements — **[VERIFY all on hardware]**)
| Item | VRAM |
|---|---|
| Inference framework baseline (llama.cpp CUDA context) | ~0.5–1.0 GB |
| Qwen2.5-VL-**7B** Q4_K_M weights | ~4.68 GB |
| 7B vision projector (mmproj, FP16 — always FP16) | ~1.35 GB |
| Per-image activation spike (ViT + image KV) | ~1.2 GB + ~3–5k tokens/image |
| KV cache, 7–8B class @ 8K ctx (FP16, GQA) | ~1–2 GB |
| **7B "loaded + one image" total** | **~8–9 GB → breaches the 8 GB ceiling** |
| Qwen2.5-VL-**3B** Q4_K_M weights + mmproj | ~1.93 GB + ~1.34 GB |
| **3B "loaded + one image" total** | **~5–6 GB → fits with headroom** |
| **faster-whisper small** (CTranslate2, GPU `stt-host`) | **~2 GB** (carry the **1–2 GB range** for L1 worst-case) [ADR-024] |
| faster-whisper distil-large-v3 **int8** (opt-in) | ~1.48 GB (measured upstream) |
| whisper.cpp **base** (default) / **tiny** — CPU fallback | separate cheap GGML bundle, no VRAM [ADR-024] |

## 3. The two sanctioned loadouts
| Loadout | Resident set | Projected VRAM | Rule |
|---|---|---|---|
| **L1 — default** | Qwen2.5-VL-**3B** + **faster-whisper** + framework | 3B+mmproj ~3.27 + framework ~0.75–1.0 + KV ~1.0 + STT ~2.0 ⇒ **~7.0 GB at the cap for a *text-only* VLM co-resident with STT** [VERIFY] | **Conditional co-residency** (ADR-030): 3B VLM + faster-whisper co-resident *when memory allows*; **under image-VLM pressure faster-whisper is the swap victim** (unloaded to admit the job, reloaded on next PTT). The mutex still serializes *execution* so UI compositing keeps bandwidth. |
| **L2 — high-quality (opt-in)** | Qwen2.5-VL-**7B** *exclusive* | ~8–9 GB at peak ⇒ must run **alone**, context capped, image capped | Strict time-sharing: STT is unloaded while 7B is resident; a PTT press forces 7B unload → STT load |
No third loadout exists. 13B+ local models are out of scope (weights alone ≈ 7.4 GB).

> **[ADR-030 / FIX 4.1 — how to read "co-resident".** With faster-whisper (~2 GB) and a 7.0 GB cap, true co-residency *during a VLM image job* is the **exception, not the rule**: STT is resident while the VLM is idle/unloaded, and STT is the swap victim during VLM image jobs. In practice L1 behaves mostly as **fast-swapping single-heavyweight** with *opportunistic* co-residency; warm-keep (§5) holds STT during voice-heavy spells at the cost of forcing VLM unloads. The adaptive 768 px downscale (ADR-032) and 60 s idle-unload widen the windows where co-residency *is* admissible. **No admission ever exceeds 7.0 GB — the ceiling holds by construction** (measured at M5). See Doc 21 §4/§7.2.]

## 4. The GPU mutex (time-sharing) rule — normative
1. A single-permit mutex guards **execution** on the GPU; the Resource Manager (Doc 12) is its only issuer.
2. In **L1**, both sidecars may stay loaded *when the projection permits*, but only one runs a job at a time (protects the 448 GB/s bandwidth the overlay also needs — Doc 14's degrade contract keys off "mutex held"). The projection **counts co-resident weights** (ADR-030): `projected = active(weights + mmproj + kv + img_act) + framework + co_resident_weights`. Under image-VLM pressure the enforcer unloads STT before admitting the job.
3. In **L2**, residency itself is exclusive: requesting STT while 7B is resident triggers unload→load swap (priority rules in Doc 12).
4. Every load request is preceded by a **projection check** (§6 R1); refusal triggers the degrade ladder.

## 5. Load/unload strategy
- Demand-load on first job; **idle-unload** after **60 s** without jobs (range retained; default 60 [ADR-032/Q32]).
- **Warm-keep:** if **≥2 PTT uses in 5 min**, pin faster-whisper resident and make the VLM the swap victim. Note the higher churn vs a ~2 GB STT, bounded by the 20 s min-residency (Doc 12 §7). A warm-kept STT is protected from pattern-VLM (which degrades to OCR-text-only) but yields to a user/enrichment image-VLM (ADR-030/Q36).
- **Capture OFF ⇒ kill both sidecars immediately** (process death = guaranteed release; SC6 < 3 s).
- Cold-load SLAs to verify: 3B VLM < 4 s, 7B < 6 s, Whisper small < 2 s [VERIFY].

## 6. OOM-avoidance rules (normative)
- **R1 — projection cap:** never start a load/job where `active(weights + mmproj + KV(ctx_tokens) + image_activation(n_images)) + framework + co_resident_weights > 7.0 GB` (**1.0 GB** safety margin under 8 GB; the projection **counts co-resident weights** — ADR-030). KV(ctx) and image_activation use the §2 planning figures until measured. [VERIFY]
- **R2 — image prefill is the silent killer:** attention activations grow ~O(n²) at prefill; therefore cap VLM input to **one image**, downscaled **adaptively to 768 px (under memory pressure) / 1024 px (with headroom)**, chosen by the BudgetEnforcer at admission [ADR-032; Doc 06], and cap VLM context at 4K tokens in L2 [ASSUMPTION]. The 768 px path also eases co-residency (ADR-030).
- **R3 — degrade ladder (in order):** drop 7B→3B → shrink ctx (8K→4K→2K) → **unload co-resident STT** → drop the image (OCR-text-only prompt) → queue the job behind the mutex → refuse with a UI notice. (Voice warmth yields *before* image quality — ADR-030/Q38.)
- **R4 — runtime choice:** llama.cpp-family runtimes that can spill to system RAM degrade (slow) rather than hard-crash; prefer them for resilience. [VERIFY behavior on the 5060 driver.]
- **R5 — hard exclusions:** no 13B+ local models; no concurrent dual heavyweight execution; no VLM job during active video capture bursts.

## 7. System RAM budget (16 GB)
| Item | Resident RAM | Note |
|---|---|---|
| **App shell (Tauri + WebView2)** | **~30–50 MB idle** | the explicit shell line item; Electron's 150–300 MB baseline is why it lost |
| Rust core (bus, hooks, connectors, orchestration) | ~150–300 MB [VERIFY] | |
| SQLite + page cache | ~50–100 MB | tunable |
| OCR engine resident | ~100–300 MB [VERIFY] | Windows.Media.Ocr is OS-hosted; alt engines cost more |
| nomic-embed-text-v1.5 (137M) | ~520 MB resident | confirmed-scale figure |
| Sidecar host overhead (when loaded) | ~300–600 MB each [VERIFY] | weights live in VRAM; host buffers in RAM |
| **Aperture steady-state target** | **< 1.5 GB (SC1)** | leaves ≥ 14 GB for the user |

## 8. CPU budget
Idle (capture ON, user idle): < 2 % average. Event burst (focus storm): < 15 % for < 1 s (debounce in Doc 05). OCR pass: one core ≤ 400 ms. Embedding: one core ≤ 300 ms. [All VERIFY.]

## 9. Measurement plan (feeds the M-gates, Doc 16)
`nvidia-smi --query-gpu=memory.used -lms 250` around load/unload; ETW/Windows Performance Recorder for CPU; PresentMon for overlay frame drops during inference; a network monitor for SC5. Each [VERIFY] figure above becomes a recorded number in the M1/M5/M6 gate reports, and this document is updated to the measured values.

> **RAM note (ADR-024, Doc 21 §4):** the GPU `stt-host` is a **Python faster-whisper (CTranslate2)** service (~300–600 MB host RAM when loaded), which makes the SC1 `< 1.5 GB` steady-state **tighter but plausible**; the nomic→MiniLM embedder fallback (ADR-005) is the release valve. [VERIFY M2/M5].

---
> **R2 amendments applied** (see docs/19–21): ADR-024 (faster-whisper GPU / whisper.cpp CPU split, ~2 GB STT figure), ADR-030 (7.2→**7.0 GB** cap, co-resident-weights in the projection, conditional L1 co-residency, degrade-ladder reorder, FIX 4.1 honest framing), ADR-032 (adaptive 768/1024 px image, 60 s idle-unload), Q36 (warm-keep ≥2 PTT/5 min).

## Implementation status (2026-08-19) — the 7.0 GB figure is now the FALLBACK, not the constant (owner decision #43)

ADR-030's cap was written for one machine's 8 GB card and hard-coded as `7.0`. Owner decision #43 (Doc 24, 2026-08-16) reversed that: the ceiling is **derived from the installed GPU at startup** by `orchestration::startup_projection_ceiling_gb()` — one `nvidia-smi --query-gpu=memory.total` call (OnceLock, `CREATE_NO_WINDOW`), ceiling = **total − 1.0 GB headroom**, clamped to **[2.0, 31.0] GB**, falling back to **7.0** when the query fails or no NVIDIA GPU answers.

What this does and does not change:
- **The 1.0 GB safety margin and the R1 projection formula are unchanged.** Only the number the formula is compared against moves, and only upward on a larger card.
- **On the 8 GB dev machine the behavior is identical** — 8 − 1 = 7.0, the same value ADR-030 locked. Every measured figure in this doc still stands there.
- The clamp is what keeps this honest on the edges: a 2 GB card cannot produce a ceiling that admits nothing, and a 32 GB card cannot produce one that treats VRAM as free.
- **The M5 load-times gate still asserts the constant** rather than `BudgetEnforcer::ceiling_gb()`. That is a known gap, not a decision — switching it is listed in the handoff leftovers.

ADR-030 keeps its ⚠️ INVARIANT status: the ceiling is still a hard admission bound that no load or job may exceed. What decision #43 changed is that it is now the *right* bound for the machine it is running on.

## Implementation status (2026-08-22) — idle wakeups trimmed (2026-08-19 review finding 6)

- **§8 idle budget.** The overlay's 10 Hz hit-test re-measure interval now runs only while at least one interactive rect is published (`useHitTestRects`: a measurement that publishes `[]` clears it; the MutationObserver / motion listeners re-arm it) — with nothing on screen, zero JS timer wakeups per WebView. The Rust cursor poller drops from 60 Hz to **30 Hz when no window has rects or a modal** (`hit_test::IDLE_POLL_INTERVAL`), since with zero rects the reconcile answer cannot change. **SC3 is still unmeasured** — the < 2 % figure remains an assumption; this only removes cost that was provably doing nothing.
- **VLM weights (decision #30, settings `loadout.vlm_download`)** now carry a `sha256` beside `bytes`; the fetcher hashes while streaming (a resumed `.part` is hashed from its prefix first) and refuses to install on mismatch; the URLs are pinned to HF revision `5037fcf…` instead of `main` (review finding 5). `is_present` stays size-only by design (no multi-GB hash on every status poll); content is verified once, at download.
Full detail: `docs/handoff/session-bridge-2026-08-22-v2-wiring.md`.
