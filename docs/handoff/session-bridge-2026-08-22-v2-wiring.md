<!-- Handoff/process doc (not an architecture doc). Bridges the session that
     closed the 2026-08-19 SDLC review's 8 open findings, built Doc 24 batch 5
     (#3 image redaction, #1 SC5), and wired the v2 agent execution layer
     (Doc 22, V2-M0 → V2-M6) per Doc 24 §K. Supersedes the "What's left" section
     of session-bridge-2026-08-19-doc24-batch4.md.
     Authoritative design: Docs 00–22. Decisions log: Doc 24 (statuses inline). -->

# 🔄 CLAUDE SESSION BRIDGE — fix-ups + batch 5 + v2 wired — read this first

**Session date: 2026-08-22**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`
**Previous bridge:** `session-bridge-2026-08-19-doc24-batch4.md` (commit `108c05c`).
**This session's commits:** see `git log` after `108c05c` (listed at the bottom once pushed).

Apply **"How this user works"** from `session-bridge-2026-08-09-m9.md` from your very first line: answer first, short, at most one question.

---

## 0. The 60-second version

1. **All 8 open findings of the 2026-08-19 SDLC review are closed**, HIGH #1 first: a push Send is now bound to the transport the preview named; if it is not ready the user is asked, never silently billed.
2. **Doc 24 batch 5 is done**: #3 image redaction (OCR word boxes → solid blocks → 768 px JPEG) and #1 SC5 — now a **real, unignored, byte-level harness** that runs on every `cargo test --workspace`.
3. **v2 is built and wired** (Doc 22 V2-M0 → V2-M6, Doc 24 §K #47–#54 + F2): Claude Desktop drives the desktop through two new MCP tools; the user approves once per task, sees a live step log, and can stop from the overlay or the tray. The V2-M0 gate **passed on the real desktop** (a Notepad driven end to end).
4. **Doc 22 is promoted from Draft.** The Q-V2 questions the owner never got to grill are answered **[PROVISIONAL]** in its §12 with reasoning — Rajeev should read that table and overturn anything he disagrees with (most are settings now).
5. **Nothing has been rebuilt or installed.** The 08-16 host-rebuild rule still stands (§8). No new migration this session (0004 is still the newest).
6. Ground rules this session: ultracode (multi-agent) was ON by the owner's `/effort` — implementation fanned out on disjoint file sets, then an adversarial review verified the tree (§6). No downloads over 1.5 GB (vitest ≈ 30 MB was the only new download).

**Start here next session:** read Doc 22 §12 (the [PROVISIONAL] table) with Rajeev, then §7 (owner QA), then rebuild + install (§8).

---

## 1. What the fix-ups are (2026-08-19 review, findings 1–8)

| # | Fix | Where |
|---|-----|-------|
| 1 HIGH | `Gateway::send_with_preview` picks ONLY the transport whose target == `payload.transport_target` and is Ready; otherwise `GatewayError::TransportMismatch { named, available }` — nothing sent, nothing audited. `preview_send` returns `PreviewSendResult::{Sent, TransportMismatch}`; the panel says "X isn't available right now" and offers "Send via Y instead" → new `preview_retarget` (drops the approval; the user re-approves the re-stamped payload). An MCP-staged payload can never be retargeted to a push transport. 3 gateway tests. | `reasoning-gateway/src/lib.rs`, `commands/mod.rs`, `ContextPreviewPanel.tsx` |
| 2 | `set_snooze` emits `suggestions_refresh {reason}` on `off`; a timed snooze spawns a one-shot that CASes `snooze_until` back to 0 at the deadline and emits `snooze_expired`. `BubbleContainer` re-fetches and admits rows it does not already hold (a held row is NOT re-admitted — that would reset a visible bubble's dwell). | `commands/mod.rs`, `events.rs`, `BubbleContainer.tsx` |
| 3 | `restorable_suggestions(db, now)`: 7-day horizon on `created_ts`, and local rows whose `connector_id` was nulled by the FK-detach are dropped. UI: a `Failed` open sets `instance.fallback` → the bubble shows "Couldn't resume — …", hides Resume, records NO `clicked`; Degraded counts as clicked (it did open). | `commands/mod.rs`, `Bubble.tsx`, `bubbleLifecycle.ts` |
| 4 | `commands::blocking()` (`spawn_blocking`) wraps the DB work of 12 commands (list_audit, purge_all, list_exclusions, dashboard_stats, list_events, list_patterns, list_suggestion_history, list_suggestions, list_trail_events, get_settings, set_settings' write loop, record_feedback). No behaviour change; no purge progress event (out of scope). | `commands/mod.rs` |
| 5 | `FetchItem.sha256`; hashed while streaming, resumed `.part` prefix hashed first; `HashMismatch` deletes the `.part` and never renames. URLs pinned to HF revision `5037fcf163dd…`; hashes verified 2026-08-22 against HF `X-Linked-ETag` AND the dev box's files (`d02fe9b6…`, `b9160fe9…`). `is_present` stays size-only on purpose. A test pins the consts to the seed file. | `orchestration/model_fetch.rs`, `vlm_fetch.rs`, `settings.default.json` |
| 6 | JS: the 100 ms re-measure interval runs only while rects exist. Rust: poller 16 ms when any window has rects/modal, else 33 ms. **SC3 still unmeasured.** | `useHitTestRects.ts`, `hit_test.rs` |
| 7 | vitest 2.1.9; `ui/src/state/bubbleLifecycle.test.ts` ports all 16 scratch assertions + 3 glassBudget tests. `npm --prefix ui run test`. | `ui/package.json` |
| 8 | Retention step 5: `COALESCE(resolved_ts, shown_ts, created_ts)` in both clauses; test. | `db/src/retention.rs` |
| 10-class | `VoiceTab` confirm-floor slider debounced like the Advanced tab. | `Dashboard.tsx` |

## 2. Batch 5 (the trust items)

### #3 image redaction — built, consumed by v2
- `vision-ocr`: `OcrOutput.lines: Vec<OcrLine{text, words: Vec<OcrWord{text,x,y,w,h}>}>` (additive; `text`/`mean_confidence` byte-identical). `windows_media_ocr` reads `OcrWord.BoundingRect` (rounded outward). `aggregate_ocr_lines` beside the kept `aggregate_lines`.
- `privacy`: `Redactor::find_spans` (byte spans, same gates as `redact_text`, earlier rules win; parity test diffs the two) and `image_redaction::redact_bgra(bgra, w, h, &[LineBoxes], &Redactor)` — opaque black block (2 px pad) over every word a span covers. **Block, not blur** — blur is partially invertible. No `image` crate in privacy; privacy does not depend on vision-ocr (the serializer converts).
- `screen-serializer::screenshot::observe_frame`: OCR at Layer A scale (≤ 1600) → paint at that scale → downscale to 768 → JPEG q85 → base64. The ONLY producer of a payload screenshot; `RawObservation.screenshot` is `Option<RedactedScreenshot>` so an unredacted one cannot be constructed through the crate.
- **Q-V2-02 measured:** worst-case noise 768×432 → 525 KB JPEG / 700 KB base64, under the 1 MiB MCP cap with 64 KiB headroom (`q_v2_02_…` test prints it).
- **Not done:** the v1 "Add screenshot (v2)" enrichment button stays disabled — the push transports carry no image (API body has no image block; CLI cannot take one). Small separate item.

### #1 SC5 — real
`gates/tests/sc5_network_monitor.rs` (not `#[ignore]`, Windows-only, loopback-only): loopback origin = byte counter + body capture; `netstat -ano` = zero non-loopback connections from the test PID; `Get-CimInstance Win32_Process` = zero children. Proactive chain = real `PatternEngine` over a scripted 4-day workflow + real `Redactor`/`payload_builder` over the now-real `golden::redaction_fixture()` (2 e-mails + 1 secret assembled at runtime — push protection) → `PreviewSession`. Then approve → real `Gateway` + `ApiTransport` at the loopback URL → exactly one request, `sha256(body) == sha256(transport.wire_bytes(payload))` == the `cloud_send` row; second test: an unapproved payload never opens the socket. `cargo xtask sc5` runs it unconditionally now.

## 3. v2 — what was built, file by file

- **`crates/capture`** — `CaptureSubsystem::observe_now() -> Result<sampler::Observation, CaptureError>`: same gate order as a scheduled sample (suspended ⇒ `CaptureUnavailable("capture is off…")`; exclusion incl. the hook-tracked URL only if that identity is in front now ⇒ new `CaptureError::Excluded(label)`), no debounce, no pHash, frame RETURNED. `Observation` is deliberately not Debug/Clone.
- **`crates/action-executor`** (V2-M0, the real thing): `UiaExecutor::new(probe: Arc<dyn ExclusionProbe>, stop: Arc<AtomicBool>)`; guard ladder on every call = stop flag → exclusion probe on the foreground window → elevation (`TokenElevation`; access-denied counts as elevated) → dispatch. `grounding` (normalize, Levenshtein, `match_label` exact→contains→fuzzy≤2, `best_index`), `keys::parse_chord`, `risk::{consequential_reason, reversibility}`, `platform` (EnumWindows, SendInput, UIA find/click/scroll, elevation, `read_document_text` = the VISIBLE document — Store Notepad keeps restored tabs offscreen in the tree), `close_windows` (#54 undo). `launch` = Win → type → Enter (#52). F2 = xtask lint on the literal `ExecutorTicket::for_task(`.
- **`crates/screen-serializer`** — above.
- **`crates/agent-loop::driver`** — `AgentDriver` (pure policy, 16 offline tests): `approve/deny/hard_stop/classify/confirm/answer/resume/execute/pause_for/note_malformed/observed/undoable_windows`, `PauseReason`, `Disposition`, `StepLogEntry`, `LoopConfig` (from the new `agent` settings section). Every executed/skipped/refused action writes a `task_steps` row carrying the hash of the last payload staged (locked decision 6).
- **`crates/task-manager`** — `list_tasks(limit)`.
- **`src-tauri/src/agent.rs`** — `AgentRuntime` in `AppState.agent` (+ `agent_notify`): one task at a time; `mcp_start` / `mcp_step` (waits ≤ 110 s for the user inside the call; executes on `spawn_blocking` with the driver moved OUT of the mutex so the stop flag is never queued behind an action; observes → `build_step_payload` → **audit BEFORE release, fail-closed** → text + image); `user_start_task / user_decide / user_answer / user_undo_close_windows`; `ExclusionListProbe`.
- **`mcp_bridge.rs`** — `aperture_agent_start(task?)`, `aperture_agent_step(task_id, instruction?)`; the 1 MiB cap on text + image; image as an MCP `image` content block. `transports/mcp.rs` — the two descriptors (the system prompt Claude sees lives in `TOOL_AGENT_STEP`'s description).
- **Commands** — `agent_status, agent_start_task, agent_decide, agent_answer, agent_undo_close_windows, agent_dismiss, agent_list_tasks, agent_task_steps, agent_purge_task`. **Event** — `agent_task` (`TaskView` | null). **Tray** — "Stop agent task". **Settings** — `agent.{step_cap, error_threshold, confirm_low_confidence}` (backfilled on upgrade by the 08-19 additive merge).
- **UI** — `AgentSurface.tsx` + `agent.css` (status bar, step log, Stop, the five pause cards, terminal card with undo, the ✦ task composer opened from the HUD); Dashboard **Agent** tab; `ipc.ts` agent section.

### The two entry paths (both end at the same gate)
- **Claude-initiated:** the user asks Claude Desktop to do X → Claude calls `aperture_agent_start{task:"X"}` → approval card → `agent_step` loop.
- **User-initiated (Doc 22 §9.1):** ✦ on the HUD → type the task → "Start task" (approved by construction, locked decision 4) → the user tells Claude Desktop "run my Aperture task" → `aperture_agent_start` with no `task` adopts it.

### Three WinUI input quirks the V2-M0 gate found (all fixed in `platform.rs`)
1. XAML drops a key-down it processes after the key-up arrived → every key is **held until the target pumps** (`SendMessageTimeout(WM_NULL)`) + 12 ms; chords send modifiers, pump, key, pump, then release.
2. `KEYEVENTF_UNICODE` packets coalesce to the last character ("text" → "tttt") → printable characters go as **layout VK events** (`VkKeyScanW`), Unicode only for characters the layout cannot produce.
3. A freshly-foregrounded app silently drops input for ~500 ms → `switch_window` ends with pump + 500 ms + pump (300 ms lost characters every run; 700 ms lost none), and retries `SetForegroundWindow` 5× for a still-starting app.

## 4. Doc 22's [PROVISIONAL] answers (owner to confirm — the one thing Rajeev must read)
Q-V2-04 step cap **50** (now `agent.step_cap`) · Q-V2-05 **no VLM** in the loop · Q-V2-07 **retry with error context**, 3-consecutive-failure bound, malformed instruction = failed step · Q-V2-10 **no agent VRAM tier** (loop is 0-VRAM; `PauseReason::Vram` exists, nothing raises it). Decided by Doc 24: Q-V2-03 (#49), Q-V2-09 (#50 — **partial**: prompt + Resume, no "restart as admin" relaunch), Q-V2-08 out. Measured: Q-V2-01 (gate), Q-V2-02. Dissolved: Q-V2-06.

**Deviations stated in Doc 22's status section:** hard stop is in-process on the tray thread (not a separate process); no task-complete *bubble* (the terminal card plays that role); no auto-approve-after-timeout for confirmations; coords fallback not advertised to Claude (physical px, unscaled).

## 5. Verification state at session end
- `cargo test --workspace`: **479 passed, 0 failed, 0 warnings**, 6 ignored (on-hardware gates). New this session: gateway +3, db +1, aperture +2, orchestration +4, vision-ocr +4, privacy +6, screen-serializer +3, action-executor +21, agent-loop +10, task-manager +1, SC5 +2, capture +1.
- `npm --prefix ui run test`: 19/19. `tsc --noEmit` clean. `vite build` clean.
- `cargo run -p xtask -- lint-emitters`: OK (110 files; executor/serializer/agent-loop scanned as non-emitters; F2 ticket lint active).
- **On-target, run once on the dev box:** `cargo test -p aperture-gates --test v2m0_uia_executor -- --ignored` → **passed** (switch → type verified → click "File" → Esc → Ctrl+A → Backspace → Alt+F4).
- **Not run:** the app itself (no rebuild/install yet — §8), SC3/SC4/PresentMon, the real Claude Desktop ↔ `aperture_agent_step` loop end to end (needs the installed app + Claude Desktop; §7).

## 6. Adversarial review of this tree — outcome (updated 2026-08-26)
The review ran as a fan-out (9 subsystem mappers → reviewers → skeptics). **The skeptic/verify phase mostly died on account session limits (54 of 71 agents errored)**, so what survived is the reviewers' raw findings — severities are the reviewers' own, not all adversarially re-verified. The full verbatim list (file:line + FIX sketch per finding) is preserved in **`docs/handoff/review-findings-2026-08-22.md`**.

**Fixed on 2026-08-26 (all in `crates/agent-loop/src/driver.rs`; 18/18 tests green, workspace still compiles):**
- [med] Stale confirmation chip — an unanswered `PauseReason::Confirm` is now sticky: `classify` re-returns `NeedsConfirm` instead of letting a later instruction overwrite or slip past it.
- [med] No-op / no-instruction turns — `note_noop` records a step row (result Skipped) and counts toward the step cap, so a planner that only ever looks cannot release screens forever.
- [low] `approve`/`resume` — persist to DB FIRST, then move the in-memory machine; `approve` is idempotent. A DB error no longer wedges the task with IllegalTransition on retry.

**Still OPEN — the next session's first job, [high] items first (exact file:line + FIX text in the findings doc):**
1. [high] Hard stop while an action is in flight never reaches the driver (state stays Running, one more screen released) — `src-tauri/src/agent.rs` ~576.
2. [high] Paused tasks are not gated: `mcp_step` observes/executes while Paused (Excluded/Elevated/Clarification) — the Resume gate is advisory — `agent.rs` ~444–454.
3. [high] `observe_and_release` never re-checks the driver (hard stop / paused / superseded id) before capturing and releasing — `agent.rs` ~657.
4. [high] `open_windows` ships every top-level window title unredacted, including excluded windows — `agent.rs` ~681.
5. [high] `observe_now` drops the URL when the hook identity is stale instead of refusing like `sample_once` does (url_pattern exclusions bypassed) — `crates/capture/src/sampler.rs` ~340.
6. [high] Multi-line PEM/OPENSSH secrets: OCR text fully redacted but only the BEGIN line painted in the screenshot — `crates/screen-serializer/src/screenshot.rs` ~81.
7. [med] (nine) focused_window.url / last_action.result / clarification answer leave unredacted; agent `cloud_send` row written before the MCP cap check (phantom row) + `wire_sha256`/`byte_count` computed over non-wire bytes; `wait_for` lost wakeup (Notified created after the predicate check); blocking-thread panic leaves `in_flight` true forever; sequential 110 s waits can blow the 180 s pipe budget; Skip after the confirm timeout re-arms the chip; Stop during an in-flight action unacknowledged in the bar; cancel-during-send resurrects an approved preview session (zero-residue broken); `GatewayError::Validation` restores the approval after egress so retry re-sends the same bytes.
8. [low] (twelve) stale-id stop raises the current task's flag; no startup reconciliation of crashed 'running' tasks; Agent-tab Purge without confirmation; executor exclusion guard ignores browser URL; quality-dropped OCR pixels left unpainted; executor debug log prints window titles; retarget reverts panel item edits; vlm_fetch legacy `resolve/main` URL + pinned sha mismatch loop; AgentSurface listener leak; TaskComposer focus steal; agent bar shares the bottom-centre slot with voice surfaces; clarification answer types into the user's foreground app (UI focus routing).

After fixing: re-run `cargo test --workspace` + lint-emitters + tsc/vite/vitest, re-run the V2-M0 gate once on the dev box, and update this section.

## 7. Owner QA for the installed build (after §8)
Everything from the 08-19 bridge's list, plus:
- **Transport binding:** remove `claude` from PATH (or set CLI as preferred with it missing) → open a preview → Send → expect "Claude CLI isn't available right now" + "Send via Messages API instead", NOT a silent API call. Check the Activity view: no `cloud_send` row until the retargeted Send.
- **Snooze:** snooze 15 min (or use `off` after a `forever`) with patterns firing → queued bubbles appear when it lifts, without a restart.
- **Agent, Claude-initiated:** in Claude Desktop: "Use Aperture to open Notepad and type hello" → approval card appears on the overlay → Allow → watch the step log; Stop mid-task from the tray; check Dashboard → Agent for the rows.
- **Agent, user-initiated:** ✦ → type a task → Start → tell Claude Desktop "run my Aperture task".
- **Guards:** put 1Password (excluded) in front mid-task → "Paused — on your exclusion list"; open Task Manager in front → "runs as administrator" pause.
- **Consequential chip:** a task whose step hits a "Send"/"Delete" button → Approve / Skip / Stop chip appears before the click.
- **Undo:** a task that launched an app → after Stop, "Close 1 window it opened" closes it.
- **Redaction:** have an e-mail address and a fake `sk-…` on screen during a step; the step's screenshot Claude describes must show black blocks there (ask Claude what it sees).

## 8. ⚠️ REBUILD REQUIRED before the next install (unchanged rule)
1. `cargo build --release -p aperture-stt-host -p aperture-vlm-host` and rebuild `aperture-mcp` (**the MCP binary changed: two new tools in `tools/list`** — Claude Desktop reads it at launch; restart Claude Desktop after installing).
2. Copy all three into `src-tauri\binaries\` under the `-x86_64-pc-windows-msvc` names.
3. `ui\node_modules\.bin\tauri.cmd build` — NEVER a bare `cargo build` into the install dir → `target\release\bundle\nsis\`.
Schema: no new migration (0004 is the newest). Settings: the `agent` section is backfilled on first launch.

## 9. Gotchas added this session
- **The Bash tool mangles backslashes in heredocs — for real, every time.** Two patch attempts silently no-op'd (`'\r'` became `'\\r'`). Write patch scripts with the Write tool and run the file.
- **Workflow subagents can die on the account's session limit** mid-task ("You've hit your session limit"); the executor implementer did, with ~90 % of the crate written and a temp `tests/dbg_tmp.rs` left behind. Check `git status` for strays after any fan-out.
- **Store Notepad restores unsaved tabs across restarts and keeps them in the UIA tree offscreen** — "first Document control" is the wrong one. Also its session persisted the dead agent's debug typing until the gate's Ctrl+A/Backspace cleared it.
- **`cargo test` of the gates with `--nocapture` moves the user's mouse/keyboard** (V2-M0 gate). Check `Get-Process notepad` first — the guard kills every notepad.exe.
- HF `X-Linked-ETag` on a `resolve/` URL is the LFS SHA-256 (verified equal to `certutil -hashfile`).

## 10. Invariants (never re-open) — unchanged from the 08-19 bridge
VRAM ceiling = GPU total − 1.0 GB clamped [2.0, 31.0], 7.0 fallback · two-emitter gate (+ `model_fetch.rs` sanctioned ingress) and now **the executor is not an emitter and cannot spawn** · exclusions never fail open (now also: the agent never looks at or acts on an excluded window) · capture toggle < 3 s · capture OFF until consent · `preview_set_approved` sole setter of `user_approved`; `preview_send` / `aperture_get_context` / `aperture_agent_step` the only releases, every one audited before release · **the hard stop always wins** (flag first, then transition).

---

## How to resume

> *"Read this bridge — §6 first: fix the six OPEN [high] review findings (full text in `docs/handoff/review-findings-2026-08-22.md`; three driver findings are already fixed), then the [med]s, then re-verify. Then walk Doc 22 §12 with Rajeev to confirm or overturn the [PROVISIONAL] answers, rebuild the three hosts + `tauri build`, install, and run §7's QA — the real Claude Desktop ↔ `aperture_agent_step` loop has not been exercised end to end yet."*
