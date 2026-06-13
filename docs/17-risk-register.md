# Doc 17 — Risk Register & Open Questions

## 1. Risk register
| ID | Risk | L | I | Mitigation | Validation | Owner doc |
|---|---|---|---|---|---|---|
| RK1 | **8 GB VRAM ceiling**: 7B VLM alone ≈ 8–9 GB loaded+image | High | High | L1 3B-default loadout; GPU mutex; 7.2 GB projection; degrade ladder | M5 measures both loadouts; enforcer runs on measured numbers | 04, 12 |
| RK2 | **16 GB RAM tightness** with user's browser/IDE open | Med | Med | Tauri shell; sidecar RAM measured; SC1 < 1.5 GB steady-state | M0/M5 RAM line items on hardware | 04 |
| RK3 | **Video position capture** has no universal API — least-certain connector | High | Med | Honest hierarchy (URL `t=` → heuristics → null) + "from the start" degrade | M4 spike on real YouTube sessions | 10 |
| RK4 | **Browser URL via UIA** is localization/version-fragile | High | Med | ControlType-based resolution, last-known-URL fallback; v2 extension companion decision point | M4 spike across Chrome/Edge/Firefox + one non-English locale | 05, 10 |
| RK5 | **WGC yellow capture border** may be unacceptable always-on | Med | Med | Per-monitor capture + truthful tray indicator; investigate border suppression for non-UWP callers [VERIFY] | M1 user check; fallback: duplication-API path | 05 |
| RK6 | **UI/ML GPU contention** (glass vs inference on one 5060) | Med | Med | ≤ 3 glass surfaces; degrade-under-load contract keyed to `gpu_busy` | M8 PresentMon during VLM jobs | 14, 11, 12 |
| RK7 | **Claude transport variability** (CLI headless large-stdin caveat & 10 MB cap; Desktop MCP is pull-only; API needs a key) | Med | Med | One gateway interface; both CLI+MCP built at M7; payload size guards; model/headers never hard-coded | M7 fallback matrix with each transport disabled | 09 |
| RK8 | **Suggestion noise** misses SC7 (≥ 50 % useful) | Med | High | τ_conf, caps, cooldowns, dismissal decay, mute; silence on cold start | M3 scripted precision; dogfood telemetry | 08 |
| RK9 | **In-box OCR accuracy** as the primary text signal | Med | Med | Confidence gating; VLM wake on low-confidence rich frames; engine swappable behind `OcrEngine` | M2 accuracy sample on dense UI text | 06 |
| RK10 | **MCP pull-UX friction**: Aperture can't push a prompt into Claude Desktop | Med | Low | Handoff UX (copy starter prompt) + `aperture_submit_suggestions` return path; CLI remains the push option | M7 usability pass | 09 |
| RK11 | **L2 swap thrash** (alternating voice/VLM demand under 7B) | Low | Med | Min-residency debounce; auto-recommend L1 on detected thrash | M6 stress script | 12 |
| RK12 | **Encryption/key-wrapping lib risk** (CVE or DPAPI surface change) | Low | Med | Pinned lib, wrapped key isolates blast radius, documented recovery posture | M9 review | 13 |

## 2. Decision thresholds to validate on real hardware (single list)
Projected-VRAM cap **7.2 GB** · idle-unload **90 s** (60–120 range) · VLM wake rate **< 6/h** · τ_conf **0.6** · min support **3** · suggestion cap **4/h**, cooldown **30 min** · dwell **12 s** · glass surfaces **≤ 3**, blur **≤ 16 px** · OCR ≤ **400 ms**, embed ≤ **300 ms** · STT job priority **100**, deadlines VLM **10 s** / STT **15 s** · payload warn **50 KB**. *All [VERIFY]; each becomes a measured value at its owning M-gate.*

## 3. Open questions (tracked, not blocking)
1. WGC border suppression availability for non-UWP callers on current Win11 builds — determines RK5's final answer. [VERIFY]
2. Exact SQLCipher-equivalent library + DPAPI wrapping API choice. [VERIFY]
3. Whisper CPU fallback real-time factor on the target Ryzen (is the fallback actually usable?). [VERIFY]
4. Claude Code CLI: current flag set, JSON output stability, and whether the large-stdin caveat persists in the shipping version. [VERIFY]
5. MCP server registration specifics for current Claude Desktop (config schema, tool-consent UX). [VERIFY]
6. Whether VS Code title-parsing is stable enough or the `code -g` CLI should be primary for the IDE connector. [VERIFY]
7. Whether per-call consent (no "always allow") survives dogfood feedback or needs a scoped-allow design in v1.1. [ASSUMPTION holding]
8. Non-English OCR language-pack coverage and its effect on pattern quality. [VERIFY]
