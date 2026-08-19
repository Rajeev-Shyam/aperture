<!-- Handoff/process doc (not an architecture doc). Bridges the session that
     executed Doc 24's batch 4 (the bubble-UI cluster). Supersedes the
     "IMMEDIATELY NEXT" section of session-bridge-2026-08-16-doc24-execution.md.
     Authoritative design: Docs 00-22. Decisions log: Doc 24 (read with Doc 23). -->

# 🔄 CLAUDE SESSION BRIDGE — Doc 24 batch 4 (bubble UI) — read this first

**Session date: 2026-08-19**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`
**Previous:** `9b43ba2` (batches 1–3 bridge). Read `session-bridge-2026-08-16-doc24-execution.md` for everything before this session, and `session-bridge-2026-08-09-m9.md` for "How this user works" — apply it from your first line.

---

## The headline

**Batch 4 is done: all six of its Doc 24 decisions are implemented, tested, and green.** Executed solo (the owner asked for no multi-agent workflow this session), reading each site before editing — which is how the three real bugs below were found.

Verification at session end: `cargo test --workspace` green (0 failures, **0 warnings**), `tsc --noEmit` clean, `vite build` clean, `cargo run -p xtask -- lint-emitters` OK. 6 new shell tests, 7 new suggestion-generator tests, plus a 16-assertion scratch check of the pure admission helpers (the repo has no JS test runner; see "Verification" below for how to re-run it).

Four of the 08-16 bridge's "small leftovers" are also closed (settings migration for upgraded installs, the Doc 04/ADR-030 amendment for #43, Doc 10's TTL values, the stale m9 assertion message) — see "Small leftovers" below.

**Not done, deliberately:** batch 5 (the trust items, #3 + #1), and the rebuild/installer QA. The ⚠️ rebuild warning from the 08-16 bridge **still stands and now has one more reason** — see below.

**On batch 5's sequencing, for whoever picks it up:** `PayloadItem::Screenshot` currently has **zero producers** — nothing in the codebase constructs one, and the enrichment toggle is still the disabled "(v2)" control in `ContextPreviewPanel.tsx`. So #3 is not "add redaction to a shipping feature", it is "build the image-redaction gate so the feature can be switched on at all". Scoping note from reading it: `Windows.Media.Ocr`'s `OcrWord` **does** carry a bounding rect, but `windows_media_ocr::aggregate_lines` takes `Vec<String>` and throws the geometry away — so a real OCR-then-redact-then-recompose pass needs the `OcrEngine` trait widened to return word boxes first, plus the `image` crate in `privacy`. That is a genuine multi-crate feature with a Windows-only, hard-to-unit-test leg; budget it as its own batch rather than a tail-end task.

## What was done, by decision

### #5 — slot admission is freshness × confidence
- `BubbleSpec` gained `created_ts` (additive, `Option<i64>`), durable as **`suggestions.created_ts`** (new migration `0004_suggestion_created_ts.sql`, backfilled from `shown_ts`). `shown_ts` could not serve: it is NULL while a row is queued (snoozed), which is exactly the case that needs an age.
- `bubbleLifecycle.admissionScore` = `confidence × 0.5^(age / half_life)`; half-life from `ui.bubble_freshness_half_life_sec` (default **600 s**, clamped 10–86 400). Queued bubbles are re-scored on every admission. An absent `created_ts` scores as fresh — penalizing it would bury the whole pre-upgrade queue.
- Chosen as a decay rather than a cutoff because the hard freshness rule (past TTL ⇒ no candidate at all) already lives core-side in doc 08 §5; a second cliff would only move the arbitrariness.

### #7 — bubble dwell is a real control
- `ui.bubble_dwell_sec` now drives the countdown (it was `DEFAULTS.dwellMs`, a constant) via a `dwellMs` prop, and the Dashboard's new **Advanced** tab edits it (5–120 s).
- Live because **`set_settings` now announces its write**: it emits `settings_changed { sections }` to every window and pings `settings_reload_tx` for long-lived Rust tasks. Without that, any Dashboard control is only a next-launch preference. `dwellMs` is intentionally out of the dwell effect's dep list — a settings change must not restart a countdown the user is already watching.

### #8 — "Exclude this app" is real
- `BubbleSpec.exclusion_offers: Vec<ExclusionOffer { label, match_kind, pattern }>`, derived core-side by `suggestion_generator::exclusion_offers_for(&ConnectorState)`:
  - **browser / youtube** → the *site* (`url_pattern`, house style: `^https?://([a-z0-9-]+\.)?docs\.rs([:/?#]|$)`). Excluding the whole browser from one page's bubble is far more than the user asked for.
  - **document / app_focus** → the *app* (`process`, lowercased image name). Only when the stored value ends in `.exe` — process matching is an exact image-name match, so a bare `"chrome"` would be a rule that silently protects nothing.
  - **ide** → nothing. Its payload (`path`/`line`/`workspace`) names no process.
- Escaping lives in Rust, next to the matcher, not in the WebView. The ⋯ menu renders "Stop capturing {label}" per offer → `add_exclusion` (durable + hot-reloaded) → an inline confirmation with **Undo**. The bubble deliberately stays: excluding an app is a capture decision, not a judgment on the suggestion, and the menu must stay mounted to offer Undo. "Exclusions…" remains for everything the offers cannot express.
- **Undo restores the row's prior state, and only that** — flagged by the sibling session's `note-from-doc24-session-2026-08-19.md`: `Db::add_exclusion_rule` is idempotent on `(match_kind, pattern)` and **re-enables** a matching row instead of inserting. A blind `set_exclusion(id, null)` on Undo would therefore delete a rule the user had set up earlier and merely switched off. The click now pre-reads `list_exclusions`: already-enabled ⇒ "already excluded", no add and no Undo offered; previously-disabled ⇒ Undo re-disables; absent ⇒ Undo deletes.
- `list_suggestions` LEFT JOINs `connector_state` so a restored (post-respawn) bubble still carries its offers. LEFT, not INNER — a Claude answer bubble has no connector row and must still restore.

### #10 — multi-monitor bubble state, closed at the source
- `record_feedback` is now **exactly-once** for terminal transitions: `UPDATE suggestions SET state=? WHERE id=? AND state IN ('queued','shown')`; a 0-row update means someone already resolved it → no engine signal, no second broadcast. Extracted as `record_feedback_row` so it is testable against a real DB.
- This was a **live bug**, not a hypothetical: every monitor runs its own overlay root with its own dwell timer, so one ignored bubble reported `expired` once per screen within milliseconds of itself, and `EXPIRE_DECAY_MULT` was applied N times. Patterns were suppressed faster the more screens the owner has. The 08-15 lifecycle broadcast converges the *rendering* but races the sibling timers.
- Thumbs are guarded the same way, on the rating actually CHANGING (👍👍👍 was compounding ×1.5). They are deliberately NOT state-guarded — the Dashboard rates resolved rows, which is the point of retroactive thumbs (decision #9).
- New `suggestion_rated { id, rating }` broadcast; ratings moved from per-Bubble local state into the container's shared map, so the pressed thumb lights on every monitor.

### #39 — transport switching without a relaunch
- `AppState.gateway/push_target` → one `GatewaySlot` behind an `RwLock`; `set_settings` rebuilds it via `crate::build_gateway` when the patch touches `reasoning`. Reads clone the `Arc<Gateway>` out immediately (no guard across an `await`); an in-flight Send finishes on the transport it started with.
- Dashboard **Advanced** → radio over the push transports (Claude CLI / Messages API) with live health per transport, moving the choice to the front of `transport_order`.
- **ADR-025's MCP-primary default is UNCHANGED** — only switching got easy (docs 09 amended to say exactly this). MCP stays in the list either way: it is pull-only, `pick_healthy_transport` already skips it, and dropping it would unregister a path the switch is not about. **If the default is ever actually flipped, amend ADR-025 + doc 09.**

### #17-UI — pattern-engine knobs, applied immediately
- Dashboard **Advanced** exposes certainty (`tau_conf`), repeats-before-a-habit (`cold_start_support_floor`), quiet time (`cooldown_min`), suggestions/hour (`cap_per_hour_default`) — merge-written into the whole `pattern_engine` section.
- The **push-reload path is built** (it was listed as an optional add-on): the pattern task now selects on `settings_reload_rx` alongside its 24-hour tick, re-reading only when the write names `pattern_engine` (a Lagged receiver re-reads unconditionally). The daily tick stays as the backstop for edits made outside the app.

## Three bugs found by reading the code (all fixed)

1. **The ≤3 visible cap was violated.** `admit()` sorted the whole list and promoted anything landing in the first `maxVisible` positions, but never demoted a visible bubble — so a high-scoring arrival while 3 were on screen produced a **4th**, breaking the doc 11 §3 UX cap and the doc 14 §5 glass budget together (2 glass + 2 opaque).
2. **The queue was never drained.** Promotion lived in `removeBubble`, wired to the Bubble's `onExited` — which cannot fire: `onLifecycle("exit")` removes the bubble from the container's list, unmounting the Bubble before the effect that would call `onExited`. Invisible while bug 1 made everything visible on arrival; fatal once the cap is enforced. Both now go through one `promote()`, used by arrivals and by the freed-slot path.
3. **Glyphs rendered as words.** `BubbleSpec.glyph` is a semantic token ("video", "globe", …) that is also PERSISTED into the suggestions row, and the bubble drew it verbatim — the literal string "video" inside a 28 px chip. The mark is now chosen at render (`glyphMark`), so old rows and new ones agree; decision #15's app-focus bubbles got their own `switch` token.

Also fixed in passing: the ⋯ menu now pauses the dwell while open (it is portalled to `<body>`, so moving the cursor onto it fired the bubble's `mouseleave`, resumed the countdown, and could expire the bubble mid-decision — much worse now that the menu holds a real, consequential action), and a pre-existing `unused_mut` warning in `pattern-engine/src/lib.rs`.

## ⚠️ REBUILD REQUIRED before the next install (unchanged, plus one)

The 08-16 multipart wire break (#33) still applies **and** the UI bundle + shell both changed here:
1. `cargo build --release -p aperture-stt-host -p aperture-vlm-host`, rebuild `aperture-mcp`.
2. Copy all three into `src-tauri\binaries\` under the `-x86_64-pc-windows-msvc` names.
3. `ui\node_modules\.bin\tauri.cmd build` (NEVER bare cargo build into the install dir) → installer at `target\release\bundle\nsis\`.

**Schema:** migration **0004** is new this session (additive `ALTER TABLE suggestions ADD COLUMN created_ts`). Forward-only, applies automatically.

## Cross-session note

The sibling session (batches 1–3) left `docs/handoff/note-from-doc24-session-2026-08-19.md` with its gotcha list. Checked against this batch: no secret-shaped test fixtures were added (push protection is not a risk here); no deletes near FK-referenced tables (migration 0004 only adds a column); `lint-emitters` passes; exclusions remain add-only and never fail open; the MCP default is untouched. Its `add_exclusion_rule` re-enable warning caught a real defect in this batch's Undo — see #8 above. The note is committed alongside this bridge as the record of that hand-off.

## Verification

- `cargo test --workspace` — green, 0 failures, **0 warnings**. New: 6 in `aperture::commands::tests` (exactly-once feedback, mute→dismissed, rating-once-per-change, restored queue keeps ages + offers, connectorless rows still restore), 7 in `aperture-suggestion-generator` (per-connector offers, escaping, honest empties, `created_ts` stamp), 6 in `aperture::tests` (the settings backfill's additive-only rule).
- `tsc --noEmit` clean; `vite build` clean; `lint-emitters` OK (no new egress surface — the gateway rebuild reuses `build_gateway`).
- **The pure UI helpers have no test runner.** They were verified with a scratch script kept at
  `…/scratchpad/check_admit.mjs` (16 assertions: score decay, the cap regression, single-slot promotion, ageing flipping the ranking, tuning clamps). Re-run by transpiling `ui/src/state/{bubbleLifecycle,glassBudget}.ts` to a temp dir and running it under node. **If a future session adds vitest, port these first** — they cover the two bugs above.

## What's left — in order

### IMMEDIATELY NEXT: Batch 5 (trust items)
- **#3 screenshot redaction** (`privacy/redaction.rs` `redact_payload` skips `PayloadItem::Screenshot`): OCR-then-redact-then-recompose or region blur; must land BEFORE screenshot enrichment leaves its "(v2)" disabled state in `ContextPreviewPanel.tsx`. The #24 text rules compose with an OCR pass.
- **#1 SC5 zero-egress proof**: `gates/tests/sc5_network_monitor.rs` is 100% `todo!()`/`#[ignore]`. Build the real byte-level monitor harness (SC6's real-harness conversion on 08-15 is the precedent). Also the reasoning-gateway TODO for a CI lint statically blocking sockets/spawns outside sanctioned crates (note: `orchestration/model_fetch.rs` is a sanctioned exception).

### Then: installer + owner QA (decision #2 — needs Rajeev at the machine)
Everything in the 08-16 bridge's QA list, plus this session's: bubbles queueing correctly (arrive a 4th while 3 are up — it must WAIT, then appear when one leaves), the Advanced tab's dwell slider taking effect on the next bubble with no restart, "Stop capturing X" in a bubble's ⋯ menu writing a rule visible in the Privacy panel (and Undo removing it), thumbs lighting on every monitor, and the transport radio changing where a Send actually goes.

### Deferred (owner/hardware-dependent)
Unchanged: #25 GPU STT, #26 VAD/mic validation, SC3/SC4/PresentMon runs, the M5 load-times gate switch to `BudgetEnforcer::ceiling_gb()`.

### Small leftovers — four of the 08-16 list are CLOSED this session
Closed here:
- **Settings migration for upgraded installs** — `main::backfill_new_settings_keys` runs at every launch and adds seed keys the install never received, recursively and **additive-only** (a stored value is never overwritten at any depth; type mismatches are skipped rather than merged; `$comment` keys are skipped). This was made pressing by this batch's own new key: behavior was always correct (code defaults mirror the seed) but the Advanced tab cannot show a value that was never seeded. 6 tests, including "a current install is a no-op" over every section of the real seed file.
- **Doc 04 + ADR-030 amendment for #43** — 7.0 GB is now documented as the *fallback*, with the ceiling derived at startup as `GPU total − 1.0 GB` clamped [2.0, 31.0]. Identical on the 8 GB dev machine; the invariant status is unchanged.
- **Doc 10 §2-5 TTL values** — corrected to the code (browser 24 h, youtube 3 d, document 30 d, ide 30 d) after decision #37.
- **`gates/m9_privacy.rs` assertion message** — reworded (and the test renamed) to say what it actually asserts since decision #20: the *compiled* bootstrap stays empty on purpose, because a compiled-in rule could never be deleted; the shipped defaults are a durable, deletable seed.

Still open, carried over: Bearer-rule capture-group variant; Slack `xapp-`/`xoxe-`; `position_rank` treating Estimated as exact; persist temporal histograms + `app_class→process` map across restarts; VLM download cancel button; the M5 load-times gate still asserting the VRAM constant instead of `BudgetEnforcer::ceiling_gb()`.

New from this session: the `promote`/`admissionScore` scratch checks want a real test runner; `Bubble.onExited` is now belt-and-braces dead code (kept deliberately, documented).

### Decisions explicitly "No action" (do NOT re-open without asking)
#6, #9, #14, #19, #22, #34, #40, #44, #46, #55 — unchanged from the 08-16 bridge.

## How to resume
Tell the next session: *"Read docs/handoff/session-bridge-2026-08-19-doc24-batch4.md, then start batch 5 — #3 screenshot redaction first."*
