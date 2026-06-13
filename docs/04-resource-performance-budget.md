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
| Whisper **small** | ~1.0 GB |
| faster-whisper distil-large-v3 **int8** (opt-in) | ~1.48 GB (measured upstream) |

## 3. The two sanctioned loadouts
| Loadout | Resident set | Projected VRAM | Rule |
|---|---|---|---|
| **L1 — default** | Qwen2.5-VL-**3B** + Whisper **small** + framework | ~5–6 + ~1 + ~1 ≈ **7–8 GB worst case; ~6.5 GB typical** [VERIFY] | Both may be co-resident; the mutex still serializes *execution* so UI compositing keeps bandwidth |
| **L2 — high-quality (opt-in)** | Qwen2.5-VL-**7B** *exclusive* | ~8–9 GB at peak ⇒ must run **alone**, context capped, image capped | Strict time-sharing: Whisper is unloaded while 7B is resident; a PTT press forces 7B unload → STT load |
No third loadout exists. 13B+ local models are out of scope (weights alone ≈ 7.4 GB).

## 4. The GPU mutex (time-sharing) rule — normative
1. A single-permit mutex guards **execution** on the GPU; the Resource Manager (Doc 12) is its only issuer.
2. In **L1**, both sidecars may stay loaded, but only one runs a job at a time (protects the 448 GB/s bandwidth the overlay also needs — Doc 14's degrade contract keys off "mutex held").
3. In **L2**, residency itself is exclusive: requesting STT while 7B is resident triggers unload→load swap (priority rules in Doc 12).
4. Every load request is preceded by a **projection check** (§6 R1); refusal triggers the degrade ladder.

## 5. Load/unload strategy
- Demand-load on first job; **idle-unload** after 90 s without jobs (range 60–120 s, default 90 [VERIFY]).
- **Warm-keep:** if ≥3 PTT uses in 10 min, pin Whisper small resident (1 GB is cheap) and make the VLM the swap victim.
- **Capture OFF ⇒ kill both sidecars immediately** (process death = guaranteed release; SC6 < 3 s).
- Cold-load SLAs to verify: 3B VLM < 4 s, 7B < 6 s, Whisper small < 2 s [VERIFY].

## 6. OOM-avoidance rules (normative)
- **R1 — projection cap:** never start a load/job where `weights + mmproj + KV(ctx_tokens) + image_activation(n_images) + framework > 7.2 GB` (0.8 GB safety margin under 8 GB). KV(ctx) and image_activation use the §2 planning figures until measured. [VERIFY]
- **R2 — image prefill is the silent killer:** attention activations grow ~O(n²) at prefill; therefore cap VLM input to **one image**, downscaled to ≤ 1024 px long edge [ASSUMPTION; Doc 06], and cap VLM context at 4K tokens in L2 [ASSUMPTION].
- **R3 — degrade ladder (in order):** drop 7B→3B → shrink ctx (8K→4K→2K) → drop the image (OCR-text-only prompt) → queue the job behind the mutex → refuse with a UI notice.
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
