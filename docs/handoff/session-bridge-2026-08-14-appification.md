<!-- Handoff/process doc (not an architecture doc). Bridges the session that made
     Aperture an installable, always-on desktop app (tray + autostart + NSIS
     installer) and wired REAL STT end-to-end (whisper.cpp). Supersedes
     session-bridge-2026-08-13-v1-wiring.md. Authoritative design: Docs 00–21. -->

# 🔄 CLAUDE SESSION BRIDGE — installable app + real STT — read this first

**Session date: 2026-08-14**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`

Apply **"How this user works"** from `session-bridge-2026-08-09-m9.md` (bottom
section) from your very first line. This bridge covers only what changed since
2026-08-13.

---

## Where the build is

**Aperture is now a click-and-run installed app.** `ui\node_modules\.bin\tauri.cmd build`
produces an NSIS installer (`target\release\bundle\nsis\Aperture_0.1.0_x64-setup.exe`
— workspace-root target dir; per-user install, no admin). The installed app: starts at login, lives in the
system tray, restores capture from stored consent, and transcribes voice locally
via whisper.cpp. `cargo test --workspace` green, `tsc` clean, `lint-emitters` OK.

**Installed-app smoke test PASSED live (2026-08-14, %LOCALAPPDATA%\Aperture):**
nomic embedder auto-downloaded once (~40 s, into the install-dir `models\`) then
loaded; 2 overlays; 528 patterns hydrated; `start-at-login synced enabled=true`
(HKCU Run key written by the upgrade path); capture restored from stored
consent; WGC live; voice chord `Ctrl+Alt+Space` registered + mic probed; second
launch handed off to the running PID (single-instance); WAL writing heartbeats.
Note: first installed launch shows nothing for ~40 s while the embedder
downloads — a known cold-start gap (no splash/progress surface yet).

### 1. System tray (`src-tauri/src/tray.rs`, new)
The overlay skips the taskbar, so the tray is the app's only findable handle.
- Menu: Open Dashboard / **Capture my screen** (CheckMenuItem) / **Start when I
  sign in** (CheckMenuItem) / Quit. Left-click on the icon opens the Dashboard.
- Truthfulness: the capture checkmark + tooltip mirror the `capture_indicator`
  event via `app.listen_any` (the OBSERVED state, never the requested one).
  `CaptureIndicatorPayload` gained `Deserialize` for this.
- The tray capture toggle calls the same `commands::toggle_capture` as the HUD;
  it refuses (recheck-reverts) while first-run is incomplete.
- Quit ≠ capture-off: consent persists, next login restores it.
- **Win11 chevron fix** (user report: "I don't see the app on my taskbar"):
  Windows 11 buries new tray icons in the overflow flyout. `promote_tray_icon()`
  (tray.rs, winreg) writes `IsPromoted=1` under
  `HKCU\Control Panel\NotifyIconSettings\<id>` on the icon's FIRST registration
  only — an explicit user demotion (0) is never overridden. Retries ~10 s
  because Explorer creates the entry asynchronously.
- **Build gotcha:** changing a resource mapping from file→`dir/` to
  file→`dir/file` leaves the old staged FILE at `target\{debug,release}\models`;
  the next build fails with os error 183 until you delete it.
- New event `dashboard_open` (`events::emit_dashboard_open`) — **targeted at the
  primary overlay only** (a broadcast would open one dashboard per monitor).
  App.tsx listens and opens the Dashboard.

### 2. Autostart + single-instance (plugins, `main.rs`)
- `tauri-plugin-single-instance` is registered FIRST: a second launch (shortcut
  click while running) hands off to the running instance, which surfaces the
  Dashboard. Two instances would race one DB + double-run capture.
- `tauri-plugin-autostart`: choice persists as settings `ui.autostart`;
  `main::sync_autostart` re-asserts it every launch (repairs a moved exe).
  Defaults ON once first-run completes — **release builds only** (a dev run must
  never write a `target\debug` path into HKCU Run). `complete_first_run` enables
  it for fresh installs; `sync_autostart` covers upgraded installs (pref absent
  + first-run done → ON). Commands `get_autostart`/`set_autostart`; Dashboard
  Overview has the toggle; the tray item drives the same helpers.

### 3. Real STT, end-to-end (closes the 2026-08-13 §3 carry-forward)
- `os_spawn::spawn` (orchestration): the "M6 milestone" refusal is GONE; it now
  branches per `SidecarKind` — SttHost spawns `stt_host_bin --port … --whisper-bin
  … --model … --device cpu --child-port …`.
- `SidecarRunner::run` implements the `GpuJobKind::Stt` arm: POST
  `{endpoint}/transcribe` `{ wav: [...] }` → `JobOutput::Stt`.
- `SidecarConfig` gained `whisper_bin` / `stt_model` / `stt_on_gpu` (default
  false — we ship the CPU+BLAS whisper build; a CUDA `whisper-server` flips it).
- `main::sidecar_config()` resolves paths for both layouts: installed
  (`stt-host.exe`, `whisper\`, `models\` next to aperture.exe) and dev
  (`target\*\aperture-stt-host.exe`, `src-tauri\binaries\whisper\`, repo
  `models\`). VLM keeps crate defaults → spawn fails → OCR-only degrade
  (unchanged; no llama/Qwen weights yet).
- **Verified live 2026-08-14**: full chain `aperture-stt-host` → whisper.cpp
  v1.9.2 `whisper-server` (CPU+BLAS, ggml-base.en) transcribed a SAPI-spoken
  test utterance EXACTLY ("Continue the Rust tutorial video from yesterday."),
  confidence 0.93, 796 ms. The whisper `/inference` multipart + `verbose_json`
  confidence contract is no longer [VERIFY].
- On-disk (git-ignored): `src-tauri\binaries\whisper\` (whisper-server.exe +
  ggml/BLAS DLLs, 61 MB, from whisper.cpp release `whisper-blas-bin-x64.zip`),
  `models\ggml-base.en.bin` (147 MB, HF `ggerganov/whisper.cpp`). The REAL
  `aperture-stt-host.exe` (release) replaced the 0-byte externalBin stub at
  `src-tauri\binaries\stt-host-x86_64-pc-windows-msvc.exe`. vlm-host stub stays
  0-byte (harmless: spawn fail = existing degrade).

### 4. Packaging (`tauri.conf.json`)
- `bundle.targets: ["nsis"]`, `installMode: currentUser`.
- `resources`: `../extension/` → `extension\`, `binaries/whisper/` → `whisper\`,
  `../models/ggml-base.en.bin` → `models\ggml-base.en.bin`. **Gotcha (hit +
  fixed 2026-08-14): a single-FILE resource mapped to a `dir/` target gets
  RENAMED to the dir name** — map file → explicit file path. externalBin unchanged.
- **Known quirk, verified safe:** NSIS per-user installs to `%LOCALAPPDATA%\Aperture`
  — the SAME dir `default_db_path()` uses, so `history.db`/`nm-token` sit next
  to `aperture.exe`. Checked the generated `installer.nsi`: uninstall deletes
  only its tracked files and ends with a NON-recursive `RMDir "$INSTDIR"` —
  the DB survives updates and uninstall (only the explicit "Delete app data"
  checkbox removes it). Separating the dirs (data → `%APPDATA%\Aperture` with a
  migration) is a candidate for a future session, not urgent.
- Icon: real multi-size set generated from a drawn 1024px aperture-shutter mark
  (`tauri icon`; source script in the session scratchpad). Replaces the 766-byte
  placeholder. Keep `src-tauri/icons/icon.ico` stable.
- `@tauri-apps/cli` is a ui devDependency; build from repo root with
  `ui\node_modules\.bin\tauri.cmd build`.
- `main::models_dir()` (embedder cache): repo `models\` only when the NOMIC
  cache subdir exists there (dev); else `%LOCALAPPDATA%\Aperture\models`
  (fastembed auto-downloads once). Never bare-CWD-relative — and the installed
  `models\` (whisper resource) must NOT be mistaken for the fastembed cache.

### 5. UI polish
- Dashboard Overview: "Start Aperture when I sign in to Windows" toggle
  (`.dash__setting`). Voice tab: honest copy now says transcription runs
  locally (it does); stale hardcoded `Ctrl+Win+Space` empty-state hint removed;
  `useful_rating` 👍/👎 render bug fixed.
- First-run extension step: points at the installed `extension\` folder +
  chrome://extensions steps (was a repo-relative path).

## Second wave (same day, "finish everything left for v1") — ALL FOUR big items closed

### A. sqlcipher at-rest encryption — DONE (M9 criterion closed)
- Strawberry Perl + NASM installed (winget); `aperture` now DEFAULTS the
  `sqlcipher` feature (`src-tauri` features → `aperture-db/sqlcipher`).
  Build hosts must have `C:\Strawberry\perl\bin` + `%ProgramFiles%\NASM` on
  PATH (vendored OpenSSL compile, one-time per profile).
- **Plaintext→encrypted migration** (`crates/db`): `open_encrypted` detects the
  SQLite header magic, converts via `sqlcipher_export` into a temp file,
  swaps, and deletes the plaintext original only AFTER the keyed open verifies
  the ciphertext reads back. Crash-safe ordering; tested
  (`sqlcipher_tests::plaintext_db_is_migrated_to_sqlcipher_on_open`, wrong-key
  test). The REAL dev DB migrates on first launch of the new build.
- **M9 gate: 9/9 green** with `--features sqlcipher`, including
  `m9_db_is_unreadable_without_the_key`. Two gate fixes for cargo feature
  unification (equality→implication; purge sentinel scan is build-aware — in
  an encrypted build the sentinel must be unreadable BEFORE purge too).

### B. VLM backend — DONE and verified live on the RTX 5060 (8 GB)
- Downloads (git-ignored): llama.cpp CUDA-13.3 win-x64 + cudart DLLs →
  `src-tauri\binaries\llama\` (~700 MB); Qwen2.5-VL-3B-Instruct Q4_K_M +
  mmproj-f16 (ggml-org HF repo) → `models\qwen2.5-vl-3b-{q4_k_m,mmproj-f16}.gguf`
  (sizes verified byte-exact vs HF).
- `SidecarConfig` gained `llama_bin`; the VlmHost spawn passes `--llama-bin`;
  `main::sidecar_config()` resolves llama/weights for dev + installed layouts.
  Weights CANNOT ship in NSIS (2 GB cap) — on this box they are HARDLINKED into
  `%LOCALAPPDATA%\Aperture\{models,llama}`; other machines fetch out-of-band.
- **Two real bugs found live and fixed in vlm-host**: (1) the system prompt
  said "match the schema" but never stated the schema — the model could not
  know the keys; it is now spelled out inline. (2) strict `f32` confidence
  rejected `"confidence":"High"` — now coerced (high/medium/low → 0.9/0.6/0.3).
- **Verified end-to-end**: synthetic Excel-like screenshot → `/infer` returned
  a perfect scene JSON (every dollar figure read, `document` hint). Timings:
  914 image tokens prefilled at ~912 tok/s, decode ~93 tok/s. VRAM: 4.7 GB
  resident → **439 MB within 3 s of kill** — the SC6 release invariant
  demonstrated on hardware (harness body in `sc6_vram_release.rs` remains
  `todo!()`; these figures are the recorded evidence).

### C. MCP stdio server + gated search — DONE (deferred-M7 closed)
- **`aperture-mcp`** (new bin in the reasoning-gateway crate — its stdout IS an
  egress surface, so it lives in the emitter crate): newline-delimited JSON-RPC
  2.0; `initialize`/`ping`/`tools/list` answered locally (tool schemas single-
  sourced in `transports/mcp.rs::tool_descriptors`), `tools/call` bridged over
  named pipe `\\.\pipe\aperture-mcp-v1` to the running app. BOM-tolerant.
  Verified standalone: initialize → tools/list (4 tools) → graceful
  "Aperture is not running" call error.
- **`src-tauri/src/mcp_bridge.rs`** (the GATE): `aperture_get_context` releases
  ONLY approved sessions (content-bound hash re-checked), audits `cloud_send`
  BEFORE returning (audit-write failure = payload NOT released), consumes the
  session; `aperture_list_recent` = metadata only; `aperture_search_history`
  (ADR-037 — **gating shape decided: per-query approval**) runs LIKE retrieval
  over `redaction_flags = 0` rows only, redacts via the user's terms, STAGES
  the results as a preview session, pops the panel (`preview_request` event →
  primary overlay), and returns only a staging notice — Claude then fetches via
  `aperture_get_context` after the user's explicit approval;
  `aperture_submit_suggestions` schema-checks + connector-validates (rendering
  as bubbles stays deferred; validation verdict returned).
- The preview panel is transport-aware: `transport_target ==
  "claude-desktop-mcp"` renders **"Approve for Claude"** (approve-only, no push
  send). `main::register_mcp_server()` merges the resolved `aperture-mcp.exe`
  path into `%APPDATA%\Claude\claude_desktop_config.json` at startup (never
  overwrites an unparseable config). `aperture-mcp` ships via externalBin.

### D. Multi-monitor render slimming — DONE
`App.tsx` gates on `getCurrentWindow().label === "overlay"`: secondary clones
render bubbles + voice only; HUD/panels/first-run are primary-only; the preview
panel renders on whichever monitor opened it (MCP-staged previews target the
primary via `emit_preview_request`).

## ⚠️ SESSION END STATE (2026-08-15, read FIRST next session)

The user hit the session limit mid-fix-application. Exact state:

- **36-agent review ran: 31 CONFIRMED findings** — full list + fix sketches in
  `review-findings-2026-08-14.md`. Two HIGH privacy bugs in the MCP gate
  (search-history oracle leak; get_context transport binding), one HIGH data-loss
  crash window in the SQLCipher migration, one HIGH sidecar tree-kill gap
  (killing a host orphans the llama/whisper grandchild — use Job Objects;
  also fixes tray-Quit orphaning), one HIGH: STT WAV-as-JSON exceeds axum's
  2 MB default body limit (utterances >~18 s FAIL — add DefaultBodyLimit to
  stt-host/vlm-host routers).
- **Fixed in the WORKING TREE (committed 2026-08-15) but NOT yet in the
  installed app**: the MCP "Approve for Claude" self-cancel (App.tsx onClose({})
  — was killing the approval it just made), CREATE_NO_WINDOW on all three
  sidecar spawn sites (terminal-flash fix — this IS in the installed build),
  dark near-opaque glass tokens (also installed).
- **Installed build** = CREATE_NO_WINDOW + dark glass + adaptive VAD + gain;
  it still has the broken MCP approve and all other review findings.
- **NEXT SESSION, in order**: (1) apply the 5 HIGH fixes + the quick mediums
  per review-findings doc (triage notes: constant-response + LIKE-escape +
  per-search audit row for search_history; migration resume-from-.encrypting
  + gated backup deletion; Job Objects in os_spawn; body limits; transport
  binding in get_context/list_recent), (2) rebuild sidecars + `tauri build`
  (NEVER bare cargo build into the install dir — dev-cfg bricks the screen),
  (3) reinstall + verify, (4) work the completeness list (snooze UI, bubble
  menu no-ops, 👍/👎 thumbs, MCP submit→bubbles, enrichment stubs — remove or
  implement, gate harnesses).
- **v2 prep is DONE**: `v2-kickoff-2026-08-14.md` maps shipped v1 onto Doc 22,
  marks overtaken assumptions + which Q-V2 questions have empirical answers.

## Still open after this session

1. **Gate harnesses** SC3/SC4/PresentMon/SC5-byte-monitor + `sc6` bodies stay
   `todo!()`/`#[ignore]` — the SC6/SC4 PROPERTIES are now demonstrated live
   (numbers above + STT 0.93-conf/0.8 s) but not yet automated.
2. GPU STT (faster-whisper/CUDA) still [VERIFY]; CPU base.en ships.
3. MCP submit→bubble rendering (suggestions currently validate + acknowledge).
4. VLM weights distribution for OTHER machines (installer can't carry 3 GB).

## Practical notes

- Dev launch unchanged: `npm --prefix ui run dev` + `cargo run -p aperture`.
- Installer output: `target\release\bundle\nsis\` (workspace root — the Tauri
  crate builds into the shared workspace target dir).
- The installed app shares `%LOCALAPPDATA%\Aperture\history.db` with dev runs —
  consent, patterns, exclusions carry over; expect capture to come up ON at
  first installed launch (stored consent) and autostart to self-enable.
- Rebuild stt-host after editing it: `cargo build --release -p aperture-stt-host`
  then re-copy to `src-tauri\binaries\stt-host-x86_64-pc-windows-msvc.exe`.
