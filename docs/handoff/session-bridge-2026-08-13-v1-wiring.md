<!-- Handoff/process doc (not an architecture doc). Bridges the session that wired
     the v1 composition root (voice + gateway + hit-testing + hot-reload) and got
     the app RUNNING on the dev box. Supersedes session-bridge-2026-08-09-m9.md.
     Authoritative design remains Docs 00–21. -->

# 🔄 CLAUDE SESSION BRIDGE — v1 composition root wired, app RUNS — read this first

**Session date: 2026-08-13**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`

Apply **"How this user works"** from `session-bridge-2026-08-09-m9.md` (bottom section)
from your very first line. This bridge covers only what changed since 2026-08-09.

---

## Where the build is

**The app runs.** `npm --prefix ui run dev` (Vite on :5173) + `cargo run -p aperture`
brings the overlay up; first-run consent is clickable, capture turns ON and WGC
starts sampling, the nomic embedder loads, per-monitor overlays fan out (verified
count=2 on this box). `cargo test --workspace` green (0 failures), `tsc --noEmit`
clean, `cargo run -p xtask -- lint-emitters` OK.

The 2026-08-09 carry-forward items 2 (composition root), 4 (exclusion hot-reload),
5 (bubble hit-testing), 6 (CONN-M1), plus the settings-seeding note, are **done**:

### 1. Bubble hit-testing (doc 11 §2) — cursor poller, not window-level flips
`overlay::set_hit_test_rects`'s window-level design was unusable (a monitor-sized
interactive WebView swallows every click). Instead:
- `ui/src/state/useHitTestRects.ts` measures every `.surface-interactive` element
  (+ the portalled `.bubble__overflow`) on mutation + a 250 ms interval, converts
  to physical px, and publishes via the new `set_hit_test_rects` command.
- `src-tauri/src/hit_test.rs`: per-window rect store + ~30 Hz cursor poller that
  clears `WS_EX_TRANSPARENT` only while the cursor is inside a rect (8 px pad).
  `set_overlay_interactive` (modals) now routes through the same state, so the
  modal override and bubble hover can't clobber each other's style writes.
- `overlay::set_transparent` is the new no-focus primitive; `set_interactive`
  (modal, takes focus) sits on top.

### 2. Reasoning gateway wired (M7 composition root, doc 09)
- `main::build_gateway`: transports from the settings order (MCP → CLI → API),
  `RegistryLookup` over the connector registry, **`Gateway::with_audit(AuditLog)`**
  — cloud_send rows now persist. `ConnectorLookup` gained a `Send + Sync` bound.
- `request_preview` is real: gathers last-voice-transcript (`answer_query`),
  seed connector state, and a recent-events trail (**`redaction_flags = 0` only**
  — excluded events can never enter a payload), redacts with the user's terms,
  builds via `payload_builder::build`, stores a `PreviewSession` in
  `AppState.previews` (1 h TTL prune).
- `preview_set_approved(payload_id)` marks; `preview_send(payload)` refuses
  unapproved ids, syncs the panel's edits onto the stored session object, then
  `PreviewSession::approve(Send)` → `gateway.send_with_preview`. New
  `preview_cancel` command + UI call on panel close (zero residue).
- Settings **first-run seeding** now happens (`seed_settings_if_empty`, keyed on
  the `reasoning` section — NOT table-empty, because `consent` lives in the same
  table). Model id `claude-opus-5` lives in `config/settings.default.json` (NG8).

### 3. Voice wired (M6 composition root, doc 07)
- `src-tauri/src/voice.rs`: dedicated OS thread (`aperture-voice` name) for the
  `!Send` subsystem. Loop: Win32 message pump (RegisterHotKey delivers to the
  registering thread) → non-blocking hotkey drain (`try_next_ptt_event`, new
  additive API) → command channel → 30 s ceiling → warm-keep lapse check.
  `ptt_up` pipeline runs via `block_on` on a current-thread runtime (one
  utterance at a time is the doc 07 contract).
- Capture driver: observed ON + `voice_opt_in` → `Enable`; OFF → `Disable`.
  `grant_voice_consent` enables immediately when capture is already on.
- Warm-keep: `PttWarmKeep::record_press` on Down → `set_warm_kept(SttHost)`,
  5 s lapse re-check (ADR-030/Q36).
- **`voice_run_transcript` exists** (confirm-chip Run): classify at 1.0, never
  re-stores; escalations stash the query in `AppState.voice.last_transcript`
  for the `answer_query` preview seed. UI onRun wired.
- One shared embedder instance (`build_embedder`) feeds ingest AND voice
  retrieval (doc 03 §5 comparability).
- STT still degrades honestly: `OsSpawner` refuses stt-host (no whisper binary/
  weights on this box) → `VoiceError::Stt` → an honest `empty` surface. Real STT
  needs: whisper weights + `aperture-stt-host`/`whisper-server` binaries, lift
  the refusal at `model_lifecycle.rs:453`, and implement the `/transcribe` call
  in `SidecarRunner::run` (still `Err(SidecarDown)` for Stt).

### 4. Exclusion hot-reload (doc 13 §4)
`ExclusionList` is interior-mutable (`Arc<RwLock<Arc<Vec<CompiledRule>>>>`);
sampler + normalizer share the handle, `AppState.exclusions` is the same handle,
and `add_exclusion`/`set_exclusion` recompile+swap via the shared
`exclusion::rules_from_rows` (one mapping for startup + reload). A re-read
failure KEEPS the previous list (never fail-open).

### 5. Small closures
- `gpu_busy` bus→WebView forwarder spawned (events.rs TODO(M3) closed).
- `capabilities/default.json` windows now `["overlay", "overlay-*"]` — the
  per-monitor clones had NO IPC permissions before.
- CONN-M1 fixed: coalesce map stores `(row_id, stale, captured_ts, rank)`;
  `position_rank` (has non-null `position_s` = 1). A lower-ranked capture NEVER
  overwrites a fresh higher-ranked row regardless of timestamps (out-of-order
  delivery is real); ts orders only equal-rank captures. Still stamps the event.
- `BubbleRect` derives Deserialize; `hit_test`/`voice` are new shell modules.

### 6. Dashboard + movable HUD (user-requested, same day)
- **Dashboard** (`ui/src/components/Dashboard.tsx`, `dashboard.css`): sidebar +
  content window (opaque) with Overview (counts/db size/encryption), History
  (all events joined with OCR excerpts, searchable via LIKE), Patterns,
  Suggestions (full lifecycle), Voice (transcripts) — backed by four new
  read-only commands: `dashboard_stats`, `list_events`, `list_patterns`,
  `list_suggestion_history`. Opened from the ◎ button in the HUD.
- **HUD cluster** (`ui/src/components/Hud.tsx`): capture indicator + ◎ + 🛡
  grouped, draggable by the ⠿ grip to any of 8 anchors (corners + edge
  midpoints), persisted in `ui.hud_anchor`. The grip holds the modal
  interactive override during the drag (rect publishing is too slow to follow).
- **Non-exclusive panels** (user feedback: the privacy panel swallowed every
  click on screen): `useModalSurface(ref, { exclusive: false })` for
  PrivacyPanel / ContextPreviewPanel / Dashboard — the panel's own hit-test
  rect makes it clickable while everything outside stays click-through to the
  user's apps. Only FirstRunConsent stays exclusive (must be answerable with
  no prior click). The Activity & Privacy audit feed intentionally shows only
  capture_toggle/cloud_send; the Dashboard's History tab is the full stream.
- **Window-style dragging** (`ui/src/state/useDraggable.ts`): threshold drag
  (~6 px before a press becomes a drag, so buttons keep working; pointer
  capture retargets the click away once dragging). PrivacyPanel and the
  preview drag by their headers, the Dashboard by its new titlebar, the HUD by
  anywhere on the cluster (still snaps to the 8 anchors on drop). The drag
  holds the modal interactive override (rect publishing can't follow a moving
  surface).
- **Stacking + toggles** (user feedback: the privacy panel "wouldn't close" —
  its × sat UNDER the top-right HUD cluster): panels are z-index 40, HUD 30,
  and the ◎/🛡 HUD buttons now toggle open/closed.
- **Voice is reachable**: the Dashboard's Voice tab carries the mic opt-in
  button (`grant_voice_consent` previously had no UI caller — voice could
  never be enabled), the PTT how-to (dynamic chord from settings), and an
  honest STT-not-installed note. `dashboard_stats` now reports `voice_opt_in`.
- **PTT chord conflict, observed live**: `Ctrl+Win+Space` is RESERVED by
  Windows (input-language switching) — registration fails on any machine with
  multiple layouts. Default is now `Ctrl+Alt+Space` (crate + seed), and
  `enable_with_fallbacks` walks a chord ladder on conflicts (mic failure stops
  it — no chord fixes a missing mic), persists whichever chord binds back to
  `voice.ptt_hotkey`, and tells the user via a notice. Verified live: config'd
  Ctrl+Win+Space failed → Ctrl+Alt+Space bound, mic probe PASSED (the first
  real `enable()` run — hardware path no longer UNVERIFIED on this box).
- **Voice notice surface**: opaque (was unreadable glass) and always closable
  (×) — the `empty`/notice and `answer` surfaces both gained dismiss buttons.

### 7. Multi-agent review (3 lenses + adversarial verify, 18 agents) — all fixed
15 raw findings, 10 confirmed (2 dupes → 8 distinct), 5 refuted. Fixed:
- **HIGH — approval was bound to a UUID, not content.** `preview_set_approved`
  now takes the full edited payload, syncs it into the core session, re-caps
  the trail, RE-RUNS redaction (panel-added text had never seen the redactor),
  and records approval as a SHA-256 over the canonical bytes. If redaction
  changed anything, approval is refused and the redacted object returns with
  `changed: true` — the panel re-renders and requires a second, informed Send.
  `preview_send` now takes ONLY the payload id and ships the core-owned object
  after re-verifying the hash: a client cannot substitute content post-gate.
- **HIGH — a failed Send dead-ended silently** (first-run default: no CLI, no
  API key ⇒ NoHealthyTransport, session already consumed, panel showed
  nothing, retry said "unknown payload"). `preview_send` restores the session +
  approval on transport error; the panel catches and displays `sendError`.
- **MED — voice could enable off stale consent** (capture.start() failure
  leaves `consent.capture_enabled=true` while mechanism is Off).
  `grant_voice_consent` and the PTT commands now gate on the LIVE ToggleOwner
  state (`capture_is_live`), never the persisted decision.
- **MED — WebView reload orphaned the modal override** (whole monitor stuck
  interactive). New `reset_overlay_interactivity` command, called by App on
  mount; modal flag is now a COUNT (stacked panels can't strip each other).
- **MED — CONN-M1 guard inverted for out-of-order stale captures** (see §5).
- **LOW — `last_transcript` lived forever**: cleared on voice Disable; consumed
  with `take()` (seeds exactly one preview).
- **LOW — applied-cache collapsed hover vs modal mechanisms** (modal mounting
  under a hovering cursor never got focus): the poller now caches the applied
  MECHANISM `(interactive, modal)`, not a bool.
- Hardening from a refuted finding: the settings seed uses per-key
  `INSERT OR IGNORE` (can never overwrite a user row under sentinel drift).
Refuted (no action, reasons in the workflow output): seed overwriting user
settings (unreachable), exclusion-reload swallow (single-connection Db makes
the partial failure unreachable), voice block_on breaking the 3 s SLA (STT jobs
are cancelled by the same OFF), hover-cache skipping focus on click-opens (the
opening click itself activates the window).

---

## Still open for v1 (unchanged unless noted)

1. **M9 encryption criterion** — needs native Strawberry Perl + NASM, then
   `cargo test -p aperture-gates --features sqlcipher --test m9_privacy`.
2. **On-hardware gates** (SC3/SC4/PresentMon/SC5 byte-monitor) + UNVERIFIED
   bodies. The overlay/consent/capture path is now exercised live; voice hotkey
   + mic are wired but unverified end-to-end (no STT backend yet — see §3).
3. **STT backend install** (new, concrete): whisper weights + host binaries +
   the two code lifts in §3.
4. **Deferred M7**: MCP stdio server + `aperture_get_context` gate; gated
   `aperture_search_history` UX (decide with Rajeev).
5. Multi-monitor rendering: each overlay clone runs the full React root — every
   monitor shows its own bubble stack/indicator (acceptable duplication for now;
   revisit at the M8 hardware gate).

## Practical notes (additions)

- Dev launch: `npm --prefix ui run dev` then `cargo run -p aperture` (no tauri-cli
  installed; not needed for dev). Vite is pinned :5173 strict.
- The settings seed runs once; to re-seed a dev DB, delete the `reasoning` row or
  the DB at `%LOCALAPPDATA%\Aperture\history.db`.
- The Messages API transport reads the key from settings `reasoning.messages_api_key`
  or `ANTHROPIC_API_KEY`; without one it reports NeedsSetup and the order falls
  through to the CLI transport (health = `claude --version`).
