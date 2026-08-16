<!-- Handoff/process doc (not an architecture doc). Bridges the session that
     executed Doc 24's owner decisions (batches 1-3 of a planned 5). Supersedes
     the SESSION END STATE of session-bridge-2026-08-15-hardening-v2-skeleton.md.
     Authoritative design: Docs 00-22. Decisions log: Doc 24 (read with Doc 23). -->

# 🔄 CLAUDE SESSION BRIDGE — Doc 24 execution, batches 1–3 — read this first

**Session date: 2026-08-16**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`
**Commits this session:** `342a1e4` (08-15 working tree checkpointed: 31 review fixes, v2 skeleton, Docs 23/24) → `f1954ff` (this session's work). Both pushed; remote is current.

Apply **"How this user works"** from `session-bridge-2026-08-09-m9.md` from your very first line. Read `docs/24-owner-decisions-claude-code-handoff.md` (with Doc 23 for context) — it is the master task list this session executed against; this bridge records which items are DONE and which remain.

---

## The headline

**20 of Doc 24's actionable decisions are implemented, tested, and pushed.** Executed as three multi-agent batches on disjoint file clusters, each agent verifying current code before editing (several Doc 24 citations were stale — see Findings). Verification at session end: `cargo test --workspace` green (51 suites, ~90 new tests this session), `tsc --noEmit` clean, `cargo run -p xtask -- lint-emitters` OK, zero compiler warnings.

Sequencing followed Doc 24's own order: v1 hardening first. **v2 executor work has NOT started** (per decision #4). The trust-gap items (#3 screenshot redaction, #1 SC5 test) are NOT done yet — they are batch 5, below.

## What was done, by decision

### Batch 1 — backend clusters
- **#24 secret detection** (`privacy/redaction.rs`): added GitHub classic PATs (`ghp_/gho_/ghu_/ghs_/ghr_`) + fine-grained `github_pat_`, Slack `xox[abprs]-`, `Authorization: Bearer` headers (case-insensitive, RFC 6750 token class, 16-char floor), and full **multi-line PEM/OPENSSH private-key bodies** (ordered before the retained header-only fallback; one placeholder per key). Additive within rule-kind 1; placeholder convention unchanged.
- **#20 default exclusions** (`capture/exclusion.rs` + `src-tauri/main.rs::seed_default_exclusions`): 16 curated rules (1Password, Bitwarden, KeePass/XC, LastPass, Dashlane, Proton Pass process+web-vault rules; banking url_patterns; "online banking" title_regex) seed ONCE into the durable `exclusion_list` table, guarded by settings flag `exclusion_defaults_seeded`. Seeded rows appear in the Privacy panel, are disable/deletable, and **never resurrect** (interrupted-seed retry is idempotent and can never re-enable a user-disabled rule — `Db::add_exclusion_rule` re-enables on re-add, so `defaults_needing_seed` filters rows present-even-if-disabled). `ExclusionList::shipped_defaults()` deliberately stays EMPTY (a compiled-in list could never be deleted). Additive-only; any read error → log + retry next launch, never fail open.
- **#23** PrivacyPanel copy: "Purge everything/Purge all" → **"Purge history"** (behavior unchanged; retained-data disclosure kept).
- **#21** docs/00 locked-decision E + ADR-026 amended: scoped always-allow is **v2-only**, descoped from v1. ADR-029 also amended for #20. docs/13 §4 + settings comment updated to match.
- **#35 LRU identity cache** (`capture/lib.rs`): clear-at-512 replaced with true LRU (HashMap + monotonic touch counter, evict-oldest at capacity); close-events for still-cached windows always resolve.
- **#17 pattern engine reads settings**: new `EngineConfig::from_settings` (range-validated; typos can never weaken the engine — falls back per-field to constants). Loaded at pattern-task start + re-read on the **daily maintenance tick** (no push-reload path exists in the shell yet — see Leftovers).
- **#16 temporal patterns fire**: `temporal.rs` wired into the trigger path; histogram mass now DECAYS with the configured half-life (was grow-only — dead habits stayed formed forever); temporal rows ride the normal signature grammar so flush/hydrate/feedback/mute work unchanged. Same 7-rule gate as sequence patterns.
- **#15 "Switch to X" bubbles**: app-focus consequents (rule-3-exempt ONLY — all other gates apply) emit lighter candidates via a sentinel `app-focus:` connector_id; pipeline synthesizes a persisted, FK-valid `app_focus` connector_state row (24 h TTL); `bubble_click` Path B resolves it via `ShellExecuteW('open', …)` through the connectors crate's single dispatch primitive. Needs target app observed since app start (in-memory `last_process` map — see Leftovers).
- **#18 prune unified**: the engine's decay prune is the SOLE deleter of pattern rows; retention's competing age-based prune removed. **Latent FK bug found & fixed** (see Findings).
- **#43 VRAM ceiling auto-scales**: `startup_projection_ceiling_gb()` — one `nvidia-smi --query-gpu=memory.total` at startup (OnceLock, CREATE_NO_WINDOW), ceiling = total − 1 GB headroom clamped [2.0, 31.0], falls back to 7.0. Wired via `OrchestratedSystem::with_runner`. ADR-030's locked 7.0 is reversed by owner decision #43 (code comments cite it; doc amendment still owed — Leftovers).
- **#45 per-kind lifecycle locks**: `ModelLifecycle` restructured — per-kind (VLM/STT) TokioMutexes + committed-resident StdMutex index + kind-level warm-keep AtomicBools; a VLM cold-load can never block an STT acquire (regression tests that deadline out under the old locking). All public signatures preserved verbatim. NOTE: warm-keep pins now survive sidecar reloads (small intentional behavior change, matches ADR-030/Q36 intent).
- **#31 VLM wake budget**: **verified already enforced** — `tier_router.rs` trailing-hour ledger → `Skip(RateCeiling)`, wired at the ONLY runtime VLM wake path (`pipeline.rs maybe_enrich_vlm`). No gap. Doc 23's premise was wrong here.
- **#36 YouTube estimate fallback**: new rung 3 — bounded in-memory last-known-position map (fed by real observations only), rewind 30 s, persisted with additive `PositionSource::Estimated`; honest "from the start" stays as rung 4. In-memory per app run.
- **#37 per-type staleness TTLs**: youtube 7 d → **3 d**, document/IDE 7 d → **30 d**, browser stays 24 h (was already differentiated — Doc 23's "flat 7 days" was partially stale; the trait method already existed, only values were flat). TTL-ordering pinned by a lib.rs test.

### Batch 2 — gateway/voice + hosts
- **#27 voice confirm floor**: `voice.intent_confidence_floor` read per-utterance from settings (clamped [0,1]; 1.0 = true "always confirm" — the chip's Run path re-classifies at 1.0 so it can't loop), Dashboard Voice-tab slider (merge-writes the voice section so `ptt_hotkey` survives).
- **#28 PTT cutoff warning**: the 10 Hz listening emit now carries `elapsed_ms`/`max_ms` from the SAME loop that enforces the 30 s cutoff; pill shows "auto-stop in Ns" with urgent styling in the last 5 s. Audio earcon NOT built (no audio-output path exists; noted in Doc 24 as "and/or").
- **#29 voice escalation payload**: "Ask Claude" from a voice answer now ships the clean query (words after "ask claude") + up to 3 newest non-excluded (`redaction_flags=0`) OCR snapshots (2000 chars each) — assembled shell-side (two-emitter boundary), riding the EXISTING build→redact→preview→approve(re-redact)→send gate. No new egress path; regression test proves excluded rows can't leak.
- **#41 audit-write failure surfacing**: `send_with_preview` returns `SendOutcome { suggestions, audit_failure }`; push path emits an `audit_alert` event → stacked, dismissible, never-auto-hiding banner ("reached Claude but was NOT recorded in the audit log"). MCP path keeps its fail-CLOSED behavior (nothing releases) and now also tells the user why.
- **#42 transport hard caps**: enforced BEFORE any egress on the actual wire bytes (tripwire test proves zero egress on violation): **API 20 MB** (Messages API rejects >32 MB bodies; base64 4/3 + envelope margin), **CLI 10 MB** (the repo's own doc 09 §3 number — chosen over the 16 MB example; one-constant change if you want it bigger), **MCP 1 MB** (get_context release gate; hard stop restores session+approval). At-cap passes, cap+1 refuses naming transport/size/cap. 50 KB soft warning unchanged. No auto-shrink (decision #40).
- **#33 multipart sidecar transport**: orchestration `SidecarRunner` now sends multipart/form-data (VLM: raw `image_jpeg` + text `prompt`/`schema`; STT: raw `wav`) and both hosts decode it; JSON-number-array bodies are GONE. Byte-identical round-trip tests on both hosts + a loopback fake-host test in orchestration. **Clean wire break — see the rebuild warning below.**
- **#32 VLM confidence audit**: full-workspace audit found NO consumer gating an automated decision on the VLM confidence label — evidence chain (producer `vlm-host parse_scene` → orchestration passes opaquely → vision-ocr clamps only → pipeline persists verbatim → UI treats as opaque string; pattern-engine's `confidence` is its own mined stat). Documented at the mapping site + coercion regression test. No code loosening needed.

### Batch 3 — VLM distribution + overlay mechanics
- **#30 VLM weights download**: new `orchestration/model_fetch.rs` (the ONE sanctioned non-loopback reqwest exception besides the gateway; module doc carries the ingress-vs-egress argument) — streams to `.part`, resumes via Range, size-verifies, renames atomically. Shell: `vlm_fetch.rs` + `vlm_status`/`vlm_download` commands (explicit-click-ONLY, double-start guarded, ~8 MB-throttled progress events). Dashboard Overview: honest "VLM not installed — OCR-only mode" notice + Download (~3 GB) button + live progress + resume-aware Retry. URLs+byte sizes live in settings (`loadout.vlm_download`), **verified byte-exact against ggml-org/Qwen2.5-VL-3B-Instruct-GGUF on 2026-08-16** (model 1,929,901,056 B; mmproj 1,338,428,128 B — matches dev machine). Key design: `sidecar_config()` resolves absent weights to the download destination and the lifecycle re-attempts spawn per job + clears the Degraded latch, so a finished download is live on the NEXT VLM use — **no restart**. Startup `tracing::warn` when weights missing (no more silent degrade).
- **#11 hit-test tightening**: Rust poller 30→60 Hz; React fallback remeasure 250→100 ms; animation/transition-end listeners publish rects the moment motion settles (rAF-coalesced). RECT_PAD stays 8 px with rationale; profiling checklist + per-pixel WM_NCHITTEST escalation path documented at the constants. **Owner feel pass required** (below).
- **#12 first-run consent non-blocking**: `exclusive:false` — card is clickable via its rect + takes OS keyboard focus on mount; rest of the monitor stays click-through. Verified untouched invariants: capture stays OFF until `complete_first_run`; App.tsx renders only the consent card pre-first-run; tray click still lands on it.
- **#13 controls follow the user**: Dashboard/Privacy/Preview open on the CURSOR's monitor (pure `monitor_index_at` mapping, half-open bounds; fallback primary). Exactly-one-instance across monitors via `{target}` broadcast (target opens, others close). MCP preview uses a core-side `PreviewHost` claim/release registry (requests queue behind a mid-edit panel — never clobber; `preview_claimed` broadcast folds the loser honestly). **HUD deliberately stays primary-only** (persistent/ambient, not summoned; documented in App.tsx header — routes from a secondary monitor are the tray + any bubble's ⋯ menu, both cursor-routed). `emit_preview_request` kept its signature so mcp_bridge is untouched.

### Glue work (main-loop, not agents)
`seed_default_exclusions` wiring in main.rs; settings.default.json accuracy (privacy comment, connector TTL values 3/30/30, vlm_download block landed via agent); docs/13 §4 amendment; `unused_mut` fix in `retention_policy_from_settings`; **Slack test fixture assembled at runtime** because GitHub push protection hard-blocks pattern-shaped literals (see Findings).

## Findings this session (keep these in mind)

1. **Latent FK bug (FIXED, was ticking)**: with `foreign_keys=ON`, retention's stale `connector_state` delete tripped the FK on any referenced row and **rolled back the ENTIRE nightly prune** — reproduced by test first, fixed by detaching referencing events/suggestions rows before the delete. Every app_focus row would have hit this within ~2 days; existing browser/youtube refs already could.
2. **Doc 23/24 citations are stale in places** — always re-verify: #31 was already enforced; the Connector trait already had per-type TTL (only values were flat); browser TTL was already 24 h; `intent_confidence_floor` key already existed (unread); mcp get_context already failed closed on audit failure.
3. **GitHub push protection** blocks pattern-shaped secrets in TEST FIXTURES (Slack tokens have no checksum). Assemble such fixtures at runtime (`format!` with split parts). The first batch-1-3 commit was rejected for this; history was redone via reset --soft + fresh commit `f1954ff`.
4. `cargo test -p aperture-orchestration` standalone needed the `windows` crate's `Win32_Security` feature (was compiling only via workspace feature unification) — fixed.
5. Settings seed is keyed on the `reasoning` row → **upgraded installs never receive newly added settings keys** (e.g. `loadout.vlm_download`). Code defaults mirror the seed everywhere this matters (tested), but a settings-migration pass is a real future item.
6. `vision-ocr screen_context_writer::with_vlm_summary` is still a `todo!()` stub (live path uses `db.attach_vlm_summary` in pipeline.rs) — pre-existing, noted during the #32 audit.
7. Known small race (documented, accepted): two MCP preview requests arriving before the UI registers the preview host can land on different monitors; claimed-convergence cancels the loser honestly.

## ⚠️ REBUILD REQUIRED before the next install (do not skip)

#33 is a **clean wire break** (old shell ↔ new host = 4xx; a stale host binary silently degrades VLM→OCR-only and STT→SidecarDown):
1. `cargo build --release -p aperture-stt-host -p aperture-vlm-host` and rebuild `aperture-mcp` (gateway lib changed: SendOutcome/hard caps).
2. Copy all three into `src-tauri\binaries\` under the `-x86_64-pc-windows-msvc` names.
3. `ui\node_modules\.bin\tauri.cmd build` (NEVER bare cargo build into the install dir) → installer at `target\release\bundle\nsis\`.
Schema: no new migrations this session (0003 is from the v2 skeleton, already additive).

## What's left — in order

### IMMEDIATELY NEXT: Batch 4 (specced, ready to launch — one UI-cluster agent, or two sequential)
Cluster: `ui/src/state/*` (bubbleLifecycle, glassBudget…), `Bubble.tsx`, `BubbleContainer.tsx`, `App.tsx`, `Dashboard.tsx`, `commands/mod.rs`, `contracts` (only if BubbleSpec needs metadata), `settings.default.json`, `ipc.ts`, styles.
- **#5** slot admission: replace pure-confidence sort in `bubbleLifecycle.ts admit()` (~L108-137, code's own comment calls it a placeholder) with a freshness×confidence score.
- **#7** bubble dwell: wire `ui.bubble_dwell_sec` (exists in settings, unread) to `DEFAULTS.dwellMs` + a Dashboard control that changes it at runtime.
- **#8** real "Exclude this app": extend BubbleSpec to carry source-process/URL metadata (pipeline knows it at emit time), bubble ⋯ menu calls the existing exclusion machinery. (Mute is ALREADY real since 08-15; "Exclusions…" currently just opens the panel — honest but not the decision's ask.)
- **#10** shared multi-monitor bubble state: 08-15 built dismissal convergence (`voice_dismiss`, `record_feedback` broadcasts) and today's #13 added the `{target}`-broadcast pattern + preview convergence. VERIFY what remains for bubbles specifically (are bubble stacks/queue promotion still per-monitor-independent?) and finish with the same broadcast pattern.
- **#39** transport switch: a Dashboard control making `reasoning.transport_order` trivially switchable (MCP↔CLI↔API). Owner feels MCP pull-UX friction; do NOT silently change the default — make switching easy, note the ADR-025 doc amendment if he flips it.
- **#17-UI** pattern-engine knobs: Dashboard Advanced panel exposing the (already-live) `pattern_engine` settings block via set_settings merge-writes. Optional add-on: a push-reload path (AppState plumbing) so changes apply without waiting for the daily tick.

### Batch 5 (trust items, after batch 4)
- **#3 screenshot redaction** (`privacy/redaction.rs` `redact_payload` explicitly skips `PayloadItem::Screenshot`): OCR-then-redact-then-recompose or region blur; must land BEFORE screenshot enrichment leaves its "(v2)" disabled state in ContextPreviewPanel. The new #24 text rules compose with an OCR pass.
- **#1 SC5 zero-egress proof**: `gates/tests/sc5_network_monitor.rs` is 100% todo!()/#[ignore] — build the real byte-level monitor harness (SC6's real-harness conversion on 08-15 is the precedent: measurement spawns are lint-sanctioned). Also the reasoning-gateway TODO for a CI lint statically blocking sockets/spawns outside sanctioned crates (note: `orchestration/model_fetch.rs` is now a sanctioned exception).

### Then: installer + owner QA (decision #2 — needs Rajeev at the machine)
Rebuild per the ⚠️ box, fresh install, then manually verify at minimum: the Aug-14 HIGH fixes (search-oracle constant replies; SQLCipher migration crash-safety; MCP "Approve for Claude"; sidecar tree-kill reclaims VRAM; >18 s utterances) PLUS this session's behaviors: bubbles actually firing (decode fix + temporal + switch-to-X), default exclusions visible in Privacy panel on fresh install, VLM download flow end-to-end on a weights-less machine (`cargo test -p aperture-orchestration model_fetch -- --ignored` downloads the real ~3.3 GB), PTT countdown at 25-30 s, audit banner (hard to trigger honestly — could temporarily point the audit DB somewhere read-only), **#11/#13 feel pass**: fast flicks across bubble edges at 60 Hz, dead-click halos at 8 px pad, summon-on-each-monitor convergence.

### Deferred (owner/hardware-dependent — decisions say yes, but not startable autonomously)
- **#25 GPU STT** (CUDA whisper build — toolchain + SC4 gate run), **#26 VAD/mic validation** on the real mic, SC3/SC4/PresentMon measured runs, the M5 load-times gate switch to `BudgetEnforcer::ceiling_gb()` (currently asserts the constant).

### Small leftovers (fold into any nearby session)
Doc 04/ADR-030 amendment for #43's auto-scale reversal; docs 10 §2-5 TTL values; `gates/m9_privacy.rs:329` assertion message reword (assertion itself still true/passing); Bearer-rule capture-group variant if the preview should keep the header name visible; Slack `xapp-`/`xoxe-` prefixes if wanted; pipeline `position_rank` treats Estimated == exact position (bounded −30 s drift; add a middle rank if exact should outrank); persist temporal histograms + app_class→process map across restarts (both in-memory today); VLM download cancel button; settings-migration pass for upgraded installs; suggestion-generator glyph for app_focus bubbles; UI could hide action-less resurfaced suggestions (retention FK fix nulls their connector ref).

### Decisions that are explicitly "No action — leave as-is" (do NOT re-open without asking)
#6 glass/opaque split, #9 thumbs retroactive-only*, #14 trigger thresholds, #19 passive-expiry cap, #22 retention defaults, #34 English-only incognito detection, #40 oversized hard-error, #44 refuse-and-notify, #46 whisper VRAM re-measure, #55 near-opaque look. (*Doc 23's premise was stale — thumbs were added to the live bubble on 08-15 and Doc 24 says leave-as-is, so the bubble thumbs STAY.)

## Verification state at session end
- `cargo test --workspace`: green, 51 suites, 0 failures, 0 warnings. Notables: pattern-engine 53, orchestration 53+1 ignored (real-HF download), privacy 39, voice 38, capture 33ish, gateway 31, connectors 42, shell 18.
- `tsc --noEmit` clean; `lint-emitters` OK (new [sanctioned] lines: nvidia-smi detection + model_fetch ingress, both audited).
- Remote `r2-spec-integration` = local = `f1954ff`.

## How to resume
Tell the next session: *"Read docs/handoff/session-bridge-2026-08-16-doc24-execution.md, then launch Batch 4 exactly as specced in 'What's left'."* The batch-agent pattern that worked: disjoint file clusters, each agent re-verifies current code against Doc 24's (sometimes stale) citations, targeted tests green before finishing, central workspace test + commit after each batch.
