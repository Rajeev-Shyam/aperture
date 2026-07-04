# Doc 17 — Risk Register & Open Questions

## 1. Risk register
| ID | Risk | L | I | Mitigation | Validation | Owner doc |
|---|---|---|---|---|---|---|
| RK1 | **8 GB VRAM ceiling**: 7B VLM alone ≈ 8–9 GB loaded+image | High | High | L1 3B-default loadout; GPU mutex; **7.0 GB projection counting co-resident weights** (ADR-030); degrade ladder | M5 measures both loadouts; enforcer runs on measured numbers | 04, 12 |
| RK2 | **16 GB RAM tightness** with user's browser/IDE open | Med | Med | Tauri shell; sidecar RAM measured; SC1 < 1.5 GB steady-state | M0/M5 RAM line items on hardware | 04 |
| RK3 | **Video position capture**: extension content-script `currentTime` is reliable; residual risk = extension install friction / store policy | Med | Med | Extension content script (`video.currentTime`) primary (ADR-027); URL `t=` → null "from the start" retained as fallbacks | M4 built on real YouTube sessions | 10 |
| RK4 | **Browser URL capture**: extension tabs API is primary; UIA fallback is localization/version-fragile | Med | Med | Extension tabs API primary (ADR-027); UIA address-bar reading is the no-extension fallback | M4 build the extension + UIA fallback across Chrome/Opera GX + one non-English locale | 05, 10 |
| RK5 | **WGC yellow capture border** may be unacceptable always-on | Med | Med | Per-monitor capture + truthful tray indicator; investigate border suppression for non-UWP callers [VERIFY] | M1 user check; fallback: duplication-API path | 05 |
| RK6 | **UI/ML GPU contention** (glass vs inference on one 5060) | Med | Med | **≤ 2 glass surfaces + opaque 3rd bubble** (ADR-039; refraction deferred post-v1); blur ≤ 12 px; degrade-under-load contract keyed to `gpu_busy` | M8 PresentMon during VLM jobs sets the final cap | 14, 11, 12 |
| RK7 | **Claude transport variability**: MCP is primary (pull-UX); the CLI headless large-stdin caveat & 10 MB cap are now off the critical path; API needs a key | Med | Med | One gateway interface, transport order **MCP → CLI → API** (ADR-025); both built at M7; payload size guards; model/headers never hard-coded | M7 fallback matrix with each transport disabled | 09 |
| RK8 | **Suggestion noise** misses SC7 (≥ 50 % useful) | Med | High | τ_conf, caps, cooldowns, dismissal decay, mute; silence on cold start | M3 scripted precision; dogfood telemetry | 08 |
| RK9 | **In-box OCR accuracy** as the primary text signal | Med | Med | Confidence gating; VLM wake on low-confidence rich frames; engine swappable behind `OcrEngine` | M2 accuracy sample on dense UI text | 06 |
| RK10 | **MCP pull-UX friction (central)**: MCP is now the default cloud UX, so its pull-only handoff — Aperture can't push a prompt into Claude Desktop — sits on the primary path | Med | Med | Handoff UX (stage approved payload + handoff) + `aperture_submit_suggestions` return path; CLI remains the push fallback | M7 usability pass | 09 |
| RK11 | **L2 swap thrash** (alternating voice/VLM demand under 7B) | Low | Med | Min-residency debounce; auto-recommend L1 on detected thrash | M6 stress script | 12 |
| RK12 | **Encryption/key-wrapping lib risk** (CVE or DPAPI surface change) | Low | Med | Pinned lib, wrapped key isolates blast radius, documented recovery posture | M9 review | 13 |
| RK13 | **Browser-extension lifecycle**: Manifest V3 / store-review / cross-browser (Chrome + Opera GX) / native-messaging-host registration friction | Med | Med | One Chromium codebase across browsers + guided install | M4 (extension installs, native messaging works, exclusions honored) | 10, 13 |
| RK14 | **Broad extension host access (privacy surface)**: broad host permission with narrow use + empty default exclusions | Med | Med | URL + position use only (never page DOM/content); exclusions/incognito gating; install-time disclosure; `url_pattern` + "exclude this domain" bubble action; residual exposure **accepted** (Q61) with docs reworded honestly (ADR-029) | M4 | 13 |

## 2. Decision thresholds to validate on real hardware (single list)
Projected-VRAM cap **7.0 GB** · idle-unload **60 s** (60–120 range) · VLM wake rate **adaptive ~3–10/h** · τ_conf **0.7** · min support **3** · suggestion cap **adaptive 2→8/h**, cooldown **30 min** · warm-keep **≥ 2 PTT / 5 min** · dwell **20 s** · glass surfaces **≤ 2** (+ opaque 3rd), blur **≤ 12 px** · OCR ≤ **400 ms**, embed ≤ **300 ms** · STT job priority **100**, deadlines VLM / STT **measured at M5/M6** (interim 10 s / 15 s) · payload warn **50 KB**. *All [VERIFY]; each becomes a measured value at its owning M-gate.*

## 3. Open questions (tracked, not blocking)
1. WGC border suppression availability for non-UWP callers on current Win11 builds — determines RK5's final answer. [VERIFY]
2. Exact SQLCipher-equivalent library + DPAPI wrapping API choice. [VERIFY]
3. Whisper CPU fallback real-time factor on the target Ryzen (is the fallback actually usable?). [VERIFY]
4. Claude Code CLI: current flag set, JSON output stability, and whether the large-stdin caveat persists in the shipping version. [VERIFY]
5. MCP server registration specifics for current Claude Desktop (config schema, tool-consent UX). [VERIFY]
6. Whether VS Code title-parsing is stable enough or the `code -g` CLI should be primary for the IDE connector. [VERIFY]
7. ~~Whether per-call consent (no "always allow") survives dogfood feedback or needs a scoped-allow design in v1.1.~~ **RESOLVED (ADR-026):** a scoped "always-allow" is in v1 — per app+intent, still payload-displayed + cancel-window + audited. Remaining [VERIFY]: the default cancel-window duration (3 s) at dogfood.
8. Non-English OCR language-pack coverage and its effect on pattern quality. [VERIFY]

---
> **R2 amendments applied** (see docs/19–21): ADR-025 (RK7 transport), ADR-027 (RK3/RK4 downgrades), ADR-029 + Q61 (RK14 accepted residual), ADR-030/032/033/039 (§2 threshold updates); new RK13 (extension lifecycle) and RK14 (broad host access).
