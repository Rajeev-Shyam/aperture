# Doc 06 — Vision & OCR Pipeline

## 1. Interface
| | |
|---|---|
| **Inputs** | Ephemeral frames from Doc 05; wake requests from Doc 08; mutex grants from Doc 12 |
| **Outputs** | `ocr_text` + confidence into `screen_context` (always-on layer); `vlm_summary` structured JSON (on-demand layer) |
| **Resource cost** | OCR: CPU, ~100–300 MB RAM, ≤ 400 ms/frame [VERIFY]. VLM: Doc 04 loadouts (3B ~5–6 GB / 7B exclusive) |

## 2. Layer A — cheap always-on OCR (Tier 0, CPU)
- **Engine:** `Windows.Media.Ocr` (in-box, fully local, per-language packs, fast on CPU). [VERIFY accuracy on dense UI text; fallback candidates: RapidOCR/ONNX or Tesseract if in-box quality is insufficient — swap behind one `OcrEngine` trait.]
- Pre-processing: downscale frame to ≤ 1600 px long edge [ASSUMPTION: OCR quality/speed balance], grayscale.
- Output: concatenated line text + mean word confidence. Lines under confidence 0.5 are dropped [ASSUMPTION].
- The OCR text is what gets embedded (Doc 03 §5) and what feeds pattern context and payloads — **screenshots are not the default context currency, text is** (cheaper locally and ~750× cheaper in cloud tokens, Doc 09 §5).

## 3. Layer B — on-demand VLM (Tier 1, GPU, mutex)
- **Model:** Qwen2.5-VL — **3B default (L1), 7B opt-in (L2)** per Doc 04 §3. Served by the `vlm-host` sidecar (llama.cpp server + mmproj). [VERIFY server flags/version.]
- **Pre-processing:** **adaptive downscale — 768 px long edge under memory pressure / 1024 px with headroom**, chosen by the BudgetEnforcer at admission [ASSUMPTION; enforces OOM rule R2 and eases co-residency per ADR-030], one image per job, JPEG q85 (ADR-032).
- **Prompt template (system):** "You are a screen-understanding function. Given one screenshot of a Windows 11 desktop, return ONLY JSON matching the schema. Do not guess text you cannot read." 
- **Structured output schema:**
```json
{"scene":"short description","app_guess":"string","key_entities":[{"kind":"url|file|video|control|text","value":"string"}],
 "resumable_hint":{"connector_type":"browser|youtube|document|ide|none","payload_guess":{}},
 "ocr_gaps":"what the OCR likely missed","confidence":0.0}
```
Invalid JSON ⇒ one retry with a repair instruction, then discard (the pipeline never blocks on the VLM).

## 4. VLM wake-up gating (the heuristics that protect the GPU)
```
fn should_wake_vlm(ev, ocr) -> bool {
  if !capture_on() || !mutex_likely_free() { return false }
  if debounce_active(30s per app) { return false }            // anti-thrash
  let trigger =
       pattern_engine.requested_disambiguation(ev)            // (a) Doc 08 asks
    || (ocr.confidence < 0.55 && ocr.text_density > LOW)      // (b) rich frame, weak OCR
    || user_explicit_request();                               // (c) e.g. enrichment "add scene summary"
  trigger && budget_projection_ok()                           // Doc 04 R1 via Doc 12
}
```
- Wake reasons are logged (for tuning). Target wake rate: **adaptive ~3/hr floor → ~10/hr ceiling, value-driven** — raised when VLM-enriched suggestions out-click un-enriched (requires a defensible attribution proxy); the **hard ceiling is non-negotiable so a "valuable" VLM never starves voice** [ASSUMPTION; tune at M5] (ADR-032).
- **Priority tier (ADR-031):** `should_wake_vlm` keys the projection/preemption check off the **four-tier priority** — a user-explicit request → **user-VLM (80)**, an enrichment "add scene summary" → **enrichment-VLM (70)**, a pattern-requested or OCR-density-driven wake → **pattern-VLM (50)** (Doc 12 §3).
- **VLM output never gates a bubble** (Doc 02 Path A invariant): results enrich `screen_context.vlm_summary` and improve the *next* pattern cycle and future payloads.

## 5. Internal flow
> Upstream, Doc 05's **pHash near-duplicate gate** may drop a frame before it ever reaches this pipeline — on a static screen no OCR/embed runs (Q72).
```
frame ──► [pHash gate, Doc 05: near-dup? drop] ──► downscale ──► OCR ──► screen_context.ocr_text ──► embed (Doc 03)
                          │
                          └─ gate(§4)? ──► GPU job {kind:VLM, prio:50} ──► Doc 12 mutex
                                                  └─► vlm-host ──► JSON ──► screen_context.vlm_summary
```

## 6. Failure modes
| Failure | Behavior |
|---|---|
| Mutex denied / projection over budget | Skip the wake (OCR-only is the contract); pattern engine proceeds on text |
| Sidecar cold-load slow | Job has a 10 s deadline [ASSUMPTION]; on timeout, cancel + log; never retried in a loop |
| VLM hallucinated entities | `resumable_hint` is advisory only — connectors validate against their own captured state before any suggestion uses it |
| OCR garbage on image-heavy frames | Confidence filter drops it; density heuristic may wake the VLM instead |
| Language pack missing | Fall back to en + notice; [VERIFY language coverage] |

---
> **R2 amendments applied** (see docs/19–21): ADR-031, ADR-032; Q72 (upstream pHash gate), Q85 (adaptive image downscale).
