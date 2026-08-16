# Doc 24 — Owner Decisions: Claude Code Implementation Brief

*Generated 2026-08-16 from a live decision session with Rajeev, working through Doc 23's findings as clickable multiple-choice questions. **No code was touched in that session** — this doc is the complete decisions log, written to be handed to a Claude Code session (which will have no memory of the conversation that produced it) so it can plan and execute the work directly.*

**How to use this doc:** each item has a decision, the reasoning/context behind it, exact file citations from the audit that produced Doc 23, and what needs to change. Citations are pointers, not guarantees — the codebase has moved since the audit (2026-08-16); re-locate before editing. Items marked **No action** are explicit "leave it" calls — don't touch them without asking again.

---

## Sequencing — read this before picking a task

Rajeev's own priority call (decision #4 below): **harden v1 before touching v2's real executor.** Suggested order:

1. Re-verify the Aug-14 review's 7 HIGH / 11 MEDIUM fixes on the actual installed app (decision #2) — this is pure QA, do it first since everything else assumes v1 is trustworthy.
2. Work through the v1 hardening items in categories A–J below (bubble UI, pattern engine, privacy, voice, vision, capture, connectors, reasoning gateway, orchestration).
3. Build screenshot redaction (decision #3) and the SC5 zero-egress proof test (decision #1) — both are trust-invariant gaps, not features; treat as high-value even though they weren't given an explicit deadline.
4. Only then start wiring the real v2 `action-executor` backend, using category K's decisions as the spec for confirmation policy, guardrails, and UI.

---

## 0. Priorities on known issues

**#1 — SC5 zero-egress proof test.** `gates/tests/sc5_network_monitor.rs` is 100% `todo!()`-stubbed and `#[ignore]`'d — nothing automatically proves "zero bytes leave until an approved Send," Aperture's core privacy claim, at the integration level.
**Decision:** No scheduling call made — Rajeev wants this documented, not decided on live. Treat as an open, unprioritized backlog item; use the sequencing note above (step 3) as the default unless told otherwise.
**Where to look:** `gates/tests/sc5_network_monitor.rs` (all helpers `todo!()`); `reasoning-gateway/src/lib.rs` (TODO on the CI lint that should statically block sockets/spawns outside this crate).

**#2 — Re-verify Aug-14 fixes on the installed app.**
**Decision: Verify on the installed app before trusting it.**
**What to do:** rebuild the NSIS installer from the current working tree, do a fresh install, and manually re-check at minimum: the `aperture_search_history` oracle-leak fix (`src-tauri/src/mcp_bridge.rs`), the SQLCipher migration crash-safety fix (`crates/db/src/lib.rs:137` area), the MCP "Approve for Claude" self-cancel fix (`ui/src/App.tsx:223` area), the sidecar-orphan-VRAM fix (`crates/orchestration/src/model_lifecycle.rs`, Job-Object tree-kill), and the STT body-size-limit fix (`crates/stt-host/src/main.rs:356` area).

**#3 — Screenshot redaction.** Text payloads get a 6-rule redaction pass; screenshots (opt-in enrichment) get none at all — `crates/privacy/src/redaction.rs:167-197`, `redact_payload` explicitly skips `PayloadItem::Screenshot`.
**Decision: Build image redaction before it ships wider.**
**What to build:** automated scrubbing for screenshot payloads before they're previewable/sendable — options include blurring likely-sensitive regions, or an OCR-then-redact-then-recompose pass reusing the existing text-redaction rules. Land this before promoting screenshot enrichment out of its current disabled/"(v2)" state in `ContextPreviewPanel.tsx`.

**#4 — v1 vs. v2 focus.**
**Decision: Harden v1 first.** See Sequencing above. Category K's decisions are still valuable as a locked-in spec for *when* v2 work starts — just don't start it yet.

---

## A. Bubble UI & lifecycle

**#5 — Bubble slot promotion.** Currently pure confidence-sort; a stale-but-confident queued bubble can block a fresher one forever.
**Decision: Factor in freshness too.**
**Where:** `ui/src/state/bubbleLifecycle.ts`, `admit()` (~L108-137) — sorts strictly by `spec.confidence`.
**Build:** a combined freshness×confidence score for slot admission, replacing the confidence-only sort. (The code's own inline comment already flags this as a placeholder.)

**#6 — Glass (2) / opaque (1) split among the 3 visible bubbles.**
**Decision: No action — keep it.** `ui/src/state/glassBudget.ts`, ADR-039. Leave as-is.

**#7 — Bubble dwell time (currently hardcoded 20s).**
**Decision: Make it user-adjustable.**
**Where:** `ui/src/state/bubbleLifecycle.ts` `DEFAULTS.dwellMs`; `config/settings.default.json` has a `bubble_dwell_sec` key that may not be wired to this constant.
**Build:** a Dashboard control (Settings/Advanced) that actually changes this at runtime, not just a static default.

**#8 — "Mute this pattern" / "Exclude this app" bubble-menu items (currently no-ops, just dismiss).**
**Decision: Wire them for real.**
**Where:** `ui/src/components/Bubble.tsx` (menu handlers); `src-tauri/src/commands/mod.rs` `record_feedback` (muted path, ~L558-564) already jumps to a 7-day mute — that half works. "Exclude this app" needs `BubbleSpec` extended to carry source-process/URL metadata, then a call into the exclusion-list machinery (`crates/capture/src/exclusion.rs`).

**#9 — 👍/👎 thumbs placement.**
**Decision: No action — retroactive-only (Dashboard Suggestions tab) is enough.** Don't add it to the live bubble.

**#10 — Per-monitor independent bubble/HUD state.** Confirmed bug: dismissing a bubble on one monitor leaves an identical, clickable copy live on others, including a duplicate "Ask Claude" session.
**Decision: Fix it — share state across monitors.**
**Where:** `src-tauri/src/overlay.rs` (each monitor gets a fully independent overlay window/React root); `ui/src/App.tsx`.
**Build:** a single shared bubble/voice-surface state (e.g. one canonical state broadcast via Tauri events to every overlay window) instead of N independent copies.

---

## B. Click-through & overlay window mechanics

**#11 — Hit-test feel (currently ~30Hz Rust-side poller, 8px rect padding, whole-window click-accept toggle).**
**Decision: Needs tightening.**
**Where:** `src-tauri/src/hit_test.rs` (`RECT_PAD`, poll interval); `ui/src/state/useHitTestRects.ts` (250ms fallback poll).
**Build:** profile against fast mouse movement across a bubble edge; likely candidates are a faster poll interval, reduced rect-publish latency, or per-pixel (not window-level) hit testing if window-level proves too coarse.

**#12 — First-run consent modality (currently fully blocks the whole monitor — no click-through, OS focus stolen — for the entire flow).**
**Decision: Make it less blocking.**
**Where:** `ui/src/components/FirstRunConsent.tsx`; `ui/src/state/useModalSurface.ts` (`exclusive: true` is what's forcing full-monitor capture).
**Build:** switch first-run to a non-exclusive modal (like Dashboard/PrivacyPanel/ContextPreviewPanel already are), or otherwise reduce how much of the screen it locks up.

**#13 — Primary-monitor-only controls (HUD, Dashboard, Privacy panel, Context Preview only render on the primary display).**
**Decision: Fix it — controls should follow the user, not just live on the primary monitor.**
**Where:** `src-tauri/src/overlay.rs` (the "overlay" primary-monitor label gates which window gets these components).
**Build:** make control-surface routing monitor-aware — e.g. summon controls on whichever monitor currently has focus/cursor, not hardcoded to primary.

---

## C. Pattern engine & proactivity tuning

**#14 — Thresholds (≥3 observed returns, ≥0.7 confidence).**
**Decision: No action — leave as-is, see how it feels with real use** (now that the decode bug that blocked all bubbles is fixed).
**Where:** `crates/pattern-engine/src/trigger.rs`.

**#15 — Pure window-focus patterns (never bubble today, by design — only patterns ending in a resumable connector state qualify).**
**Decision: Add a lighter "switch to X" affordance** for non-resumable patterns, rather than leaving them fully silent.
**Where:** `crates/pattern-engine/src/trigger.rs` rule 3 (requires `connector_state`).
**Build:** a new, lighter suggestion type/bubble variant for "you keep switching to X" that doesn't require a resumable state — likely just a "switch to X now" action rather than a full state-resume.

**#16 — Temporal (time-of-day) patterns — mined and stored, never used to trigger anything.**
**Decision: Wire it up for v1.**
**Where:** `crates/pattern-engine/src/temporal.rs` (exists, never called from the trigger path per the review-findings note).
**Build:** call the temporal-matching logic from the trigger/scoring path so time-of-day patterns can actually fire bubbles.

**#17 — Pattern engine settings tunability (currently hardcoded constants; `pattern_engine` block in `config/settings.default.json` is unread).**
**Decision: Yes, make them tunable.**
**Where:** `crates/pattern-engine/src/config.rs`.
**Build:** load `pattern_engine` settings at startup/config-reload instead of using compile-time constants; expose relevant knobs in a Dashboard Advanced panel.

**#18 — Two independent, uncoordinated pattern-prune processes (engine's decay-based prune vs. DB's age-based nightly prune, different timers, can disagree).**
**Decision: Unify into one source of truth.**
**Where:** `crates/pattern-engine/src/feedback.rs` (`prune()`); `crates/db/src/retention.rs` (`run_nightly_prune`).
**Build:** consolidate to a single prune policy/owner — likely have the DB-level retention job be the sole executor, with the pattern-engine's in-memory decay feeding it a "safe to prune" signal rather than pruning independently, or vice versa. Pick one and remove the other's independent timer.

**#19 — Adaptive suggestion-frequency cap on ignored/expired bubbles.**
**Decision: No action — only explicit feedback (click/dismiss/thumbs/mute) should move the cap, not passive expiry.**
**Where:** `crates/pattern-engine/src/lib.rs` (`apply_feedback`), `crates/pattern-engine/src/trigger.rs` (`adapt_cap`). Leave as-is.

---

## D. Privacy, consent & the transparency gate

**#20 — Default exclusion list (ships completely empty; onboarding scan only covers 2 folders, 1 level deep).**
**Decision: Bake in sensible defaults** (password managers, banking apps/domains pre-excluded) rather than relying solely on empty-by-default + weak scan coverage.
**Where:** `crates/capture/src/exclusion.rs` (default rule set, currently empty per ADR-029/Q15); `crates/privacy/src/detect_suggest.rs` (the weak onboarding scan, unaffected by this decision but still worth widening separately if revisited).
**Build:** seed a curated default exclusion list (common password managers — 1Password, Bitwarden, LastPass, KeePass — and generic "banking"-style URL patterns) shipped out of the box, on top of (not instead of) the onboarding suggestion flow.

**#21 — ADR-026 scoped "always-allow" (locked as a v1 decision on paper, never built, settings keys quietly deleted, punted to v2).**
**Decision: No, v2-only is fine — but document the reversal properly.**
**Build:** amend `docs/00-README.md`'s locked-decisions list (item E) and `docs/19-refinement-adrs.md` to explicitly state scoped-allow is v2-only, rather than leaving a v1 "locked" decision on paper that nothing implements. No code change needed beyond what's already been done (keys deleted); this is a documentation-accuracy fix.

**#22 — Retention periods (events 90d / OCR text 30d / voice 30d / suggestions+patterns 180d / audit 30d).**
**Decision: No action — keep current defaults.**
**Where:** `crates/db/src/retention.rs` (`RetentionPolicy` defaults).

**#23 — "Purge All" button wording vs. actual behavior (keeps exclusions, consent, and 30 days of audit log despite the "purge everything" label).**
**Decision: Change the button's wording** — keep the current behavior (it's already disclosed in-panel), just make the label honest.
**Where:** `ui/src/components/PrivacyPanel.tsx` (button copy). Something like "Purge history" instead of "Purge all"/"Purge everything."

**#24 — Secret-detection redaction rule coverage (currently AWS keys, OpenAI-style tokens, PEM headers, JWTs only).**
**Decision: Yes, expand the rule set.**
**Where:** `crates/privacy/src/redaction.rs:106-141` (built-in rule set).
**Build:** add GitHub PATs (`ghp_…`), Slack tokens (`xox…`), generic `Authorization: Bearer …` headers, and full SSH private-key bodies (today only the PEM header line is matched).

---

## E. Voice / push-to-talk

**#25 — STT is CPU-only (whisper.cpp `base.en`); the GPU (faster-whisper) path from the original build plan was never integrated.**
**Decision: Worth building GPU STT.**
**Where:** `crates/stt-host/src/main.rs`; ADR-024 (documents the CPU/GPU split decision).
**Build:** integrate a CUDA-accelerated whisper path (faster-whisper or whisper.cpp CUDA build) as a real, spawnable alternative — update `config/settings.default.json`'s `stt_model` handling to support both once GPU is real.

**#26 — VAD/hotkey thresholds tuned only against one dev laptop's mic.**
**Decision: Yes, validate against my real setup.**
**Where:** `crates/voice/src/vad.rs` (RMS thresholds, with comments admitting two earlier values were wrong on real hardware).
**What to do:** a validation/tuning pass against Rajeev's actual microphone and input-gain settings before treating the current thresholds as trustworthy defaults.

**#27 — Voice confirm-before-acting confidence cutoff (currently a hardcoded 60%).**
**Decision: Make it user-adjustable.**
**Where:** `crates/voice/src/lib.rs` (0.6 threshold); needs a settings key + Dashboard Voice-tab control.

**#28 — Max PTT hold duration (30s hard cutoff, currently silent).**
**Decision: Keep 30s, but add a warning as it approaches.**
**Where:** `crates/voice/src/lib.rs` / `src-tauri/src/voice.rs` (`MAX_UTTERANCE` handling); `ui/src/components/VoiceSurfaces.tsx` (listening state UI).
**Build:** a visual (and/or audio) cue in the listening pill as the hold approaches the 30s ceiling.

**#29 — Voice escalation ("Ask Claude" from a voice answer) payload richness — currently transcript-only, a stub.**
**Decision: Needs richer context before it's useful.**
**Where:** `crates/voice/src/lib.rs`, `EscalationDraft` (TODO(M7) stub, ~L107-111).
**Build:** the real `ContextPayload` builder for voice escalation — should include relevant screen context (recent OCR/event trail), not just the raw transcript, so it goes through the same transparency-gate preview as other payloads.

---

## F. Vision, OCR & VLM

**#30 — VLM model distribution (weights only work via manual hardlinking on the original dev machine; fresh installs silently degrade to OCR-only).**
**Decision: Build a real first-run download flow.**
**Where:** `models/` directory structure; `crates/embedding/examples/fetch_model.rs` is the precedent to mirror (the embedder already auto-downloads).
**Build:** a fetch-with-progress flow for the ~3GB VLM weights (Qwen2.5-VL-3B), triggered on first VLM use or during first-run setup, with a visible progress indicator — not a silent degrade.

**#31 — VLM wake-budget enforcement (documented hard ceiling of 10/hour; not actually enforced in the vision-ocr crate itself).**
**Decision: Yes, confirm/build real enforcement.**
**Where:** `crates/vision-ocr/src/vlm_gating.rs` (`WAKES_PER_HOUR_CEILING`, comment says enforcement lives in orchestration); check `crates/orchestration/src/tier_router.rs` and `telemetry.rs` for whether it's actually wired there, and close the gap if not.

**#32 — VLM confidence-label trust (string labels "high"/"medium"/"low" mapped to fixed numbers; model output is sometimes non-conforming).**
**Decision: Treat as a loose hint only — never let it drive automated decisions.**
**Where:** `crates/vlm-host/src/main.rs`, `parse_scene()`. Audit downstream consumers (pattern-engine, suggestion-generator) to confirm none of them gate a hard decision purely on this value; if any do, loosen that coupling.

**#33 — Binary payload encoding (screenshots/audio ride as JSON number arrays over local HTTP; a real body-size bug was patched by raising the limit, not fixing the encoding).**
**Decision: Worth fixing properly.**
**Where:** `crates/vlm-host/src/main.rs`, `crates/stt-host/src/main.rs` (JSON-array transport); the STT-to-whisper-child leg already uses multipart form upload as a working precedent.
**Build:** switch the orchestration→sidecar-host leg to raw bytes or multipart, matching the pattern already proven elsewhere in the same codebase.

---

## G. Capture & exclusions

**#34 — Private/incognito-window detection (hardcoded, English-only title-suffix list).**
**Decision: No action — not relevant right now.**
**Where:** `crates/capture/src/exclusion.rs:280-287`. Leave as-is unless non-English locale use becomes relevant later.

**#35 — Window-identity cache (clears entirely at 512 entries instead of evicting oldest; silently drops close-events for pre-existing windows right after a clear).**
**Decision: Yes, fix to a proper LRU.**
**Where:** `crates/capture/src/lib.rs`, `on_hook_event` (~L232-244), `identity_cache`.
**Build:** replace the clear-at-512 behavior with real LRU eviction so close-events aren't lost during high window/tab churn.

---

## H. Connectors & deep-link resume

**#36 — YouTube "position unknown" fallback (always "reopen from the start," no smarter heuristic).**
**Decision: Build a smarter fallback.**
**Where:** `crates/connectors/src/youtube.rs` (rung-3 fallback).
**Build:** a heuristic estimate (e.g. seek back some amount from the last-observed watch time) instead of always defaulting to position 0 — while keeping an honest fallback for when even that's not available.

**#37 — Staleness TTL (flat 7 days for every connector type).**
**Decision: Yes, vary by connector type.**
**Where:** `crates/connectors/src/youtube.rs`, `staleness_ttl()` (hardcoded 7-day constant) — likely needs to move from a fixed constant to a per-connector-type value on the `Connector` trait.
**Build:** shorter TTL for video (goes stale fast), longer TTL for documents/IDE files (arguably stays resumable much longer).

**#38 — Next connector priority.**
**Decision: Yes, Slack/Teams/Discord next** (communication-app threads), confirming Doc 10 §1's existing flag as the v2 expansion target.
**Where:** `crates/connectors/src/lib.rs` (`Connector` trait — will likely need extending to accommodate thread/channel deep-links, per Doc 10's existing note).

---

## I. Reasoning gateway & Claude integration

**#39 — MCP (Claude Desktop) as the primary, pull-only transport.**
**Decision: It's friction — lean on CLI/API instead.**
**Where:** `crates/reasoning-gateway/src/transports/{mcp.rs,cli.rs,api.rs}`.
**What to do:** consider changing the practical default transport away from MCP (or making it trivially easy to switch), given the pull-UX friction Rajeev is actually feeling day to day. This reverses ADR-025's MCP-primary framing — worth a doc amendment (Doc 19/09) if this becomes the real default.

**#40 — Oversized-payload handling.**
**Decision: No action — hard error is fine, no auto-shrink.**
**Where:** `crates/reasoning-gateway/src/payload_builder.rs` (`truncate_oldest_first`, `BuildError::Oversized`). Leave as-is.

**#41 — Audit-write failures (currently silent — logged to debug only, send still succeeds).**
**Decision: Yes, surface it in the UI.**
**Where:** `crates/reasoning-gateway/src/lib.rs`, `send_with_preview` (audit-write-failure path, ~L189-195).
**Build:** a UI-visible warning (e.g. a Tauri event → toast/banner) when an audit write fails, since the audit log is the sole record of what left the machine — keep the non-fatal behavior (don't block the send), just make the failure visible.

**#42 — Transport size ceilings (no real hard limits enforced; only a 50KB soft warning).**
**Decision: Yes, figure out and enforce real limits.**
**Where:** `crates/reasoning-gateway/src/payload_builder.rs` (`PAYLOAD_SIZE_WARN_BYTES`).
**Build:** research actual CLI stdin limits and Messages API request-size limits per transport, and enforce them as hard caps (distinct from the existing 50KB soft warning).

---

## J. Orchestration & GPU/VRAM budget

**#43 — VRAM ceiling (fixed 7.0GB, doesn't adapt to installed GPU).**
**Decision: Should auto-scale to installed GPU.**
**Where:** `crates/orchestration/src/budget_enforcer.rs` (`PROJECTION_CEILING_GB = 7.0`, hardcoded).
**Build:** detect actual available VRAM at startup (e.g. via `nvidia-smi` or the NVML API) and derive the ceiling dynamically, rather than a fixed constant tuned only for the RTX 5060.

**#44 — Budget-exceeded behavior (refuse-and-notify only, never queue/retry).**
**Decision: No action — refuse-and-notify is fine.**
**Where:** `crates/orchestration/src/budget_enforcer.rs`. Leave as-is.

**#45 — Shared VLM/STT lifecycle lock (one mutex guards both slots; a slow VLM cold-load can stall an unrelated STT request).**
**Decision: Worth splitting.**
**Where:** `crates/orchestration/src/model_lifecycle.rs` (`acquire_endpoint`, single `TokioMutex<ModelLifecycle>`).
**Build:** split into per-sidecar-kind locks so a VLM cold-load can never block an incoming STT request, protecting the "voice never waits" priority guarantee end-to-end (not just at the scheduler level, which already handles execution-order priority correctly).

**#46 — Whisper VRAM re-measurement (seeded estimate ~2GB vs. doc's original ~1GB assumption).**
**Decision: No action for now — not urgent.**
**Where:** `crates/orchestration/src/budget_enforcer.rs` (`VramTable` seed values). Leave as a known-drift flag; revisit if VLM+STT co-residency issues show up in real use.

---

## K. The v2 agent execution layer (spec for when v1 hardening is done)

**#47 — Agent action confirmation policy.**
**Decision: Confirm only risky-looking actions** — routine actions (click/type/scroll/wait on non-flagged targets) run without asking; anything that looks destructive/consequential (delete/send/pay/purchase/overwrite-type actions) always stops for confirmation regardless of Claude's self-reported confidence.
**Where:** `crates/contracts/src/agent.rs` (`ActionType`, `AgentConfidence`); `crates/agent-loop` (loop driver, not yet built).
**Build:** (a) implement the currently-missing confidence-gated pause — `TaskStateMachine::apply_instruction` today ignores `instruction.confidence` entirely (Finding F4 from the audit); (b) add a keyword/heuristic check on the action's target/label for consequential-looking actions, independent of Claude's confidence, per decision #51 below.

**#48 — v2 transparency model (approve once per task, not a forced preview per step; full payload viewable on demand).**
**Decision: Acceptable — friction reduction is fine.**
**Where:** `crates/agent-loop/src/lib.rs` §4.1 design note (reuses v1's `previews.approved` store, once per task).
**Build:** proceed with the once-per-task approval design as documented — no forced per-step preview needed. Still worth surfacing a persistent "Aperture is active" indicator per decision #53.

**#49 — Exclusion-list hit mid-task.**
**Decision: Pause and notify.**
**Where:** `crates/action-executor/src/lib.rs` — currently `ActionExecutor::execute()` has no `ExclusionList` parameter at all (Finding F3 from the audit — documented as a guardrail, not wired into the trait).
**Build:** add the exclusion-list check into the executor (or the loop driver immediately before dispatching to the executor), and implement pause-and-notify as the behavior when the active window matches an exclusion rule.

**#50 — UAC-elevated windows.**
**Decision: Offer a run-as-admin prompt** (not a hard block) — accepting the added privilege/risk this implies.
**Where:** `crates/contracts/src/agent.rs` (`ActionError::Elevated` exists as a variant but is never produced); `crates/action-executor` (`UiaExecutor` stub never reaches elevation detection).
**Build:** implement UAC/elevation detection in the executor, and a flow that explicitly surfaces a "this needs admin — allow?" prompt to the user rather than silently failing or silently elevating.

**#51 — Click risk tolerance for consequential actions.**
**Decision: Yes, add a stricter check for risky-looking actions**, on top of Claude's own confidence signal.
**Where:** `crates/action-executor` (fuzzy UIA label matching, Levenshtein ≤2, with a pixel-coordinate fallback — the primary grounding mechanism today).
**Build:** an allow-list/keyword-style secondary check (e.g. flag targets whose label matches delete/send/pay/purchase/confirm-style patterns) that forces the confirmation path from decision #47, independent of what Claude reports.

**#52 — Agent action scope (UI-only vs. filesystem/shell/network access).**
**Decision: Screen and keyboard only, forever** — a hard line, not a v2.0 starting point to expand later.
**Where:** `crates/action-executor/src/lib.rs` (already documents "no filesystem writes, no registry, no network, no shell" as the intended invariant — this decision confirms it as permanent, not provisional).
**Build implication:** resolves Finding F12 from the audit (the `ActionType::Launch` ambiguity) — `Launch` must be implemented as a simulated UI interaction (e.g. clicking a Start-menu tile via UIA), never as a direct process-spawn call, to stay consistent with this hard line. Code the "no shell" invariant as an actual impossibility (e.g. no process-spawn API reachable from `action-executor` at all), not just a documented policy someone could accidentally violate later.

**#53 — In-progress agent UI.**
**Decision: Persistent status bar + live step-by-step log** (not just a minimal pill).
**Build:** a new UI surface — likely following the pattern of `ui/src/components/VoiceSurfaces.tsx` — that's always visible while a task runs, showing a scrollable transcript of steps, fed by `crates/task-manager/src/lib.rs`'s `record_step` data. Should make it unambiguous to the user that Aperture is actively driving the mouse/keyboard right now.

**#54 — Agent undo/rollback after hard stop.**
**Decision: Worth building some undo**, at least for reversible actions.
**Where:** `crates/action-executor`, `crates/agent-loop` — no rollback mechanism exists today; hard stop currently only prevents further steps.
**Build:** classify action types by reversibility (e.g. "closed an app it opened" is reversible; "sent a message" is not) and implement rollback for the reversible subset. Don't promise undo for actions that fundamentally can't be undone — be explicit in the UI about which is which.

**Also fold in from the audit, since v2 work is now confirmed as a real future target:**
- **Finding F2** — `ExecutorTicket::for_task` has no actual capability-token enforcement; any crate that links `action-executor` can mint a valid ticket today. Harmless while `UiaExecutor` is a stub, but must be fixed (real capability/marker type that only `agent-loop` can construct) before the real backend lands.

---

## L. Design system (Chromemorphism / Liquid Meta)

**#55 — Current near-opaque glass look (12px blur cap, α≥0.96 fills) vs. the original "glassmorphism" concept.**
**Decision: No action — happy with it as-is.**

**#56 — "Liquid" refraction/distortion effects (deferred out of v1 entirely; static glass only for now).**
**Decision: Yes, still want it eventually.** Keep it on the backlog as a future polish pass — not scheduled now, but don't drop it.

---

*End of decisions log. 56 decisions total across 4 priority calls + 12 subsystem categories. Doc 23 (the source questions with fuller context/citations) and this doc should be read together by whoever picks up implementation.*
