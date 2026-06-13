# Aperture — Foundational Architecture Document Set

A local-first, multimodal, proactive desktop assistant for Windows 11. It learns how the user works by toggleably watching the screen, surfaces proactive shortcut suggestions as floating glass bubbles, and clicking a bubble **resumes a state** (e.g. reopens a YouTube video at the exact timestamp). Push-to-talk voice is both behavioral telemetry and a query channel answered with text. Pattern detection is fully local; Claude is invoked **only on explicit user action** through a swappable transport, and every cloud payload is previewed and user-approved first.

## How to read this set
The documents are dependency-ordered, bottom-up. Foundations (01–04) define what the system is and what it may cost. Component specs (05–12) each define one subsystem's inputs, outputs, internal logic, failure modes, and resource cost. Cross-cutting docs (13–17) bind them. 18 is the mandatory coherence review.

| # | Document | Layer | Depends on |
|---|---|---|---|
| 01 | Product Requirements & Scope (PRD) | Foundations | — |
| 02 | System Architecture Overview | Foundations | 01 |
| 03 | Data Model & Schema | Foundations | 01–02 |
| 04 | Resource & Performance Budget | Foundations | 01–03 |
| 05 | Capture & Event Subsystem (Tier 0) | Component | 02–04 |
| 06 | Vision & OCR Pipeline | Component | 03–05 |
| 07 | Voice / Push-to-Talk STT | Component | 03–04, 12 |
| 08 | Behavior & Pattern Engine | Component | 03, 05–06 |
| 09 | Reasoning & Claude Integration | Component | 03, 13 |
| 10 | Deep-Link / State-Resumption Connectors | Component | 03, 05 |
| 11 | Bubble UI / Overlay | Component | 08–10, 14 |
| 12 | Orchestration & Resource Manager | Component | 04–07 |
| 13 | Privacy, Security & Consent Design | Cross-cutting | 03, 05, 09 |
| 14 | Design System — Chromemorphism & Liquid Meta | Cross-cutting | 11–12 |
| 15 | Interface Contracts | Cross-cutting | all components |
| 16 | Build Sequencing & Milestone Plan | Cross-cutting | all |
| 17 | Risk Register & Open Questions | Cross-cutting | all |
| 18 | Coherence & Connection Review | Review | all |

## Conventions
- **[VERIFY]** — a figure or API detail that must be confirmed at build time on the real target (RTX 5060 / 16 GB / Ryzen) or against current vendor docs. Do not treat as settled.
- **[ASSUMPTION]** — a choice made to proceed, with stated reasoning; revisit if evidence contradicts it.
- **Grounding note.** The set was produced against an attached 2026 local-stack research report plus independent verification. Where any figure here contradicts that report, the report wins and the figure must be re-confirmed.

## Locked decisions (do not relitigate)
1. Windows 11, local-first hybrid. 2. v1 primary job: screen understanding → proactive bubbles. 3. Bubbles are actions (deep-link / state resumption). 4. Bounded connector set: browser URL, video timestamp, documents, IDE files. 5. Suggestions are proactive. 6. Push-to-talk voice, multimodal. 7. Capture + patterns local; heavy reasoning is Claude. 8. Behavioral history in a local DB that never leaves the device. 9. Context transparency: exact-payload preview + one-click enrichment before any cloud call. 10. Capture toggle with clean resource release + indicator. 11. Chromemorphism & Liquid Meta design system. 12. Hardware ceiling: RTX 5060 8 GB VRAM, 16 GB RAM, Ryzen CPU — 8 GB VRAM is the binding constraint.

**Clarified (also locked):** (A) the proactive loop is **fully local**; Claude only on explicit user action, transport swappable across Claude Desktop MCP / Claude Code CLI / Messages API, all transport specifics [VERIFY]. (B) Voice = telemetry **and** query channel; answers render as **text** in a bubble (no TTS). (C) Stack chosen freely for the hardware budget → **Tauri shell, Rust core, llama.cpp/whisper-family sidecars, SQLite + sqlite-vec**.

## The three invariants every document must honor
1. **8 GB VRAM ceiling** — one heavyweight GPU model resident at a time (the GPU mutex); 3B-VLM default loadout; projected-VRAM cap 7.2 GB (Doc 04).
2. **The transparency gate** — exactly two code paths may emit network traffic, both inside the Reasoning Gateway, both only after the user approves the exact serialized payload (Docs 09, 13).
3. **The capture toggle** — OFF stops capture, halts recording, and force-unloads GPU models within 3 s, with a visible indicator (Docs 05, 12).
