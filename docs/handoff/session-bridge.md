<!-- Handoff/process doc (not an architecture doc). Captures the decision context
     that produced the R2 refinement (Docs 19–21) and the v2 draft (Doc 22).
     Kept for provenance; the authoritative design is Docs 00–21. -->

# 🔄 CLAUDE SESSION BRIDGE — READ THIS FIRST

You are a new Claude session receiving a handoff from a previous session.
Read this entire document before responding.

After reading, your first response should:
1. Confirm you understand the project state and what's decided vs open
2. Ask at most ONE clarifying question if something genuinely blocks you
3. Apply the "How This User Works" section from your very first response
4. Do NOT re-open anything in "Locked Decisions" — those are settled

The goal is seamless continuation. Don't make Rajeev re-explain anything here.

---

## What We Were Working On

Deep-dive architecture review of **Aperture** — a local-first, privacy-preserving
desktop assistant for Windows 11 (Tauri + Rust). The session covered the full v1
architecture (models, RAG, pattern engine, VRAM budget, privacy model), then
pivoted to scoping **v2** (agent execution layer). A full v2 spec was produced.
The repo is at M0 (skeleton with `todo!()` stubs, not compile-verified).

Repo: https://github.com/Rajeev-Shyam/aperture

---

## Technical Context

| | |
|---|---|
| **Project** | Aperture — local-first proactive desktop assistant |
| **Repo** | https://github.com/Rajeev-Shyam/aperture |
| **Stack** | Tauri v2, Rust workspace, React/TS UI, SQLite + sqlite-vec, llama.cpp sidecars |
| **Hardware target** | RTX 5060 8GB VRAM, 16GB RAM, Ryzen 9 (Windows 11) |
| **Current milestone** | M0 complete (skeleton). M1–M3 is the next build block. |
| **Architecture docs** | `docs/` in the repo (Docs 00–18 + R2 amendments in Docs 19–21) |
| **Doc set status** | Docs 19–21 (R2 refinement pass) uploaded by user in this session — not yet in repo |

---

## What Was Established (Do Not Re-Investigate)

1. **v1 is a pattern-recognising recommender, not a general assistant.** It watches the screen, detects recurring workflows, and surfaces one-click state-resumption bubbles. It cannot execute tasks or follow instructions.
2. **Four models in v1:** Windows.Media.Ocr (free, always-on CPU), 3B VLM via llama.cpp (GPU, on-demand), faster-whisper STT (GPU, on-demand), nomic-embed-text-v1.5 (CPU, always resident ~520MB). Claude is optional cloud tier only.
3. **VRAM ceiling is 7.0 GB** (amended from 7.2 GB in README by ADR-030 in Doc 19). VLM and STT cannot co-reside during an image job — they fast-swap. BudgetEnforcer enforces this.
4. **Aperture idle RAM is ~570MB** (nomic-embed ~520MB + Tauri shell ~30-50MB). VLM and STT are sidecar processes — killed when not needed, VRAM fully released.
5. **Retention TTLs** (from Doc 20/Doc 03): events + ctx_vec 90d, OCR text 30d, voice 30d, suggestions + patterns 180d, audit log 30d post-purge.
6. **RAG is how v1 queries work.** Voice query → embed → KNN search against stored OCR events → retrieve top 10-20 → answer grounded in those. Claude's context window is irrelevant for v1 queries — the DB is the memory.
7. **No hallucination** because all answers are retrieve-then-summarise, not generate-from-memory. Grounding errors (bad OCR) can happen; hallucination cannot.
8. **Half-life decay:** temporal patterns ~5 day half-life, sequence patterns ~14 day half-life (ADR-033). Dismissing a bubble accelerates decay. You don't need to tell it you stopped something — time does it.
9. **v1 is the second brain framing.** It passively builds a local, encrypted, semantically searchable record of everything you've worked on. Closest competitor is Rewind.ai but Aperture is fully local.
10. **v2 is a hybrid agent architecture.** Local = eyes (capture/OCR) + hands (Win32/UIA action executor). Cloud = brain (Claude Pro/Max via MCP decides what to do next). No large local planning model needed.
11. **v2 does NOT exist in the repo.** The v2 spec was produced in this session as a separate document. It is not part of the current codebase and is not in docs/. It needs to be grilled before being added.
12. **Latency for v2 agent loop:** ~2-7 seconds per action step. Acceptable for task automation; not for instant reactive use.
13. **Rolling summary** handles v2 context window — each step sends current screen + short summary of prior steps, not full history. Context window is not a constraint.

---

## Locked Decisions (from Docs 00 + R2)

These are architectural invariants. Do not re-open them.

| Decision | What it means |
|---|---|
| 3 invariants | 8GB VRAM ceiling (7.0GB cap), transparency gate (nothing leaves without user seeing it), capture toggle (3s shutdown) |
| Local-first | Behavioral history never leaves the device on the proactive path |
| VLM sidecar | llama.cpp process, killed when not needed — only guaranteed VRAM release method |
| STT sidecar | faster-whisper (Python/CTranslate2) on GPU, whisper.cpp on CPU fallback |
| nomic-embed | CPU, always resident, 768-dim vectors, sqlite-vec for KNN |
| Claude transport | MCP primary → CLI fallback → API third (ADR-025) |
| Connector set (v1) | Browser URL, YouTube timestamp, document, IDE file. That's it. |
| Privacy | Raw frames never stored. One encrypted SQLite file. One crate opens network. |
| Hardware target | RTX 5060 8GB VRAM / 16GB RAM / Ryzen 9 (this is the design target, not a compromise) |

---

## Current State

- **Repo at M0**: skeleton with faithful crate signatures and `todo!("Mn: ...")` stubs. Not compile-verified (README explicitly states this).
- **v1 design is fully specified**: Docs 00–21 are the authoritative design. R2 amendments (Docs 19–21, uploaded in this session) are the most current version.
- **v2 design is drafted**: A spec was produced (separate file: `aperture-v2-spec-draft.md`) but not yet grilled, not in the repo, not started.
- **Next build target**: M1 → M2 → M3 (first ~30-40% of the project). This is v1 work only.

---

## Outstanding Items (Priority Order)

1. **Get repo to compile** — first `cargo check` will surface dependency version issues (all marked `[VERIFY]` in the code)
2. **M1: Capture subsystem** — WGC sampler, heartbeat (adaptive 5-20s), pHash near-duplicate gate, capture toggle, SC6 gate passes
3. **M2: OCR + embedding + store** — Windows.Media.Ocr pipeline, nomic-embed inference, sqlite-vec KNN, retention TTL enforcement, SC5 holds
4. **M3: Pattern engine + first bubble** — n-gram counting, half-life decay, threshold-based bubble trigger, basic Tauri overlay UI, SC2 gate passes
5. **Grill the v2 spec** — open questions (Q-V2-01 through Q-V2-10) need resolving before v2 build starts

---

## Open Questions

**v1 build:**
- Dependency versions in `Cargo.toml` are marked `[VERIFY]` — need resolving on first compile
- VLM binary/GGUF model choice is `[VERIFY]` — exact model name + llama.cpp build flags
- SQLCipher crate choice is `[VERIFY]`

**v2 (deferred — grill the spec first):**
- Q-V2-01: UIA label matching reliability across apps
- Q-V2-02: Claude MCP payload size with base64 screenshot
- Q-V2-03: Behaviour when agent visits excluded app mid-task
- Q-V2-04: Step cap default (50 — assumption)
- Q-V2-05: VLM needed for screen description or Claude vision sufficient?
- Q-V2-06: Rolling summary quality — when does context window become a concern?
- Q-V2-07: Error recovery on ActionError::ElementNotFound
- Q-V2-08: Local fallback planning model — in or out of v2?
- Q-V2-09: UAC-elevated windows — hard block or prompt?
- Q-V2-10: Agent loop VRAM priority tier value

---

## Decisions Made This Session

| Decision | Why |
|---|---|
| v2 = hybrid (local eyes/hands + Claude brain) | Hardware constraint: can't run large local planning model + v1 stack simultaneously in 8GB VRAM |
| Claude Pro/Max for v2, not free tier | Rate limits hit fast in an agent loop (10+ API calls per task) |
| Rolling summary for v2 context management | Prevents context window bloat; each step stays constant size |
| v2 action surface = UI only (Win32/UIA) | Keeps scope achievable; no file system / registry / shell in v2 |
| v2 out of repo scope for now | Spec needs grilling before build starts |
| First 30-40% = M1 through M3 | That's capture → OCR → embedding → pattern engine → first bubble |

---

## How This User Works (Apply From First Response)

**Communication style:**
AuDHD (autistic + ADHD) — this is an accessibility requirement, not a preference.
- **Answer first, always.** Conclusion in the first line. Reasoning after.
- **Short chunks.** Don't dump everything at once. Give a piece, check before continuing.
- **No walls of text.** Bullets over paragraphs. Bold the one load-bearing line.
- **One question at a time max.** Never a list of questions.
- **No filler.** No "great question", no apology spirals, no throat-clearing.
- Swearing is fine. Dry humour welcome. Casual register.

**Technical depth:**
Treat as a competent peer. Comfortable with Rust, Python, agentic AI, LangGraph, MCP, RAG, llama.cpp, QLoRA. Don't over-explain fundamentals. Use jargon freely, gloss only genuinely rare terms.

**Working style:**
- Give a recommendation, then alternatives — not a flat list of options
- Concrete next step at the end of every response
- Follow tangents but offer to bring back to main thread
- Direct pushback when he's wrong — don't manage, correct

**Things to avoid:**
- Don't re-explain the three invariants unless asked
- Don't re-open locked decisions
- Don't pad or hedge unnecessarily
- Don't give him 6 options at once

**Things that worked well:**
- ELI5 explanations on new concepts (he asked for these explicitly for architecture concepts)
- Concrete numbers (latency tables, VRAM budgets, token counts)
- Honest "I don't know — it's in doc 03" rather than guessing
- Following tangents naturally then offering to refocus

---

## Files Produced This Session

- `aperture-v2-spec-draft.md` — full v2 agent execution layer spec (draft, not grilled)
- `aperture-session-bridge.md` — this file
- `aperture-claude-code-prompt.md` — Claude Code prompt for M1-M3 build work

Docs 19–21 (R2 refinement) were uploaded by the user but are not yet in the repo.

---

## Why We're Bridging

Long session covering both full architecture deep-dive and v2 scoping — context
starting to accumulate. Fresh session before starting the build work.

---

*Bridge generated from Aperture architecture session. Session covered: models,
RAG, VRAM budget, retention, decay, hallucination prevention, v2 agent
architecture, context window management, hardware constraints, and v2 spec draft.*
