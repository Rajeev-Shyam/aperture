<!-- Handoff/process doc (not an architecture doc). Bridges the session that
     applied the 36-agent review fixes, overhauled UI legibility, closed the
     v1 completeness list, found+fixed the zero-recommendations root cause,
     built the v2 skeleton, and ran a 7-dimension SDLC review. Supersedes the
     SESSION END STATE section of session-bridge-2026-08-14-appification.md.
     Authoritative design: Docs 00–22. -->

# 🔄 CLAUDE SESSION BRIDGE — review fixes + v2 skeleton + SDLC review — read this first

**Session date: 2026-08-15**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`

Apply **"How this user works"** from `session-bridge-2026-08-09-m9.md` (bottom
section) from your very first line. Read `review-findings-2026-08-14.md` for
the finding IDs referenced below, and `sdlc-review-2026-08-15.md` for this
session's own review.

---

## The headline: why there were ZERO recommendations, and when they start

Root cause found (pattern-engine `normalizer.rs`): **`Token::decode` never
restored the `:` that `encode` folds to `_`** — `url:docs.rs` persisted as
`url_docs.rs` and decoded back as `url_docs.rs`, which the connector lookup
(`strip_prefix("url:")`) rejects forever. Every hydrated browser/doc/IDE
pattern was permanently un-bubbleable after the FIRST app restart (and this
app autostarts at every login). 528 patterns hydrated, 0 could ever fire.
**Fixed** (prefix-aware decode restoration, byte-stable with existing DB
signatures) + a round-trip regression test.

With the fix, honest expectations (thresholds in `pattern-engine/config.rs`):
- a workflow repeated **3× in one day** → first bubble on its **4th**
  occurrence that day;
- a **once-a-day habit** → first bubble on ~**day 4** (recency-weighted
  support needs > 3.0);
- gates that still apply per event: score ≥ 0.7, fresh connector state for the
  consequent (browser/doc/IDE/youtube — plain window-focus consequents can't
  bubble by design), target not focused in the last 10 min, ≤ 4 bubbles/hr,
  capture ON.

Still structural (documented, NOT changed this session — design decisions for
a dedicated pass): most `⇒ app-focus` patterns are ineligible at trigger rule
3; temporal ("every day at 9am") patterns are mined but never fire
(`temporal.rs` uncalled); the engine reads compile-time constants, not
`settings.default.json`'s pattern_engine block. Also wired the never-called
weekly `PatternEngine::prune` (daily check in the pattern task + DB mirror).

## All 31 review findings from 2026-08-14: status

Every HIGH and MEDIUM is FIXED in this tree; LOWs fixed except where noted.

**HIGH (7/7 fixed):**
1. `search_history` oracle → constant hit/miss replies (always stages, no
   counts), LIKE wildcards escaped (`ESCAPE '\'`), match-over-redacted-text
   post-filter, and a NEW `mcp_search` audit event type written for EVERY
   query (contracts + db retention/purge + privacy view + doc'd).
2. SQLCipher migration crash window → interrupted-migration restore
   (`.plaintext-migrating` recovered before open), backup deleted only when
   the migration verifiably ran (flag) or real schema exists, plus a
   file-lock (`File::lock`, rust-version → 1.89) killing the double-launch
   race. Regression test added.
3. MCP approve self-cancel — was already fixed in-tree 08-14; now also in the
   rebuilt app.
4. Sidecar grandchild orphan → **Windows Job Objects**
   (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) at spawn: host death by ANY path
   (kill, drop, tray Quit's process::exit, crash) reaps llama/whisper too.
   Tray Quit additionally does a bounded (3 s) graceful `kill_all_sidecars`.
5. STT/VLM 2 MB axum body limit → `DefaultBodyLimit::max(64 MB)` on both
   hosts (utterances > 18 s and large frames no longer 413).
6. Gate harnesses: **SC6 is now a real harness** (spawns the real lifecycle,
   attributes VRAM via `nvidia-smi --query-compute-apps`, asserts tree-death
   via tasklist; `#[ignore]`, on-target, requires no other Aperture running).
   SC5-byte/SC3/SC4/PresentMon remain open (below).
7. Enrichment stubs on the trust surface → fake "[selection pending]" path
   REMOVED; Add selection/screen-summary/screenshot disabled with honest
   "(v2)" copy; **"Add more history" slider implemented for real**
   (`list_trail_events` command swaps the event_trail item; approval re-runs
   redaction); health dot now queries `transport_health` (gateway
   `health_report`).

**MEDIUM (11/11 fixed):** get_context/list_recent transport binding (MCP can
only see/release MCP-approved payloads); tray-quit orphan (Job Objects, above);
VAD continuous-quiet-speech discard (threshold also capped at 0.5× utterance
peak + test); spawn health poll `try_wait` fast-fail; non-exclusive panel
focus (`focus_overlay` command called from `useModalSurface`); multi-monitor
voice/bubble dismissal convergence (`voice_dismiss` command; `record_feedback`
broadcasts `suggestion_lifecycle`); preview clobber (incoming MCP requests
QUEUE while a panel is open; user-initiated open cancels the displaced session
properly); dark-glass hover regression (new `--lift-1/2` tokens; buttons,
glyph chip, ⋯ menu, nav/grip hovers all visible again); snooze UI (HUD 🔕 +
popover, `get_snooze` command); mute/exclude menu no-ops ("Mute this pattern"
→ real `FeedbackEvent::Muted` straight to the 7-day mute; "Exclusions…" opens
the privacy panel's exclusion manager — the bubble doesn't know its source
process, so pretending to exclude would lie); 👍/👎 thumbs (on the bubble AND
on Dashboard suggestion rows — SC7 finally has a data source); MCP
submit→bubbles (below); SC4/settings honesty (`stt_model` now names the
shipped `whisper-base.en`; GPU STT stays [VERIFY]).

**LOW (7/8 fixed):** pipe DACL (`D:P(A;;GA;;;OW)` owner-only) + client-side
server verification (aperture-mcp refuses a pipe server that is not the
sibling aperture.exe) + ERROR_PIPE_BUSY retry; get_context hash-mismatch now
restores the session; tray eager revert now truthful (`false`, not `!want`) +
first-run tray click focuses the overlay so consent is seen; waveform is a
REAL meter (core emits `{listening, level}` at 10 Hz from live mic RMS; the
`level || 0.5` freeze fixed); voice-answer × overlap (padding); dead settings
keys (scoped_allow_* removed + README corrected; retention_days is now READ by
the pruner each pass). NOT done: the VLM-weights fetch flow for other machines
(still dev-box only, still silent OCR degrade elsewhere — top of the next
list).

## US3's last leg + cloud suggestions render now

`pipeline::surface_cloud_suggestions` (new): connector re-validation →
`connector_state` insert (clicks resolve through the same Path B as local
bubbles) → durable `suggestions` row (`source='claude'`) → bubble emit
(snooze-aware) → `answer_text` on the voice answer card. Wired from BOTH
transports: `aperture_submit_suggestions` (MCP — was validate-and-drop) and
`preview_send` (push — the returned StructuredSuggestions were previously
discarded by App.tsx entirely). `list_suggestions` now reports the real
source column.

## UI legibility ("too see-through") — second pass

`--glass-*` now effectively opaque (α .96–.97), `--glass-3` deliberately
LIGHTER for a visible hover delta, `--fallback-opaque` .98, ink-secondary
0.72 / faint 0.5, hairlines brighter, and the new `--lift-1/2` light tints
for anything raised ON a dark surface. If it still reads hazy on some
backdrop, the next knob is dropping `backdrop-filter` saturation (160 %).

## v2 skeleton — BUILT (per Doc 22, grill-safe)

Four crates + shared contracts, all compiling + tested, none wired into the
runtime (locked decision 7: v1 runs underneath unchanged):
- `contracts/src/agent.rs` — Doc 22 §5 response schema (round-trip tested
  against the doc's own example), task states, ActionError taxonomy.
- `crates/action-executor` — `ActionExecutor` seam + `ExecutorTicket` (only a
  user-initiated task loop can act, type-enforced) + `ScriptedExecutor` fake;
  `UiaExecutor` refuses honestly until the V2-M0 spike (Q-V2-01).
- `crates/screen-serializer` — `StepPayload` (Doc 22 §3.2 wire names tested),
  redaction-before-assembly enforced by construction, payload SHA-256 for the
  step audit. Screenshot/Q-V2-02 land at V2-M1.
- `crates/task-manager` — tasks/task_steps CRUD over **migration 0003**
  (additive; ships safely to the existing DB), legal-transition enforcement at
  the persistence boundary, append-only steps, purge-task; fully tested.
- `crates/agent-loop` — the task state machine + termination conditions (step
  cap 50 [Q-V2-04], 3 consecutive errors, hard-stop-always-wins, VRAM-pause
  resumable), fully tested. The live driver is V2-M2, AFTER the Q-V2 grilling.
- Retention/purge extended to the new tables (steps 30 d, tasks 90 d).
Open Q-V2-01..10 remain open; nothing in the skeleton pre-decides them.

## Verification state

- `cargo test --workspace`: green (see the final run in this session's log).
- `tsc --noEmit`: clean. `cargo run -p xtask -- lint-emitters`: OK (gates'
  nvidia-smi/tasklist spawns are measurement, out of the proactive path).
- 7-dimension SDLC review (multi-agent, adversarially verified):
  `docs/handoff/sdlc-review-2026-08-15.md`.

## Still open (priority order for next session)

1. **VLM weights distribution** for non-dev machines (fetch-on-first-use like
   the fastembed path, or an explicit Dashboard notice + download button).
2. SC5 byte monitor, SC3/SC4 measured runs, M8 PresentMon harness.
3. Pattern-engine design pass: window-focus consequents (rule 3), temporal
   patterns unwired, settings-vs-constants.
4. Data-dir/install-dir separation (%APPDATA% migration) — do before v2
   grows the schema further.
5. v2 grill session (Q-V2-01..10) → V2-M0 UIA spike.
6. GPU STT ([VERIFY], optional — CPU base.en is fine for PTT).

## Practical notes

- Rebuild + reinstall: `cargo build --release -p aperture-stt-host -p
  aperture-vlm-host` and rebuild `aperture-mcp` (all three changed this
  session), copy into `src-tauri\binaries\` under the `-x86_64-pc-windows-msvc`
  names, then `ui\node_modules\.bin\tauri.cmd build` (NEVER bare cargo build
  into the install dir) → installer at `target\release\bundle\nsis\`.
- SC6 on-target run: close Aperture, then
  `cargo test -p aperture-gates --test sc6_vram_release -- --ignored`.
- The migration to schema v3 happens automatically on first launch of the new
  build; it is additive (two new empty tables).
