# Session Bridge — 2026-07-05 (multi-lens audit closed, downloads landed)

**Read this first.** It exists so the next session assumes nothing, hallucinates
nothing, and makes no generic decisions. Everything here was true at the end of
2026-07-05. Where this doc and the code disagree, the code wins — but check git
log first; the state below matches commit `502c598` + the downloads commit after
it.

---

## 1. Where you are

- Repo: `C:\Users\rajee\Documents\Projects\aperture` (clone of
  `Rajeev-Shyam/aperture`).
- Branch: **`r2-spec-integration` — LOCAL ONLY. NEVER push to GitHub without
  the user's explicit OK.** The user reviews first. This has been a standing
  rule all session; do not "helpfully" push.
- Working tree at session end: clean, all work committed.
- Commit chain this project (oldest → newest):
  `4f8a9bb` (pre-R2 skeleton baseline) → `d9b6a4b` (R2 docs landed inline) →
  Step-0/M1/M2/M3 build commits → `3a02b04` (M3 complete) → `4e1b12a` /
  `2523c02` (R2 alignment + UI hygiene) → **`50b955c`** (audit fixes, 18
  findings) → **`502c598`** (audit fixes, final 7 findings) → downloads/defaults
  commit (nomic default ON, fetch_model example, this doc).
- Authoritative spec: `docs/` (Docs 00–22, R2-amended inline; Doc 20 lists all
  amendments; Doc 19 = ADRs; Doc 22 = v2 draft, not in scope). Build handoff:
  `docs/handoff/claude-code-build-prompt-m1-m3.md`.

## 2. Environment facts (do not rediscover)

- Windows 11 laptop, RTX 5060 8 GB. VS Build Tools 2022 (MSVC 14.44 + SDK
  26100), rustup 1.96.1, `rust-toolchain.toml` pinned to 1.96.1.
- The user is on **metered data**: ask before any GB-scale download. The two
  pending downloads are now DONE (see §5) — nothing else needs fetching for the
  smoke test.
- Shell quirks that burned time before (avoid repeats):
  - PowerShell 5.1: `2>&1` on native exes wraps stderr in ErrorRecords and
    fakes failure exit codes — use `*> file.txt` then read the file.
  - Backticks in `git commit -m` strings get executed — use single-quoted
    here-strings (`@'…'@`).
  - Transient `LNK1104 cannot open file` when cargo test/gates overlap — just
    re-run; it is a Windows file lock, not a code error.
  - The Edit tool requires a Read of the file in the same conversation first.
- Session/infra quirks seen twice: subagent rate limits kill Workflow agents
  mid-run (resume works: same `scriptPath` + `resumeFromRunId`, completed
  agents replay from journal cache); the safety classifier can be transiently
  down, blocking Edit calls — retry later, do other work.

## 3. What this session did (complete list)

### 3a. Multi-lens self-audit — CLOSED

A 41-agent Workflow (4 find lenses: invariants / r2-drift / correctness / gaps;
adversarial verify per finding) ran over everything since `4f8a9bb`, was killed
twice by rate limits, and was resumed to full completion (41/41 agents, 0
errors). **25 confirmed findings. Every one is fixed and committed.** Full
verified detail (failure scenarios + verify reasons) lives in the workflow
output; the fixes:

**Commit `50b955c` (first 18):**
1. `crates/capture/src/toggle.rs` — toggle-OFF 3 s SLA raced a zero-await
   future; watchdog could never fire, `force_release` was dead code. Now:
   teardown on `spawn_blocking`, `tokio::time::timeout` races the JoinHandle
   (`race_release_sla`), breach → `ToggleSlaBreach` + force path. Tests:
   `sla_race_flags_breach_when_teardown_stalls`, `sla_race_passes_fast_teardown`.
2. CRITICAL `crates/capture` url_pattern no-op — url-excluded pages were still
   frame-sampled/OCR'd/persisted (gates passed `url=None`; heartbeat re-sampled
   every 5–20 s). Now: normalizer resolves the browser URL BEFORE the primary
   event verdict (`resolve_url`, two-stage `apply_exclusion`); the URL rides
   in-memory on `Normalized.url` → `sampler.request(..)` → `pending` →
   `ForegroundContext.url` (heartbeat). Test:
   `url_pattern_rule_excludes_the_primary_event_and_frame`.
3. Debounce TOCTOU — stale `pending` identity gated, but pull/crop targeted the
   CURRENT (possibly excluded) foreground. Now: `sample_once` step 0 re-resolves
   the capture-time foreground (`capture_time_identity()`, Windows-only cfg)
   and skips on mismatch. `WindowIdentity` gained `PartialEq`.
4. `apply_exclusion` hardcoded `window_class=None` — class-only rules left
   events unflagged/mined. Now threaded from identity at both call sites.
   Test: `window_class_rule_flags_the_event_metadata_only`.
5. `crates/db/src/retention.rs` — prune opened BEGIN, no ROLLBACK on error →
   wedged shared connection (silent event loss + broken M2 store path). Now:
   inner-closure + explicit ROLLBACK (mirrors migrations.rs). Test:
   `prune_error_rolls_back_and_frees_the_shared_connection`.
6. WindowClosed events were anonymous (identity resolved after HWND death) and
   mostly dropped (GA_ROOT filter fails on destroyed hwnds). Now: hwnd-keyed
   `identity_cache` in `CaptureSubsystem` (populated on live events, serves +
   evicts on close, drops unknown hwnds); hooks callback lets null-root
   DESTROYs through.
7. xtask `lint_emitters` never scanned `src-tauri` — now it does (84 files).
8. `config/settings.default.json` — full R2 rewrite (was R1 seeds): 7.0 GB cap,
   idle-unload 60 s, faster-whisper small, adaptive heartbeat 5–20 s (default
   10 s), tau_conf 0.7, adaptive cap 2–8/hr (default 4), split half-lives
   5 d/14 d, dwell 20 s, ≤2 glass surfaces, MCP→CLI→API transport order,
   scoped-allow keys (enabled false / cancel 3 s), EMPTY exclusion_defaults
   (ADR-029).
9. UI blur: `.preview` panel 16 px → `--blur-12` (ADR-039 ceiling);
   design-tokens header comment fixed; `--blur-16` token KEPT (post-v1
   refraction tier only).
10. Comment-only drift: privacy lib header (ADR-026 scoped allow),
    reasoning-gateway ×5 spots + Cargo.toml (MCP-primary, ADR-025), stt-host +
    voice ×2 (faster-whisper small ~2 GB GPU / whisper.cpp base-tiny CPU,
    ADR-024).

**Commit `502c598` (final 7, from the resumed verifiers):**
11. `src-tauri/src/pipeline.rs` — patterns were NEVER flushed to DB;
    `suggestions.pattern_id` (FK, `PRAGMA foreign_keys=ON`) got the engine's
    negative local ids → guaranteed FK violation on the first live candidate =
    Critical Path A dead at M4. Now: `flush_patterns()` upserts dirty rows by
    `signature` (UNIQUE, `RETURNING id`), calls `engine.mark_flushed`, returns
    a local→DB id remap applied before every suggestion insert; failed flushes
    stay dirty and retry.
12. `session_id` was never stamped (every events row NULL, unrecoverable —
    ADR-032 forbids retro-sessionizing). Now: engine exposes
    `last_session()` (+ `with_next_session_id()`; `last_session` is set only
    when the event actually sessionized); the pipeline hydrates
    `MAX(session_id)+1` at startup and `UPDATE events SET session_id` after
    each minable event. Heartbeat rows still NULL — explicit TODO(M4) in
    pipeline.rs (they bypass the bus).
13. Truthful capture indicator (doc 13 §8): `toggle_capture` no longer emits
    On/Off from the REQUESTED state (only the transitional "Releasing");
    `spawn_capture_driver` (now spawned in Tauri setup, takes orchestration +
    AppHandle) emits from the OBSERVED outcome; failed start → revert
    ToggleOwner to Off (so pattern-engine rule 7 agrees and retry
    re-broadcasts).
14. `CaptureToggle::acquire` rolls back fully on failure (hooks uninstalled,
    state → Off, never stuck Starting — test updated to assert Off);
    `release()` is idempotent when already Off (no spurious audit row on the
    revert path — covered in `release_emits_audit_event_...`).
15. Feedback loop core wiring (doc 08 §7 / Q81): new `record_feedback` IPC
    (kinds: clicked/dismissed/expired/up/down) updates the suggestions row
    (state/resolved_ts/useful_rating — dismissed bubbles no longer resurrect on
    WebView respawn via `list_suggestions`) and forwards `(pattern_id, signal)`
    over an unbounded mpsc (`AppState.feedback_tx`) into the pattern task,
    which calls `engine.apply_feedback` + flushes. UI side wired in
    `BubbleContainer.tsx` for dismiss/expiry/click; `recordFeedback` +
    `setSnooze` added to `ui/src/lib/ipc.ts`.
16. Global snooze core seam (ADR-040/Q95): `set_snooze` IPC
    ("off"|"15m"|"1h"|"forever") writes `AppState.snooze_until` (AtomicI64,
    0=off, MAX=forever); pattern task inserts `queued` (no shown_ts, no emit)
    while snoozed; `list_suggestions` returns empty while snoozed; queued rows
    surface when it lifts. Capture + learning continue — snooze ≠ toggle.
17. (from the same run) capture indicator minor + toggle retry no-op — folded
    into 13/14.

### 3b. Downloads — BOTH DONE (user gave explicit OK this session)

- **UI npm install**: `ui/node_modules` populated, exit 0. Two npm audit
  advisories (1 moderate, 1 high) in dev deps — noted, deliberately NOT fixed
  (user said only do the downloads). `ui/dist` still needs a first
  `npm run build` if the Tauri shell wants bundled assets — check
  `tauri.conf.json` frontendDist/devUrl before the smoke test.
- **nomic-embed-text-v1.5**: fetched into repo `models/` (~295 MB on disk;
  git-ignored via `/models/*` + `.gitkeep` — verified). Verified working:
  `768 dims, embed 22.8 ms` (budget ≤300 ms, doc 04 §8). Fetch/verify tool:
  `crates/embedding/examples/fetch_model.rs`
  (`cargo run -p aperture-embedding --features nomic --example fetch_model`).
- **Feature default FLIPPED ON** (the audit's follow-up): `default = ["nomic"]`
  in BOTH `crates/embedding/Cargo.toml` and `src-tauri/Cargo.toml`; module docs
  + main.rs comment updated. `--no-default-features` = zero-download
  HashEmbedder dev path. `cargo check --workspace` green with the new default.
  NOTE: `NomicEmbedder::load` uses `PathBuf::from("models")` — RELATIVE to the
  process CWD. Fine when running from repo root; if the smoke test runs the exe
  from elsewhere, the load fails and it silently falls back to HashEmbedder
  (log line says which backend loaded — CHECK IT during the smoke test).

## 4. Verified state at session end

- `cargo test --workspace`: green (~90+ tests).
- `cargo run -p xtask -- gate m0|m1|m2|m3`: all pass.
- `cargo run -p xtask -- lint-emitters`: green, 84 files incl. src-tauri.
- `cargo check --workspace` with nomic default: green.
- UI TypeScript has NOT been type-checked/built yet (npm just landed) — run
  `npm run build` in `ui/` early in the smoke test; my TSX/TS edits
  (BubbleContainer.tsx, ipc.ts) compiled only in my head.

## 5. Known deferrals — all TRACKED, none silent

| What | Where | When |
|---|---|---|
| 👍/👎 thumbs affordance (backend seam ready: `recordFeedback(id,"up"/"down")`) | `ui/src/components/BubbleContainer.tsx` header TODO(M3-followup) | next UI pass |
| Snooze UI control (backend seam ready: `setSnooze`) | `ui/src/lib/ipc.ts` TODO(M3-followup) | next UI pass |
| Bubble drag + persisted position (Q66) | BubbleContainer.tsx header TODO | next UI pass |
| Heartbeat rows not sessionized | `src-tauri/src/pipeline.rs` TODO(M4) | M4 |
| bubble_click connector dispatch | commands/mod.rs (honest Err) | M4 |
| Sidecar kill on toggle-OFF | toggle_owner.rs TODO(M5) | M5 |
| SC6 VRAM on-target check | `APERTURE_SC6_ON_TARGET=1` env | smoke test |
| SC5 strict egress gate | `APERTURE_SC5_STRICT=1` env | M7 |
| SQLCipher key wiring (DB opens with empty key today) | main.rs / key_manager | M9 |
| Exclusion rules load from settings (composition root wires EMPTY list) | src-tauri/src/main.rs M9 comment | M9 |

## 6. Invariants + R2 values (do NOT re-derive, do NOT genericize)

- Three invariants: (1) 7.0 GB VRAM projection cap (ADR-030); (2) two-emitter
  transparency gate — only reasoning-gateway opens sockets/spawns Claude CLI
  (ADR-026/028/036); (3) capture toggle OFF ≤3 s incl. force-release.
- R2 numbers now embedded in code + seed: tau_conf 0.7; adaptive cap 2–8/hr
  (cold-start 4); half-lives temporal 5 d / sequence 14 d; session gap
  cold-start 15 min (clamp 5–45); dwell 20 s; blur ceiling 12 px; ≤2 glass +
  opaque 3rd (3 visible max); heartbeat 5–20 s adaptive (default 10 s);
  idle-unload 60 s; debounce 300 ms; toggle SLA 3000 ms; STT = faster-whisper
  small ~2 GB GPU / whisper.cpp base-tiny CPU (ADR-024); transport
  MCP→CLI→API (ADR-025); scoped always-allow w/ 3 s cancel (ADR-026);
  exclusion defaults EMPTY (ADR-029); pHash hamming 4; embed 768-d pinned.

## 7. NEXT TASK (user-agreed): on-target smoke test

The user explicitly queued this as the next thing. Scope (from doc 16 + what's
built): run the real app on this machine and observe, not just tests:

1. `npm run build` in `ui/` first (TS compile of my edits + produce dist).
2. `cargo run -p aperture` (src-tauri package name — verify; run from REPO
   ROOT so `models/` resolves). Expect: overlay window, log line
   `embedding backend nomic-embed-text-v1.5 (fastembed/onnx, cpu)` — if it
   says hash-trigram, the model path/CWD is wrong.
3. Toggle capture ON via the UI → indicator must reflect OBSERVED state;
   focus/title events should land in the DB (`events` table, session_id
   stamped non-NULL for minable events); OCR text should appear in
   `screen_context` with 768-d vectors in `ctx_vec`.
4. Toggle OFF → ≤3 s, audit row `capture_toggle{on:false}`.
5. Watch for: WGC capture failures (RDP/secure desktop degrade to event-only),
   first-frame OCR latency, pHash suppression counters, no bubbles expected
   (connector lookup is None until M4 — SILENCE IS CORRECT, not a bug).
6. SC6 on-target: `APERTURE_SC6_ON_TARGET=1` gate run while capture ON/OFF.
- DB lives under the user profile (`aperture_db::default_db_path()`) — inspect
  with a sqlite CLI if needed; it is NOT encrypted yet (M9).

After the smoke test (in order, all need the user): push `r2-spec-integration`
+ PR (needs explicit OK), then M4 (connectors + browser extension — YouTube
connector first per Q75).

## 8. User working style (respect this)

- Answer first, short chunks, no walls of text, one question max per message,
  no filler, direct pushback when something is wrong (AuDHD accessibility).
- Standing instruction from the build request: keep self-reviewing for
  mistakes/drift/breaches/gaps; the user checks in periodically.
- Never push to GitHub without an explicit OK. Ask before GB-scale downloads.
- Memory files live at
  `C:\Users\rajee\.claude\projects\C--Users-rajee-Documents-Projects-Quellgeist\memory\`
  — `aperture-build-state.md` mirrors this doc in compressed form; update BOTH
  when state changes.
