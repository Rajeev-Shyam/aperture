<!-- Handoff/process doc (not an architecture doc). Bridges the session that
     executed Doc 24's batch 4 (the bubble-UI cluster) and the SDLC review that
     followed it. Supersedes the "IMMEDIATELY NEXT" section of
     session-bridge-2026-08-16-doc24-execution.md.
     Authoritative design: Docs 00-22. Decisions log: Doc 24 (read with Doc 23). -->

# 🔄 CLAUDE SESSION BRIDGE — Doc 24 batch 4 + SDLC review — read this first

**Session date: 2026-08-19**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`
**Commits this session:** `ad6d30e` (batch 4) → `c9a389f` (four owed leftovers) → the review commit. **All pushed; remote is current.**
**Previous bridge:** `9b43ba2` — `session-bridge-2026-08-16-doc24-execution.md`, batches 1–3.

Apply **"How this user works"** from `session-bridge-2026-08-09-m9.md` from your very first line: answer first, short, at most one question.

---

## 0. The 60-second version

1. **Doc 24 batch 4 is done** — all six bubble-UI decisions (#5, #7, #8, #10, #39, #17-UI), implemented, tested, pushed.
2. **Three latent bugs were found by reading the code** before touching it, all live, all fixed: the ≤3 bubble cap was violated, the bubble queue was never drained, and one ignored bubble decayed its pattern once *per monitor*.
3. **Four owed leftovers closed**: settings backfill for upgraded installs, the Doc 04/ADR-030 amendment for #43, Doc 10's TTL values, the stale m9 assertion.
4. **An SDLC review ran afterwards** → `docs/handoff/sdlc-review-2026-08-19.md`. **10 findings; 2 fixed in-session, 8 open, 1 of them HIGH.**
5. **Nothing has been rebuilt or installed.** The ⚠️ host-rebuild wire break from 08-16 still stands, and migration 0004 is new.

**Start here next session:** read the review's finding #1 (HIGH) and decide whether it goes before batch 5. Then see §6.

---

## 1. Ground rules this session ran under

- **No multi-agent workflow, no subagents** — the owner asked explicitly. Everything was read and written in the main loop. That is *why* the three bugs surfaced: each site was read in full before being edited, and two of them are invisible to any test the repo currently has.
- **Verify before implementing.** Doc 23/24 citations are stale in places (the 08-16 bridge lists five examples). Batch 4 added one more: decision #8 says "Mute … currently a no-op" — Mute has been real since 08-15.
- **Commit after each coherent unit**, workspace green first.

---

## 2. What batch 4 shipped, decision by decision

### #5 — slot admission is freshness × confidence
The `admit()` sort was pure `spec.confidence`, and the code's own comment called it a placeholder.

- `BubbleSpec` gained **`created_ts: Option<i64>`** (additive; doc 15 §6 compatibility law). Durable as **`suggestions.created_ts`** — new migration **`0004_suggestion_created_ts.sql`**, backfilled from `shown_ts`.
- **Why a new column and not `shown_ts`:** `shown_ts` is NULL while a row is queued (snoozed), which is exactly the case that needs an age.
- `bubbleLifecycle.admissionScore` = `confidence × 0.5^(age / half_life)`. Half-life from **`ui.bubble_freshness_half_life_sec`**, default **600 s**, clamped 10–86 400. Settings-only on purpose: it tunes queue ordering, not anything the user can watch happen.
- **Decay, not a cutoff** — the *hard* freshness rule (past TTL ⇒ no candidate at all) already lives core-side in doc 08 §5; a second cliff would only move the arbitrariness.
- An absent `created_ts` scores as perfectly fresh. Penalizing it would bury the entire pre-upgrade queue.
- Queued bubbles are **re-scored on every admission**; visible bubbles are never re-ranked (a bubble that earned a slot keeps it for its dwell — re-sorting made them jump position and swap glass/opaque under the cursor).

### #7 — bubble dwell is a real control
- `ui.bubble_dwell_sec` now drives the countdown via a `dwellMs` prop (it was `DEFAULTS.dwellMs`, a constant). Dashboard **Advanced** tab edits it, 5–120 s.
- Live because **`set_settings` now announces its write**: `settings_changed { sections }` to every window, plus a ping on `settings_reload_tx` for long-lived Rust tasks. Without that, every Dashboard control is only a next-launch preference.
- `dwellMs` is deliberately **out of the dwell effect's dep list** — a settings change must not restart a countdown the user is already watching.
- The ⋯ menu now **pauses the dwell while open**. It is portalled to `<body>`, so moving the cursor onto it fired the bubble's `mouseleave`, resumed the countdown, and could expire the bubble mid-decision — much worse now that the menu holds a consequential action. Guarded with a `hoveringRef` so closing the menu cannot out-vote an active hover.

### #8 — "Exclude this app" is real
- `BubbleSpec.exclusion_offers: Vec<ExclusionOffer { label, match_kind, pattern }>`, derived core-side by **`suggestion_generator::exclusion_offers_for(&ConnectorState)`**:
  - **browser / youtube** → the *site* (`url_pattern`), house style: `^https?://([a-z0-9-]+\.)?docs\.rs([:/?#]|$)`. Excluding the whole browser from one page's bubble is far more than the user asked for.
  - **document / app_focus** → the *app* (`process`, lowercased image name), **only when it ends in `.exe`** — process matching is an exact image-name match, so a bare `"chrome"` would be a rule that silently protects nothing.
  - **ide** → nothing. Its payload (`path`/`line`/`workspace`) names no process.
- **Escaping lives in Rust, next to the matcher** — a regex built in the WebView that fails to compile becomes a rule the Privacy panel shows as active protection while it matches nothing. `regex` is a **dev-dependency** of suggestion-generator so tests compile every emitted pattern with the same `RegexBuilder` config the capture gate uses; no runtime dep was added.
- The menu renders "Stop capturing {label}" per offer → `add_exclusion` (durable + hot-reloaded) → inline confirmation with **Undo**. The bubble deliberately **stays**: excluding an app is a capture decision, not a judgment on the suggestion, and the menu must stay mounted for Undo.
- **Undo restores the row's PRIOR state** — flagged by the sibling session's note. `Db::add_exclusion_rule` is idempotent on `(match_kind, pattern)` and **re-enables** a matching row rather than inserting, so a blind `set_exclusion(id, null)` would delete a rule the user had set up earlier and merely switched off. The click now pre-reads `list_exclusions`: already-enabled ⇒ "already excluded", no add and no Undo; previously-disabled ⇒ Undo re-disables; absent ⇒ Undo deletes.
- `list_suggestions` **LEFT JOINs** `connector_state` so a restored bubble still carries its offers. LEFT, not INNER — a Claude answer bubble has no connector row and must still restore.

### #10 — multi-monitor bubble state, closed at the source
- `record_feedback` is now **exactly-once** for terminal transitions: `UPDATE suggestions SET state=? WHERE id=? AND state IN ('queued','shown')`. A 0-row update means someone already resolved it → **no engine signal, no second broadcast**. Extracted as `record_feedback_row` so it is testable against a real DB.
- **This was live, not hypothetical.** Every monitor runs its own overlay root with its own dwell timer, so one ignored bubble reported `expired` once per screen within milliseconds of itself, and `EXPIRE_DECAY_MULT` was applied N times — patterns were suppressed faster the more screens the owner has. The 08-15 lifecycle broadcast converges the *rendering* but races the sibling timers.
- Fixed core-side rather than per-window because the same duplicate arrives from a double-click, a retry, and a respawn re-report. The durable row is the arbiter, not any window's local belief.
- Thumbs guarded the same way, on the rating actually **changing** (👍👍👍 was compounding ×1.5). Deliberately **not** state-guarded — the Dashboard rates resolved rows, which is the point of retroactive thumbs (decision #9).
- New **`suggestion_rated { id, rating }`** broadcast; ratings moved from per-Bubble local state into the container's shared map, so the pressed thumb lights on every monitor and survives a remount.

### #39 — transport switching without a relaunch
- `AppState.gateway` + `push_target` → one **`GatewaySlot`** behind an `RwLock`, rebuilt by `set_settings` via `crate::build_gateway` whenever the patch touches `reasoning`.
- They travel together because they are two readings of one setting; stored apart, they could disagree about what a Send would do.
- Reads clone the `Arc<Gateway>` out immediately — **no guard is ever held across an `await`**, and an in-flight Send finishes on the transport it started with.
- Dashboard **Advanced** → radio over the push transports (Claude CLI / Messages API) with live per-transport health, moving the choice to the front of `transport_order`.
- **ADR-025's MCP-primary default is UNCHANGED** — only switching got easy. Doc 09 amended to say exactly that. MCP stays in the list either way: it is pull-only, `pick_healthy_transport` already skips it, and dropping it would unregister a path the switch is not about. **If the default is ever actually flipped, amend ADR-025 (doc 19) + doc 09.**
- ⚠️ See review finding **#1** — this area has a HIGH open finding that predates the change but which #39 gives a second route into.

### #17-UI — pattern-engine knobs, applied immediately
- Dashboard **Advanced** exposes certainty (`tau_conf`), repeats-before-a-habit (`cold_start_support_floor`), quiet time (`cooldown_min`), suggestions/hour (`cap_per_hour_default`), merge-written into the whole `pattern_engine` section.
- The **push-reload path is built** (it was listed as optional): the pattern task selects on `settings_reload_rx` alongside its 24-hour tick, re-reading only when the write names `pattern_engine`; a `Lagged` receiver re-reads unconditionally. The daily tick stays as the backstop for edits made outside the app.

---

## 3. Three bugs found by reading the code (all live, all fixed)

1. **The ≤3 visible cap was violated.** `admit()` sorted the whole list and promoted anything landing in the first `maxVisible` positions, but never demoted a visible bubble — so a high-scoring arrival while 3 were on screen produced a **4th**, breaking the doc 11 §3 UX cap and the doc 14 §5 glass budget together (2 glass + 2 opaque).
2. **The queue was never drained.** Promotion lived in a `removeBubble` wired to the Bubble's `onExited`, which **cannot fire**: `onLifecycle("exit")` removes the bubble from the container's list, so the Bubble unmounts and the effect that would call `onExited` is torn down first. Invisible while bug 1 made everything visible on arrival; fatal once the cap works. Both now go through one `promote()`, used by arrivals and by the freed-slot path.
3. **Per-monitor expiry multiplied the decay ladder** — see #10 above.

**Also fixed in passing:** glyphs rendered as *words* (`BubbleSpec.glyph` is a semantic token that is also persisted, and the bubble drew it verbatim — the literal string "video" inside a 28 px chip; the mark is now chosen at render by `glyphMark`, and decision #15's app-focus bubbles got their own `switch` token); the ⋯-menu dwell pause; a pre-existing `unused_mut` warning in `pattern-engine/src/lib.rs`.

---

## 4. Four owed leftovers closed (commit `c9a389f`)

- **Settings migration for upgraded installs** — `main::backfill_new_settings_keys` runs at every launch and adds seed keys the install never received. `seed_settings_if_empty` runs once, keyed on the `reasoning` row, so `loadout.vlm_download` (#30) and now `ui.bubble_freshness_half_life_sec` (#5) both landed unseeded on upgrades. Behaviour was always correct (code defaults mirror the seed); what was missing is that **a Dashboard control cannot show, or let the user move, a value that is not in the store**. The merge is recursive and **additive-only**: a stored value is never overwritten at any depth, type mismatches are skipped rather than merged, `$comment` keys are documentation. 6 tests, including one that runs it against every section of the real seed file and asserts zero changes.
- **Doc 04 + ADR-030 for #43** — 7.0 GB is now documented as the *fallback*; the ceiling is derived at startup as `GPU total − 1.0 GB` clamped [2.0, 31.0]. Identical on the 8 GB dev machine; invariant status unchanged.
- **Doc 10 §2-5 TTL values** — corrected to the code after decision #37 (browser 24 h, youtube 3 d, document 30 d, ide 30 d).
- **`gates/m9_privacy.rs`** — the assertion message and test name now say what the test actually asserts since decision #20: the *compiled* bootstrap stays empty on purpose (a compiled-in rule could never be deleted); shipped defaults are a durable, deletable seed.

---

## 5. The SDLC review → `docs/handoff/sdlc-review-2026-08-19.md`

Read the full doc; the table below is the index. **8 open, 2 fixed.**

| # | Sev | Finding |
|---|-----|---------|
| 1 | **HIGH** | A push Send can egress via a transport the preview never named — including the metered API key |
| 2 | MED | Suggestions queued during a snooze never surface when the snooze lifts |
| 3 | MED | A restored bubble can be un-actionable, and a failed Resume is silent |
| 4 | MED | Every DB read in a command blocks a tokio worker while holding the one global connection mutex |
| 5 | MED | VLM weights are verified by byte size only — no content hash |
| 6 | MED | Idle wakeup cost has grown (60 Hz + 10 Hz × N windows) and has never been measured |
| 7 | MED | No JS test runner — two of this session's three bugs were in pure, untested TS |
| 8 | LOW-MED | Suggestions queued but never shown can never be pruned |
| 9 | MED | **FIXED** — the Advanced tab merged into a settings snapshot cached at mount (reverted the HUD anchor) |
| 10 | LOW | **FIXED** — one settings write + broadcast + engine reconfigure per slider *pixel* |

**Finding 1 in one paragraph, because it should shape the next session:** the preview footer names `payload.transport_target` and shows its health dot, but `Gateway::send_with_preview` picks the first *healthy* push transport from the whole order and never compares. If the `claude` CLI is missing from PATH, a payload the user approved for "Claude CLI" leaves over **HTTPS against their Messages API key, and is billed** — and ADR-010 records that this owner deliberately redirected away from a metered key. The MCP release path already guards exactly this (`mcp_bridge.rs:216-224` refuses to release a payload approved for a different transport); the push path has no equivalent. Findings 2 and 8 are the same feature seen twice and should be fixed together.

---

## 6. What's left, in order

### Decide first: does review finding #1 go before batch 5?
It is small (bind the push path the way MCP is bound, or name the transport that would actually be picked), and it is the only HIGH. Recommendation: **yes** — it is a trust-and-money bug in the one path the whole product's promise rests on, and batch 5's #1 (SC5) is *about* proving that promise.

### Then: Batch 5 (the trust items) — budget it as a real batch
- **#3 screenshot redaction.** Scope this honestly: **`PayloadItem::Screenshot` has ZERO producers today** — nothing in the codebase constructs one, and the enrichment toggle is still the disabled "(v2)" control in `ContextPreviewPanel.tsx`. So #3 is not "add redaction to a shipping feature", it is "build the image-redaction gate so the feature can be switched on at all". Scoping note from reading it: `Windows.Media.Ocr`'s `OcrWord` **does** carry a bounding rect, but `windows_media_ocr::aggregate_lines` takes `Vec<String>` and throws the geometry away — a real OCR-then-redact-then-recompose pass needs the `OcrEngine` trait widened to return word boxes first, plus the `image` crate in `privacy`, plus a decision on blur-vs-block. Multi-crate, with a Windows-only, hard-to-unit-test leg.
- **#1 SC5 zero-egress proof.** `gates/tests/sc5_network_monitor.rs` is 100 % `todo!()`/`#[ignore]`. Build the real byte-level monitor harness (SC6's 08-15 real-harness conversion is the precedent; measurement spawns are lint-sanctioned). Also the reasoning-gateway TODO for a CI lint statically blocking sockets/spawns outside sanctioned crates — note `orchestration/model_fetch.rs` is now a sanctioned exception.

### Then: installer + owner QA (decision #2 — needs Rajeev at the machine)
Everything in the 08-16 bridge's QA list, **plus this session's**:
- bubbles **queue** correctly — let 3 be up, trigger a 4th: it must WAIT, then appear when one leaves (that is bugs 1+2 above, and the single most valuable thing to eyeball);
- the Advanced tab's dwell slider takes effect on the **next** bubble with no restart, and does not disturb a bubble already counting down;
- a bubble's ⋯ → "Stop capturing X" writes a rule visible in the Privacy panel, and **Undo** removes it (and, if you disable a rule first then re-add via the bubble, Undo puts it back to *disabled*, not deleted);
- thumbs light on **every** monitor;
- the transport radio changes where a Send actually goes (and keep finding #1 in mind while testing this);
- glyph marks render as marks, not the words "video"/"globe".

### Deferred (owner/hardware-dependent — unchanged)
#25 GPU STT (CUDA whisper build + SC4), #26 VAD/mic validation on the real microphone, SC3/SC4/PresentMon measured runs, the M5 load-times gate switch to `BudgetEnforcer::ceiling_gb()`.

### Small leftovers still open
Bearer-rule capture-group variant; Slack `xapp-`/`xoxe-` prefixes; `position_rank` treating `Estimated` as an exact position; persist temporal histograms + the `app_class→process` map across restarts (both in-memory today); a VLM download cancel button; `VoiceTab`'s confirm-floor slider still writes per drag step (review finding 10's class, left alone deliberately); `Bubble.onExited` is now belt-and-braces dead code (kept and documented).

### Decisions explicitly "No action" — do NOT re-open without asking
#6 glass/opaque split, #9 thumbs retroactive-only, #14 trigger thresholds, #19 passive-expiry cap, #22 retention defaults, #34 English-only incognito detection, #40 oversized hard-error, #44 refuse-and-notify, #46 whisper VRAM re-measure, #55 near-opaque look.

---

## 7. ⚠️ REBUILD REQUIRED before the next install (do not skip)

The 08-16 multipart wire break (#33) still applies **and** the UI bundle + shell both changed here:

1. `cargo build --release -p aperture-stt-host -p aperture-vlm-host` and rebuild `aperture-mcp`.
2. Copy all three into `src-tauri\binaries\` under the `-x86_64-pc-windows-msvc` names.
3. `ui\node_modules\.bin\tauri.cmd build` — **NEVER** a bare `cargo build` into the install dir → installer at `target\release\bundle\nsis\`.

**Schema:** migration **0004** is new this session (additive `ALTER TABLE suggestions ADD COLUMN created_ts`, backfilled from `shown_ts`). Forward-only, applies automatically. Nothing else changed.

---

## 8. Verification state at session end

- **`cargo test --workspace`** — green, 0 failures, **0 warnings**. 19 new tests: 6 in `aperture::commands::tests` (exactly-once feedback, mute→dismissed, rating-once-per-change, restored queue keeps ages + offers, connectorless rows still restore), 7 in `aperture-suggestion-generator` (per-connector offers, regex escaping, honest empties, `created_ts` stamp), 6 in `aperture::tests` (the settings backfill's additive-only rule).
- **`tsc --noEmit`** clean; **`vite build`** clean.
- **`cargo run -p xtask -- lint-emitters`** OK — no new egress surface (the gateway rebuild reuses `build_gateway`).
- **The pure UI helpers have no test runner** (review finding 7). They were verified with a scratch node script — **16 assertions**: score decay, the cap regression, single-slot promotion, ageing flipping the ranking, tuning clamps. Kept at `…/scratchpad/check_admit.mjs`. Re-run by transpiling `ui/src/state/{bubbleLifecycle,glassBudget}.ts` to a temp dir and running it under node. **If a future session adds vitest, port these first** — they cover exactly the two bugs above.

---

## 9. Gotchas worth carrying forward

Everything from `note-from-doc24-session-2026-08-19.md` still applies. Confirmed or added this session:

- **GitHub push protection** blocks pattern-shaped secrets in *test fixtures* — assemble them at runtime with `format!`. (Not hit this session; no token-shaped fixtures were added.)
- **`git commit --amend` is denied** by the permission classifier in auto mode — use `reset --soft HEAD~1` + a fresh commit.
- **`Db::add_exclusion_rule` re-enables** a disabled row on re-add. Any "undo" must restore the *prior* state, not delete. This bit decision #8 and was caught by the sibling session's note.
- **The Bash tool mangles backslashes in heredocs** — `\\\\` arrived as one backslash and silently produced invalid Rust. Write edit scripts to the scratchpad with the Write tool and run them, rather than piping heredocs through the shell. (This cost the session two false starts.)
- **`cmd | tail` in Bash eats cargo's exit code** — read the output text, not the exit status.
- **`cd` persists between Bash calls** — several commands failed with "No such file or directory" after an earlier `cd ui`. Prefer absolute paths or re-`cd` explicitly.
- Parallel `cargo test` transient LNK1104 = Defender; retry. PS 5.1 wraps cargo stderr in `NativeCommandError` noise.

---

## 10. Invariants (never re-open)

- **VRAM ceiling** = `GPU total − 1.0 GB`, clamped [2.0, 31.0], **7.0 fallback** (ADR-030 as amended by #43). A hard admission bound no load or job may exceed.
- **Two-emitter transparency gate**: no egress outside reasoning-gateway approved sends; `orchestration/model_fetch.rs` is sanctioned **ingress** only. `lint-emitters` must pass before every commit.
- **Exclusions never fail open.** Capture toggle releases in <3 s. Capture is OFF until first-run consent.
- **`preview_set_approved` is the sole setter of `user_approved`**; `preview_send` / `aperture_get_context` are the only releases, both content-hash-bound.

---

## How to resume

> *"Read `docs/handoff/session-bridge-2026-08-19-doc24-batch4.md` and `docs/handoff/sdlc-review-2026-08-19.md`. Fix review finding #1 (transport binding on the push Send path), then start batch 5 with #3 screenshot redaction."*
