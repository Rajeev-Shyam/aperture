<!-- Handoff/process doc (not an architecture doc). The build brief for the first
     ~35% (M1→M3) of v1. Authoritative specs are Docs 00–21; where this brief cites
     an R1 value that R2 changed (e.g. 7.2 GB cap → 7.0 GB), the docs win. -->

# Aperture — Claude Code Build Prompt (M1 → M3)

## What you are doing

You are implementing the first ~35% of **Aperture** — a local-first, privacy-preserving
desktop assistant for Windows 11. The repo is a Rust workspace (Tauri v2) that
currently sits at **M0: an architecture-faithful skeleton with `todo!()` stubs**.

Your job is to make it real, one milestone at a time, without breaking the three
invariants or re-opening any locked decisions.

**Repo:** https://github.com/Rajeev-Shyam/aperture

**Read before touching any code:**
- `docs/00-README.md` — invariants, locked decisions, crate map
- `docs/03-data-model.md` — DB schema (this is ground truth for the DB)
- `docs/04-resource-budget.md` — VRAM/RAM budget (never exceed 7.0 GB VRAM)
- `docs/05-capture.md` — capture subsystem spec (M1)
- `docs/06-vision-ocr.md` — OCR pipeline spec (M2)
- `docs/08-pattern-engine.md` — pattern engine spec (M3)
- `docs/12-orchestration.md` — GPU mutex + resource manager (touches M2/M3)

---

## The Three Invariants — Never Break These

These are build-time + runtime guarantees. Every change you make must preserve them.

**1. 8 GB VRAM ceiling (cap: 7.0 GB)**
- One heavyweight GPU model resident at a time
- GPU mutex enforced by the `orchestration` crate
- BudgetEnforcer projection = `active(weights + mmproj + kv + img_act) + framework + co_resident_weights`
- STT is the swap victim when VLM image job is running
- Idle-unload after 60s of no jobs

**2. Transparency gate**
- ONLY the `reasoning-gateway` crate may open network sockets
- Proactive path (M1-M3) is zero-network — SC5 test must pass at every milestone
- `cargo xtask lint-emitters` enforces this at the lint level

**3. Capture toggle**
- OFF must stop capture, halt recording, force-unload GPU models within 3s
- Visible indicator must reflect true state
- SC6 gate tests this

---

## Locked Decisions — Do Not Re-Open

- Windows 11 only, Tauri v2, Rust workspace
- VLM = llama.cpp sidecar process (separate binary in `crates/vlm-host/`)
- STT = faster-whisper sidecar (Python/CTranslate2, GPU); whisper.cpp (CPU fallback)
- Embeddings = nomic-embed-text-v1.5, CPU, always resident, 768-dim
- Vector search = sqlite-vec (KNN)
- Storage = SQLite WAL + SQLCipher-style encryption, single file
- Raw frames NEVER persisted — frame → OCR → drop frame
- Proactive path is fully local, zero network, zero GPU on Path A
- Connector set = browser URL, YouTube timestamp, document, IDE file (not expandable in v1)

---

## What Is NOT In Scope (Do Not Build)

- **v2 agent execution layer** — `action-executor`, `agent-loop`, `task-manager`,
  `screen-serializer` — these DO NOT EXIST yet and are NOT your task here.
  A v2 spec exists separately but v2 is not in the repo and not part of this work.
- M4 (connectors + browser extension) — after M3
- M5 (VLM sidecar) — after M4
- M6 (STT + voice) — after M5
- M7 (reasoning gateway + Claude integration) — after M6
- Multi-monitor support — post-v1
- Any cloud calls, any network egress, any Claude API calls

---

## Milestone Targets

### Step 0 — Get It Compiling

**Before anything else:** run `cargo check` and fix all dependency issues.
The README explicitly states the workspace has NOT been compile-verified.
Expect `[VERIFY]` dependency versions to need resolving.

Order of attack:
1. Fix `contracts` crate first — everything depends on it
2. Fix `db` crate — schema is the most settled part
3. Fix remaining crates one by one
4. Run `cargo xtask gate m0` — must pass before moving to M1

Gate: `cargo check` clean, `cargo xtask gate m0` passes, idle RAM ≤ 1.5 GB.

---

### M1 — Capture Subsystem

**Crate:** `crates/capture/`
**Spec:** `docs/05-capture.md`

What to build:

**WGC sampler (Windows Graphics Capture)**
- Capture the foreground window frame using Windows.Graphics.Capture API
- Primary monitor only (multi-monitor is post-v1)
- Adaptive heartbeat: default 10s, range 5-20s (modulated by input activity + event density)
- Do NOT store raw frames — capture → hand to OCR pipeline → drop

**pHash near-duplicate gate**
- Compute perceptual hash of each frame before sending to OCR
- If pHash within Hamming threshold of last frame → skip OCR/embed (redundant work)
- Threshold is `[ASSUMPTION]` — start at Hamming distance ≤ 4, tune at M2
- Store `thumb_phash` in the events table (already in schema per Doc 03)

**WinEvent/UIA hooks**
- Foreground window change events via WinEvent hook
- Read: app name, window title, process ID
- Browser URL via UIA address-bar reading (this is the NO-EXTENSION fallback — the extension is M4)

**Exclusion list enforcement**
- Check `exclusion_list` table before processing any event
- Match kinds: `process`, `window_class`, `title_regex`
- `url_pattern` kind is in the schema (Doc 03 amendment) but URL capture is M4

**Capture toggle**
- Toggle OFF: stop WGC sampler, stop WinEvent hooks, kill both GPU sidecars within 3s
- Visible indicator in UI reflects true state
- Write `capture_toggle` audit event on every toggle

**Gate (SC6):** Toggle OFF → VRAM drops to ~0 within 3s, SC5 still holds (zero network).

---

### M2 — OCR + Embeddings + Store

**Crates:** `crates/vision-ocr/`, `crates/embedding/`, `crates/db/`
**Specs:** `docs/06-vision-ocr.md`, `docs/03-data-model.md`

What to build:

**OCR pipeline** (`crates/vision-ocr/`)
- Primary: `Windows.Media.Ocr` (built into Windows 11, no external dep)
- Fallback: `RapidOCR` or `Tesseract` behind the `OcrEngine` trait
- Input: frame from capture, scaled to ≤ 1600px before OCR
- Drop results with confidence < 0.5 (Doc 06 §2)
- Output: structured text with region metadata → hand to embedding
- OCR must complete in ≤ 400ms (SC2 partial gate)

**Embedding pipeline** (`crates/embedding/`)
- Model: nomic-embed-text-v1.5 (137M params, 768-dim, CPU)
- Load via llama.cpp GGUF (CPU inference only — this model never touches GPU)
- Keep resident always — do not unload between events (~520MB RAM)
- Input: OCR text string → output: Vec<f32> of length 768
- Must complete in ≤ 300ms per embed
- Write vector to `ctx_vec` column in events table via sqlite-vec

**DB writes** (`crates/db/`)
- Schema is already in `crates/db/` (Doc 03 is ground truth)
- Write each event: `window_events` + `ctx_vec` in one transaction
- Retention enforcement: events/ctx_vec TTL 90d, OCR text 30d
- Run retention cleanup on startup and on a background timer (daily)
- sqlite-vec KNN query must return sane results (sanity test: embed a query,
  check that the top-1 result is semantically related)

**Gate (M2):** OCR ≤ 400ms measured, embed ≤ 300ms measured, KNN returns correct
top-5 for a test query, SC5 still holds (zero network egress on Path A).

---

### M3 — Pattern Engine + First Bubble

**Crates:** `crates/pattern-engine/`, `crates/suggestion-generator/`, `src-tauri/`, `ui/`
**Specs:** `docs/08-pattern-engine.md`, `docs/11-bubble-ui.md`

What to build:

**Pattern engine** (`crates/pattern-engine/`)

N-gram sequence counting:
- For each new event, look at the preceding N events in the session
- Increment count for each (A→B), (A→B→C) sequence observed
- Session boundary: rolling idle-gap distribution, 15min cold-start default (ADR-032)

Half-life decay (ADR-033):
- Temporal patterns (time-of-day): half-life ~5 days
- Sequence patterns (A→B→C): half-life ~14 days
- Apply decay on every read, not on every write
- Formula: `score = count * 0.5^(days_since_last / half_life)`

Threshold + bubble trigger:
- When a pattern score exceeds threshold AND current context matches → emit candidate
- Threshold is `[ASSUMPTION]` — start at score ≥ 3.0, tune after M3
- Suggestion cap: adaptive 2-8 per hour (start at 4/hr default)

Dismissal decay:
- Every dismiss without click → apply 1.5x decay multiplier to that pattern
- Every click → reinforce (multiply score by 1.2)
- "Useful?" thumbs down → apply 3x decay multiplier

**Suggestion generator** (`crates/suggestion-generator/`)
- Takes pattern engine candidates → resolves to a `BubbleSpec`
- BubbleSpec contains: app name, action_ref, display text, connector type
- Validate that the action_ref is resolvable before emitting (validate-on-click per ADR-035,
  but the spec must be at least partially valid at generation time)

**Bubble UI** (`src-tauri/`, `ui/`)
- Overlay window: bottom-right corner (default), draggable, position persisted
- Bubble anatomy: app icon, short description, click to resume
- Dwell: 20s auto-dismiss (hover pauses), 3 max visible (≤2 glass + 1 opaque)
- Glass style: static glass (NO liquid refraction — deferred post-v1 per ADR-039)
- Blur: ≤12px (ADR-039)
- "Useful?" thumbs affordance on each bubble
- Global snooze: 15min / 1hr / until re-enabled (distinct from capture toggle)
- Degrade hook: glass→opaque + reduced motion while `gpu_busy`

**Gate (SC2):** Scripted workflow (open app A → do thing → open app B → do thing → repeat
3 times) produces a bubble within 2s of the third repetition. SC5 still holds.

---

## Crate Dependency Order

Build and fix in this order (each depends on the ones above it):

```
contracts       ← fix first, everything depends on it
db              ← schema, migrations, KNN
event-bus       ← tokio broadcast<Event>
privacy         ← redaction, exclusion, audit log (needed by capture)
capture         ← WGC, hooks, toggle (M1)
vision-ocr      ← OCR pipeline (M2)
embedding       ← nomic-embed inference (M2)
pattern-engine  ← n-gram + decay (M3)
suggestion-generator ← BubbleSpec (M3)
orchestration   ← GPU mutex, sidecar lifecycle (wire up M2/M3)
src-tauri       ← Tauri IPC, overlay windows (M3 UI)
ui              ← React bubble components (M3 UI)
```

Do NOT touch `vlm-host`, `stt-host`, `connectors`, `voice`, `reasoning-gateway` yet.
Those are M4+.

---

## Key Constraints Per Crate

**`capture`:**
- No GPU usage on Path A (the proactive capture path)
- pHash gate must sit BEFORE OCR — it only removes work, never delays a bubble
- Heartbeat adaptive range: 5-20s. Default 10s. Never go below 5s.
- Idle threshold: 60s of no foreground change → pause capture

**`vision-ocr`:**
- Never store the raw frame. Frame → OCR → drop.
- Confidence filter: drop OCR results < 0.5 confidence
- VLM gate (the on-demand VLM path) is M5 — stub it with `todo!("M5: VLM wake")` for now

**`embedding`:**
- nomic-embed is CPU-only — never let it touch the GPU mutex
- Always-resident — do not unload between events
- sqlite-vec write must be in the same transaction as the event row write

**`pattern-engine`:**
- No GPU, no network, no disk writes (reads only, writes go via `db` crate)
- Decay is applied at read time, not write time
- Pattern scores are never sent anywhere — they are local only

**`orchestration`:**
- The GPU mutex is the single source of truth for VRAM state
- Projection formula must include co_resident_weights (ADR-030)
- Hard cap: 7.0 GB. Refuse admission above it.
- Priority order: STT 100 > user-VLM 80 > enrichment-VLM 70 > pattern-VLM 50

---

## SC5 — The Zero-Egress Test (Must Pass at Every Milestone)

This is a CI gate. It must stay green through M1, M2, M3.

What it tests: running the full proactive path (capture → OCR → embed → pattern →
bubble) produces zero network traffic.

How it works: `cargo xtask gate m0` (and eventually m1/m2/m3) runs the SC5 monitor.
The `cargo xtask lint-emitters` check ensures no crate outside `reasoning-gateway`
opens a socket at the lint level.

**If you add any dependency that opens a network socket, the gate fails. Do not add such deps to any crate except `reasoning-gateway`.**

---

## Dependency Notes

Several Cargo.toml dependency versions are marked `[VERIFY]`. When resolving:

- `sqlite-vec` — use the latest crates.io version, check it compiles on Windows MSVC
- `windows` crate — use `0.58` or latest, enable features: `Win32_Graphics_Capture`,
  `Win32_Media_Audio`, `Win32_UI_Accessibility`, `Win32_UI_WindowsAndMessaging`
- `sqlcipher` / SQLCipher — check `[VERIFY]` in the db crate; use `rusqlite` with
  the `bundled-sqlcipher` feature if available on Windows MSVC
- `nomic-embed` inference — run via `llama-cpp-2` crate (CPU only, no CUDA needed for this model)
- `tauri` — v2, check `src-tauri/Cargo.toml` for the exact version already specified

---

## How to Work

1. Start with `cargo check` in the workspace root. Fix errors in dependency order above.
2. Run `cargo xtask gate m0` — must pass before touching M1.
3. Implement M1 crates. Run SC6 gate. Run SC5. Both must pass.
4. Implement M2 crates. Run M2 gate checks. SC5 must still pass.
5. Implement M3 crates. Run SC2 gate. SC5 must still pass.
6. Every `[VERIFY]` item you confirm — note the actual value in a comment.
7. Every `[ASSUMPTION]` item that turns out wrong — flag it before changing it.

Do not implement beyond M3 without checking in. The next milestone (M4: connectors
+ browser extension) has design decisions (`[OPEN]` items in the docs) that need
resolving first.

---

## What Good Output Looks Like

- `cargo check` clean with no warnings on the workspace
- `cargo xtask gate m0` passes
- `cargo xtask gate m1` passes (SC6: toggle OFF → VRAM ~0 in < 3s)
- `cargo xtask gate m2` passes (OCR ≤ 400ms, embed ≤ 300ms, KNN sane)
- `cargo xtask gate m3` passes (SC2: scripted workflow → bubble in < 2s)
- SC5 holds at every gate (zero network egress on proactive path)
- No raw frames anywhere in storage
- Idle RAM stays ≤ 1.5 GB (nomic-embed + shell + overhead)
- All `todo!("M4+: ...")` stubs remain — do not implement beyond M3
