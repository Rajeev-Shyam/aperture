# SDLC review — 2026-08-19 (post Doc-24 batch 4)

Single-session review, read by hand rather than fanned out to agents (the owner asked for no multi-agent workflow this session). Scope: the batch-4 working tree (`ad6d30e`, `c9a389f`) **plus** the surrounding v1 surfaces those changes touch — bubble lifecycle, suggestions/retention, the settings surface, the reasoning gateway's transport selection, the overlay's idle cost, and the test/verification story.

Every finding below was verified against the code at the cited line, and each states the concrete failure, not a style preference. **Nothing here is a regression from batch 4 unless it says so** — most are pre-existing gaps that batch 4 made visible or reachable.

Status legend: **[FIXED]** = closed in this session's tree · **[OPEN]** = for a future session.

> **Update 2026-08-22:** all eight open findings below were closed in the v2-wiring session — see `docs/handoff/session-bridge-2026-08-22-v2-wiring.md` §2 for what each fix is. The per-finding text is left as written (it is the record of what was found); the summary table carries the new status.

Severity is about user consequence, not effort.

---

## Summary

| # | Sev | Dimension | Finding | Status |
|---|-----|-----------|---------|--------|
| 1 | HIGH | privacy / cost | A push Send can egress via a transport the preview never named — including the metered API key | **FIXED 2026-08-22** — `send_with_preview` binds to `payload.transport_target`; `TransportMismatch` + `preview_retarget` |
| 2 | MED | correctness | Suggestions queued during a snooze never surface when the snooze lifts | **FIXED 2026-08-22** — `suggestions_refresh` event + core one-shot at the deadline |
| 3 | MED | correctness / UX | A restored bubble can be un-actionable, and a failed Resume is silent | **FIXED 2026-08-22** — 7-day restore horizon + detached-row filter; `Failed` renders fallback copy, no `clicked` |
| 4 | MED | performance | Every DB read in a command blocks a tokio worker while holding the one global connection mutex | **FIXED 2026-08-22** — `commands::blocking()` (spawn_blocking) on the 12 heavy commands |
| 5 | MED | supply chain | VLM weights are verified by byte size only — no content hash | **FIXED 2026-08-22** — `sha256` in settings, verified while streaming (resume prefix included), URLs pinned to revision `5037fcf` |
| 6 | MED | performance | Idle wakeup cost has grown and has never been measured against doc 04 §8's <2 % target | **FIXED (cost) 2026-08-22** — JS interval only while rects exist; Rust poller 30 Hz when idle. SC3 still unmeasured. |
| 7 | MED | process / testing | No JS test runner — two of the three bugs found this session were in pure, untested TS | **FIXED 2026-08-22** — vitest 2.1.9, 19 tests (the 16 scratch assertions + glassBudget), `npm --prefix ui run test` |
| 8 | LOW-MED | correctness | Suggestions that were queued but never shown can never be pruned | **FIXED 2026-08-22** — `COALESCE(resolved_ts, shown_ts, created_ts)` in both clauses |
| 9 | MED | correctness | The Advanced tab merged into a settings snapshot cached at mount, silently reverting the HUD's anchor | **FIXED** |
| 10 | LOW | performance | One settings write, one broadcast and one engine reconfigure per slider *pixel* | **FIXED** |

---

## 1. [HIGH] [privacy/cost] A push Send can egress via a transport the preview never named — including the metered API key

`crates/reasoning-gateway/src/lib.rs:180-186` · `src-tauri/src/commands/mod.rs` (`request_preview`) · `ui/src/components/ContextPreviewPanel.tsx:324-341`

The preview footer names exactly one transport — `payload.transport_target`, stamped at build time from `AppState::push_target()` — and shows its health dot. That is what the user reads before pressing Send.

`Gateway::send_with_preview` then calls `pick_healthy_transport()`, which walks the **whole configured order** and returns the first push transport that is `Health::Ready`. It never compares its choice to `payload.transport_target`. So whenever the named transport is not ready, the approved payload silently leaves over the next one instead.

The MCP release path already guards exactly this: `mcp_bridge.rs:216-224` refuses to release a payload whose `transport_target` is not `ClaudeDesktopMcp`, restoring the session untouched — "an approval given for a push Send must stay reserved for that send". The push path has no equivalent.

**Failure scenario.** `transport_order` is `[mcp, claude-cli, messages-api]` (the shipped default) and `messages_api_key` is set. The `claude` CLI is not on PATH — a fresh machine, a PATH change, a failed update — so `CliTransport::health()` reports `NeedsSetup`. The user opens a preview: the footer says **Claude CLI**, with a yellow dot they may read as "setting up". They press Send. The payload goes out over **HTTPS to api.anthropic.com against their API key**, and is billed. ADR-010 records that this owner *deliberately redirected away from a metered key*; nothing on screen said the meter was about to run. The audit row is honest — `used_target` is the transport that actually carried it — so the trail and the consent disagree after the fact, which is the harder failure to notice.

Decision #39 adds a second route to the same gap: switching the preferred transport in the Dashboard rebuilds the gateway while an already-approved payload still carries the old `transport_target`.

**Fix sketch.** Bind the push path the way MCP is bound: if `pick_healthy_transport()` returns a transport other than `payload.transport_target`, do not send — return a typed error the panel renders as "Claude CLI isn't available; send via Messages API instead?" with an explicit second confirmation, and re-stamp `transport_target` only on that confirmation. Cheaper interim: have the panel call `transport_health` for the *whole* order and name the transport that would actually be picked, so the footer is never a guess.

---

## 2. [MED] [correctness] Suggestions queued during a snooze never surface when the snooze lifts

`src-tauri/src/commands/mod.rs:806-840` · `src-tauri/src/pipeline.rs:518-525` · `ui/src/components/BubbleContainer.tsx:82` · `ui/src/components/SnoozeControl.tsx:52-60`

ADR-040/Q95's contract, quoted in the pipeline's own comment: *"while snoozed, rows queue (learning continues) and surface via list_suggestions when the snooze lifts; only EMISSION is silenced."*

The first half works — the pattern task inserts the row with `state='shown'`→`'queued'` and skips `emit_bubble_spec`. The second half is not implemented. `list_suggestions` is called from exactly one place, `BubbleContainer`'s mount effect. Nothing polls `snooze_until`, nothing emits on expiry, and `SnoozeControl.choose()` writes the new mode and re-reads the deadline for its own icon without asking anyone to re-fetch bubbles.

**Failure scenario.** The user snoozes 15 minutes to get through a meeting. Four suggestions are mined and queued. Fifteen minutes later the snooze has expired — the 🔕 icon un-fills correctly, because it re-reads `get_snooze` — but no bubble ever appears. Turning snooze off manually behaves identically. The queued rows stay invisible until the WebView remounts, i.e. until the app is restarted; by then the freshness score added by decision #5 correctly ranks them last, so several may never surface at all. The feature reads as "snooze silently discards suggestions", which is precisely what ADR-040 chose *not* to do.

**Fix sketch.** `set_snooze` broadcasts a `suggestions_refresh` event and the container re-runs `listSuggestions()` on it; for a timed snooze, the core also schedules a one-shot timer at the deadline that emits the same event (and the container can re-fetch on window focus as a backstop). Small, and it makes an already-built feature real.

---

## 3. [MED] [correctness/UX] A restored bubble can be un-actionable, and a failed Resume is silent

`src-tauri/src/commands/mod.rs` (`restorable_suggestions`) · `crates/db/src/retention.rs:120-143` · `src-tauri/src/commands/mod.rs` (`bubble_click`) · `ui/src/components/BubbleContainer.tsx` (`onResume`)

Three behaviours compose badly:

1. `restorable_suggestions` returns the 16 newest `queued`/`shown` rows **with no age bound**. A row is `shown` when it was displayed but never resolved — exactly what a crash or a kill mid-dwell leaves behind, days ago.
2. The nightly prune **nulls `suggestions.connector_id`** for rows whose connector state expired (the FK-detach that fixed the 2026-08-16 rollback bug). The suggestion row survives 180 days; its `action_ref` becomes `''`.
3. `bubble_click` rejects an empty `action_ref` with `Err`, and a stale-but-present state with `Ok(Failed{..})`. The UI's `onResume` handles neither: `.catch(console.error)` for the first, `console.warn("resume degraded/failed")` for the second.

**Failure scenario.** The app is killed with a bubble on screen. Two days later it restarts; the bubble is restored and, as slots free, enters. The user clicks **Resume**. Nothing happens — no open, no error, no fallback copy — and the bubble resolves as `clicked`, teaching the engine that this suggestion *worked*. Doc 10 §6 says the opposite should happen ("bubble swaps to fallback copy"), and doc 08 §5's posture is that stale bubbles are "prevented, not apologized for". A `TODO(M4-followup)` at the call site acknowledges the missing half.

Batch 4 did not create this, but it changed the shape: before the cap fix, restored rows appeared all at once and were dismissed en masse; now they drain through the queue one at a time, so each one gets clicked.

**Fix sketch.** Two independent halves, both small. (a) Give the restore a staleness floor — drop rows whose `created_ts` is older than the dwell-scale horizon, or whose `connector_id IS NULL`, in `restorable_suggestions` (the query already reads both columns). (b) Render the `OpenOutcome`: `Failed`/`Degraded` swaps the bubble to fallback copy instead of logging, and a failed open should not record `clicked` as reinforcement.

---

## 4. [MED] [performance] Every DB read in a command blocks a tokio worker while holding the one global connection mutex

`crates/db/src/lib.rs:217-223` · every `#[tauri::command]` in `src-tauri/src/commands/mod.rs`

`Db::with_conn` locks a single `Mutex<Connection>` and runs the closure inline. Every command that reads or writes — `list_events` (LIMIT 200 with a `LIKE` over OCR text), `dashboard_stats` (eight aggregates), `list_audit`, `purge_all`, `get_settings`, and now `restorable_suggestions`' LEFT JOIN — does so directly inside an `async fn`, i.e. **on a tokio worker thread, synchronously**. The same mutex is the Tier-0 pipeline's write path.

Two consequences: the async runtime loses a worker for the duration, and the capture pipeline's event/OCR writes queue behind the query. The codebase already knows the fix — `bubble_click` wraps its `ShellExecuteW` leg in `tokio::task::spawn_blocking` for exactly this reason — it is just not applied to DB work.

**Failure scenario.** The user types in the Dashboard's History search on a mature DB. Each keystroke (debounced to 250 ms) runs a `LIKE '%…%'` scan over `screen_context.ocr_text`, which has no index for that pattern. On a multi-hundred-MB history that is comfortably hundreds of ms per keystroke, during which the capture pipeline cannot write and other IPC calls sit behind the worker. `purge_all` is the worst case: an unbounded multi-table delete, inline, blocking capture for its whole duration with no progress indication.

**Fix sketch.** Wrap DB access in `spawn_blocking` at the command boundary (a thin `db_call(move |db| …)` helper keeps it one-line per site), and give `purge_all` a progress event since it is user-initiated and long. Neither changes the single-writer model.

---

## 5. [MED] [supply chain] VLM weights are verified by byte size only

`crates/orchestration/src/model_fetch.rs:55-70, 196-220` · `config/settings.default.json` (`loadout.vlm_download`)

The fetcher streams to `<dest>.part`, resumes with a `Range` request, and accepts the file when `written == item.expected_bytes`. `is_present` likewise keys on exact size. There is no content hash anywhere in the path, and the settings block carries `url` + `bytes` only.

**Failure scenario.** The `.part` from an interrupted download is resumed days later against an upstream file that has been re-uploaded (Hugging Face `resolve/main` is a moving reference, not a pinned revision). The resumed range comes from the new file; the concatenation is a valid-length, internally inconsistent GGUF. Size verification passes, the file is renamed into place, `is_present` reports installed, and the Dashboard says "Screen understanding (VLM) is installed". The next VLM job spawns llama-server against a corrupt model — best case a sidecar crash that reads as a flaky degrade, worst case garbage scene descriptions written into `screen_context.vlm_summary` and mined as if they were observations. A hostile mirror or an intercepted range response has the same shape with intent.

**Fix sketch.** Add `sha256` beside `bytes` in the settings block (HF publishes it) and verify the completed file before the rename; on mismatch delete the `.part` and report honestly rather than resuming into the same bad file. Pin the URL to a revision SHA rather than `main` while you are there — that alone removes the moving-target half.

---

## 6. [MED] [performance] Idle wakeup cost has grown and has never been measured

`src-tauri/src/hit_test.rs:46` (`POLL_INTERVAL = 16 ms`) · `ui/src/state/useHitTestRects.ts:26` (`REMEASURE_MS = 100`) · doc 04 §8

Doc 04 §8 budgets **< 2 % average CPU** at idle with capture ON. Two permanent timers run against it, and neither stops when there is nothing on screen:

- the Rust cursor poller at **60 Hz** (raised from 30 by decision #11 for hover feel), and
- a **10 Hz** `setInterval` in *every* overlay WebView that runs `querySelectorAll` + `getBoundingClientRect` over all interactive surfaces and diffs the result.

The second is per-monitor: a three-monitor desk runs three WebView2 renderers each forcing layout ten times a second, forever. The interval's own comment calls it "the last-resort net for MID-flight drift" — the MutationObserver and the animation/transition listeners already cover mount, unmount, class/style flips, and settled motion — so it is doing nothing at all in the common case where no bubble exists.

**Failure scenario.** Not a crash: a laptop that never reaches deep idle. Timer wakeups at 60 Hz + 10 Hz × N keep cores and the WebView renderers from coalescing, which shows up as battery drain and fan noise on an "always-on background app". Nobody would notice it in a functional test, and **SC3 (idle CPU) has never been run** — so the 2 % figure is still an assumption, and it has been getting more expensive.

**Fix sketch.** Gate the JS interval on there being anything to measure (start it when the first rect is published, stop it when the set goes empty — the diff already knows). Consider dropping the Rust poller to 30 Hz when no rects are published, since with zero rects the answer cannot change. Then run SC3 and put a real number in doc 04.

---

## 7. [MED] [process/testing] No JS test runner

`ui/package.json` (scripts: dev / build / preview)

Two of the three bugs found this session lived in `ui/src/state/bubbleLifecycle.ts` and `ui/src/components/BubbleContainer.tsx` — pure, framework-free, trivially testable logic with **zero tests**, in a project whose Rust side carries 51 test suites. Both had been latent for months. `tsc --noEmit` type-checks them and cannot catch either: an off-by-one in a cap and a callback that is never invoked are both perfectly well-typed.

The admission helpers were verified this session with a throwaway node script (16 assertions, kept at `…/scratchpad/check_admit.mjs`) — proof the logic *is* testable, and that the only thing missing is somewhere to put the file.

**Fix sketch.** Add vitest (one dev-dependency, no config for pure modules) and port the scratch assertions first — they cover exactly the two regressions. `bubbleLifecycle`, `glassBudget`, and the settings-clamp helpers are all pure and would be covered on day one.

---

## 8. [LOW-MED] [correctness] Suggestions that were queued but never shown can never be pruned

`crates/db/src/retention.rs:113-118`

```sql
DELETE FROM suggestions WHERE COALESCE(resolved_ts, shown_ts, 0) < ?1
  AND COALESCE(resolved_ts, shown_ts) IS NOT NULL
```

A row queued while snoozed has `shown_ts IS NULL` and `resolved_ts IS NULL`, so the second predicate excludes it permanently. Combined with finding 2 — which means those rows are never surfaced and therefore never resolved — "snooze forever" turns every mined suggestion into an immortal row.

**Failure scenario.** A user who prefers the app quiet leaves snooze on. The pattern engine keeps mining (by design — learning continues), inserting a `queued` suggestion row plus, for decision #15 candidates, a synthetic `app_focus` connector state each time. Nothing ever deletes the suggestions. The table grows without bound for the life of the install, and `list_suggestions` will one day return 16 rows from an arbitrary point in the past.

**Fix sketch.** One-word fix now that batch 4 added the column: `COALESCE(resolved_ts, shown_ts, created_ts)` in both clauses. Worth doing with finding 2, since together they close the snooze path properly.

---

## 9. [MED] [correctness] **[FIXED]** The Advanced tab merged into a settings snapshot cached at mount

`ui/src/components/Dashboard.tsx` (`AdvancedTab`)

Introduced by batch 4 and fixed in the same session. `set_settings` replaces a whole top-level key, and the `ui` section has **two** writers: the draggable HUD persists `ui.hud_anchor` (correctly — it re-reads and merges at write time), and the new Advanced tab held `ui` in a ref populated at mount.

**Failure scenario (as written).** Open the Dashboard, drag the HUD to another corner, then move the dwell slider. The slider's write carries the mount-time `ui` object, without the new anchor — the HUD jumps back to where it was. Silent, and it looks like the drag simply did not stick.

**Fix applied.** All writes now go through one `flush()` that re-reads `get_settings` and merges into the *current* value, batching every pending section into a single `set_settings` call. Pending edits are flushed on unmount so a gesture that ends with the panel closing is not lost.

---

## 10. [LOW] [performance] **[FIXED]** One settings write per slider pixel

`ui/src/components/Dashboard.tsx` (`AdvancedTab`, `VoiceTab`)

A range input fires `onChange` for every step of a drag. Each of those was a `set_settings` call = a DB row write + a `settings_changed` broadcast + a `getSettings` round-trip in *every* overlay window + a pattern-engine reconfigure. Dragging "quiet time" across its 5→180 range produced ~35 of them.

**Fix applied.** Slider edits are debounced (300 ms) into one batched write per gesture; the transport radio still writes straight through, since a click is not a drag and the gateway rebuild should not wait behind a debounce the user cannot see. **`VoiceTab`'s confirm-floor slider still has the original behaviour** — same class, one writer, left alone deliberately rather than touched outside this batch's scope. *(Debounced on 2026-08-22.)*

---

## Not findings (checked, and correct)

Recorded so a future review does not re-litigate them:

- **The MCP release gate** (`mcp_bridge.rs:200-280`) is genuinely well-hardened: transport binding, content-bound hash re-check, the decision-#42 cap, and audit-before-release with a fail-closed path *and* a user-visible reason. It is the model finding 1 should copy.
- **`preview_send`'s failure path** restores both the session and the approval hash, and the restored session re-hashes identically because `user_approved` is `#[serde(skip_serializing)]`. Retry after a transport failure genuinely works.
- **The nightly prune's FK detach** (retention.rs:120-143) correctly nulls both referencing tables before deleting `connector_state`, and the whole pass is wrapped with an explicit ROLLBACK on error. The 2026-08-16 bug is properly closed.
- **`add_exclusion`** validates with the same `RegexBuilder` configuration the capture gate compiles with, so a rule the panel shows as active cannot be one that matches nothing.
- **The settings backfill** added this session is additive-only at every depth and is covered by a test that runs it against every section of the real seed file and asserts zero changes.
- **`lint-emitters`** passes; batch 4 added no new egress surface (the gateway rebuild reuses `build_gateway`).

---

## What this review did not cover

Stated plainly so the next reviewer knows where the holes are:

- **The Windows-only legs were read, not run**: `windows_media_ocr`, DPAPI key wrapping, the named-pipe DACL, Job-Object sidecar reaping. All are on-hardware behaviours.
- **The v2 skeleton** (`action-executor`, `agent-loop`, `task-manager`, `screen-serializer`) was out of scope — it is built but unwired, and Q-V2-01..10 are still open.
- **No dynamic analysis**: SC3 (idle CPU), SC4 (STT latency), SC5 (byte-level zero egress), and PresentMon have still never been run. Findings 1 and 6 are both the kind of thing a real SC5/SC3 run would have caught first.
- **The voice subsystem** was only spot-checked; the VAD thresholds remain tuned against one laptop's microphone (decision #26, owner-dependent).
