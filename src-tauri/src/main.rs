//! Aperture Tauri v2 shell — the composition root (doc 02 §2, doc 16 M0).
//!
//! Responsibilities, in startup order:
//! 1. init tracing (local-only; never logs payload contents, doc 13).
//! 2. open the history DB (doc 03). At-rest encryption keys on
//!    `aperture-privacy::key_manager` at **M9** — until then the DB opens with
//!    the M9-shaped call and an empty key, exactly as `Db::open_encrypted`
//!    documents (the file sits under the user profile with default ACLs).
//! 3. `EventBus` — the in-process notify channel (doc 15 §1); SQLite stays the
//!    durable form (persist-then-notify, wired via the capture EventStore seam).
//! 4. `OrchestratedSystem` — ToggleOwner (single capture writer), GpuScheduler
//!    (single-permit mutex; jobs land at M5) (doc 12 §2).
//! 5. the Tier-0 pipeline: capture subsystem + OCR/store frame sink + the
//!    pattern-engine consumer (doc 02 §4, Critical Path A).
//! 6. `tauri::Builder.manage(AppState).invoke_handler(commands).setup(...).run()`.
//!
//! Three invariants this root preserves:
//! - 8 GB VRAM ceiling / single GPU mutex: only the OrchestratedSystem touches
//!   the GPU; the shell never spawns a sidecar itself (doc 12 §1).
//! - two-emitter transparency gate: the shell opens NO sockets and spawns NO
//!   Claude CLI — only the reasoning gateway does (doc 13 §2, wired M7).
//! - capture toggle: capture starts OFF and stays off until the user opts in
//!   (doc 13 §8); OFF releases capture, VRAM -> ~0 in < 3 s (doc 12 §6).

// Hide the console window on Windows release builds (overlay app, no terminal).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod app_state;
mod commands;
mod events;
mod hit_test;
mod mcp_bridge;
mod overlay;
mod pipeline;
mod tray;
mod vlm_fetch;
mod voice;

use std::sync::Arc;

use app_state::AppState;

fn main() {
    init_tracing();

    // Our subsystems spawn tokio tasks (capture drain/heartbeat, pattern task):
    // give them a runtime that outlives `main`'s scope and enter it so plain
    // `tokio::spawn` works during composition. Tauri's own loop runs on the
    // main thread alongside.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _rt_guard = rt.enter();

    // 2. history DB (doc 03) opened under the DPAPI-wrapped per-install key
    //    (doc 13 §6, M9). Key loss => the DB is unreadable by design, so a key
    //    error is fatal rather than a silent fall back to plaintext.
    let db_key = aperture_privacy::key_manager::get_or_create_key()
        .expect("read/create the DPAPI-wrapped DB key (doc 13 §6)");
    let db = Arc::new(
        aperture_db::Db::open_encrypted(aperture_db::default_db_path(), db_key.as_bytes())
            .expect("open history DB"),
    );
    // `db_key` drops (and zeroizes) at the end of main; the connection already
    // holds the derived page key.
    if !db.is_encrypted() {
        tracing::warn!(
            "HISTORY DB IS NOT ENCRYPTED AT REST — this build lacks the `sqlcipher` \
             feature (doc 13 §6). Data is plaintext on disk; the M9 gate will fail."
        );
    }

    // First-run settings seeding (doc 13 §6): the settings.default.json values
    // land in the encrypted `settings` table once, on an empty table only — a
    // user's edited settings are never overwritten.
    seed_settings_if_empty(&db);
    backfill_new_settings_keys(&db);

    // Consent (doc 13 §8) — the source of truth for whether capture may run.
    // Loaded before capture is composed so a fresh install starts OFF.
    let consent = Arc::new(tokio::sync::Mutex::new(
        aperture_privacy::consent::ConsentManager::load(Arc::clone(&db))
            .expect("load consent state"),
    ));

    // Retention: enforce TTLs on startup + daily (doc 03 §6, doc 16 M2).
    spawn_retention(Arc::clone(&db));

    // 3. the bus (doc 15 §1).
    let bus = aperture_event_bus::EventBus::new();

    // 4. orchestration — capture starts OFF until consent (doc 13 §8). The
    //    lifecycle gets the RESOLVED sidecar paths (dev checkout vs installed
    //    layout) instead of the crate's bare-name defaults.
    let sidecar_config = sidecar_config();
    // Decision #30: the VLM install surface shares the spawner's resolved
    // weight paths — where `vlm_status` checks and `vlm_download` writes is,
    // by construction, where the next VLM spawn reads (no restart needed).
    let vlm_fetch = Arc::new(vlm_fetch::VlmFetchState::new(
        sidecar_config.vlm_model_gguf.clone(),
        sidecar_config.vlm_mmproj_gguf.clone(),
    ));
    let lifecycle = Arc::new(tokio::sync::Mutex::new(
        aperture_orchestration::model_lifecycle::ModelLifecycle::new(Box::new(
            aperture_orchestration::model_lifecycle::OsSpawner::new(sidecar_config),
        )),
    ));
    let orchestration = Arc::new(tokio::sync::Mutex::new(
        aperture_orchestration::OrchestratedSystem::with_runner(
            aperture_orchestration::Loadout::L1,
            aperture_orchestration::toggle_owner::CaptureState::Off,
            Arc::new(aperture_orchestration::gpu_scheduler::SidecarRunner::new()),
            Some(lifecycle),
        ),
    ));

    // Idle-unload sweep (doc 04 §5): kill sidecars idle > 60 s so their VRAM
    // returns to the budget. Inert until sidecars actually load (capture ON).
    spawn_idle_sweep(Arc::clone(&orchestration));

    // 5. Tier-0 pipeline: store seam + OCR sink + capture subsystem.
    // `current_session` mirrors the pattern engine's sessionizer outward so
    // heartbeat rows (which bypass the bus) stamp the current session (M4).
    let current_session = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let store: Arc<dyn aperture_capture::normalizer::EventStore> =
        Arc::new(pipeline::DbEventStore { db: Arc::clone(&db) });
    // One embedder, shared by the OCR ingest sink AND voice retrieval — the
    // doc 03 §5 requirement that query and ingest embeddings be comparable.
    let embedder = build_embedder();
    let sink = build_frame_sink(
        Arc::clone(&db),
        Arc::clone(&current_session),
        Arc::clone(&orchestration),
        Arc::clone(&embedder),
    );
    // Decision #20 (amending ADR-029/Q15): curated defaults (password managers,
    // banking patterns) are seeded ONCE into the durable `exclusion_list` table,
    // where the user can disable or delete them like any rule — never resurrected.
    // Must run before load_exclusions so a fresh install starts protected.
    seed_default_exclusions(&db);
    // The user's confirmed rules come from the encrypted `exclusion_list` table
    // (doc 13 §4, M9). The handle is kept: every clone shares one swappable rule
    // set, so `add_exclusion` hot-reloads the running matcher through
    // `AppState.exclusions`.
    let exclusions = load_exclusions(&db);
    let capture = aperture_capture::CaptureSubsystem::new(
        aperture_capture::CaptureConfig::default(),
        bus.clone(),
        exclusions.clone(),
        sink,
        Some(store),
    );
    // The browser-extension feed (ADR-027/028): named-pipe server for the
    // native-messaging hosts. Toggle-governed (FIX 2.1) — inert until capture ON.
    #[cfg(windows)]
    capture.spawn_nm_server();

    // The connector registry (doc 10 §1): bubble_click resolves through it;
    // the connector task captures through it (Path A step 4).
    let connectors = Arc::new(aperture_connectors::default_registry());

    // The reasoning gateway (doc 09, M7): the ONLY component that may reach the
    // network, wired with the DB-backed audit log (doc 13 §3).
    let (gateway, push_target) = build_gateway(&db, Arc::clone(&connectors));

    // Voice (doc 07, M6): the channel + handle exist now; the `!Send` subsystem
    // itself spawns on its dedicated OS thread inside Tauri's setup (it needs
    // the AppHandle for `voice_surface` events).
    let (voice_handle, voice_rx) = voice::channel();
    let voice_deps = voice::VoiceDeps {
        rx: voice_rx,
        scheduler: rt.block_on(async { orchestration.lock().await.scheduler() }),
        lifecycle: rt.block_on(async { orchestration.lock().await.lifecycle() }),
        db: Arc::clone(&db),
        embedder: Arc::clone(&embedder),
        config: voice_config(&db),
        last_transcript: Arc::clone(&voice_handle.last_transcript),
    };

    // Bubble feedback channel (doc 08 §7) + global snooze deadline (ADR-040):
    // commands write, the pattern task reads.
    let (feedback_tx, feedback_rx) = tokio::sync::mpsc::unbounded_channel();
    let snooze_until = Arc::new(std::sync::atomic::AtomicI64::new(0));

    // Settings-write notifier (decision #17's UI half): `set_settings` pings it,
    // the pattern task re-reads its block immediately instead of on the next
    // 24-hour maintenance tick. Capacity 8 is generous for a channel whose
    // traffic is "a human moved a slider"; a lagged receiver just misses one
    // ping and picks the value up on the daily re-read.
    let (settings_reload_tx, settings_reload_rx) = tokio::sync::broadcast::channel(8);

    // The capture driver + pattern task spawn inside Tauri's setup — both need
    // the AppHandle (indicator events / bubble_spec events).
    let db_for_agent = Arc::clone(&db);
    let state = AppState::new(
        bus,
        db,
        capture,
        orchestration,
        feedback_tx,
        snooze_until,
        connectors,
        consent,
        gateway,
        push_target,
        voice_handle,
        exclusions.clone(),
        vlm_fetch,
        settings_reload_tx,
        // v2 agent runtime (Doc 22): shares the DB + the live exclusion handle.
        agent::build_runtime(Arc::clone(&db_for_agent), exclusions),
    );
    run_tauri(
        state,
        feedback_rx,
        settings_reload_rx,
        current_session,
        voice_deps,
        &rt,
    );
}

/// Seed the encrypted `settings` table from `config/settings.default.json` on
/// first run only (doc 13 §6). Keyed on the `reasoning` section — written only
/// by this seed or an explicit user edit — because the table also carries the
/// `consent` row, which must not suppress seeding on an upgraded install.
/// Existing keys are never overwritten either way.
fn seed_settings_if_empty(db: &aperture_db::Db) {
    match db.get_setting("reasoning") {
        Ok(Some(_)) => return, // already seeded (or user-configured)
        Ok(None) => {}
        Err(e) => {
            tracing::error!(%e, "settings read failed; skipping seed");
            return;
        }
    }
    let seed: serde_json::Value =
        match serde_json::from_str(include_str!("../../config/settings.default.json")) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(%e, "settings.default.json failed to parse; skipping seed");
                return;
            }
        };
    let Some(map) = seed.as_object() else { return };
    for (key, value) in map {
        if key.starts_with('$') {
            continue; // $comment keys are documentation, not settings
        }
        // INSERT OR IGNORE, never upsert: a seed must not be able to overwrite
        // a user-written row under any sentinel drift.
        if let Err(e) = db.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, value.to_string()],
            )
            .map(|_| ())
        }) {
            tracing::error!(%e, key, "settings seed write failed");
        }
    }
    tracing::info!("settings seeded from config/settings.default.json (doc 13 §6)");
}

/// Add settings keys that exist in the shipped seed but not in this install's
/// stored settings — **without ever overwriting a stored value**.
///
/// [`seed_settings_if_empty`] runs once, keyed on the `reasoning` row, so an
/// install created before a key existed never receives it: `loadout.vlm_download`
/// (decision #30) and `ui.bubble_freshness_half_life_sec` (decision #5) both
/// landed this way. Code defaults mirror the seed everywhere it matters, so
/// behavior was already correct — but a Dashboard control cannot show, or let
/// the user move, a value that is not in the store. That is the real cost: a
/// setting the app honors and the settings UI cannot see.
///
/// Runs at every launch (a handful of small rows). The merge is
/// **additive-only and recursive**: a key present in the stored section is left
/// exactly as it is, at any depth, so a user's edits and a deliberately
/// different value both survive. Sections whose stored value is not an object
/// (or is unparseable) are skipped rather than repaired — guessing at a shape
/// we did not write is how a config gets silently reset.
///
/// A failure anywhere is logged and skipped: missing keys are a UI-visibility
/// problem, never a reason to fail startup.
fn backfill_new_settings_keys(db: &aperture_db::Db) {
    let seed: serde_json::Value =
        match serde_json::from_str(include_str!("../../config/settings.default.json")) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(%e, "settings.default.json failed to parse; skipping backfill");
                return;
            }
        };
    let Some(sections) = seed.as_object() else { return };

    let mut added_total = 0usize;
    for (key, seed_section) in sections {
        if key.starts_with('$') {
            continue; // $comment keys are documentation, not settings
        }
        let stored_raw = match db.get_setting(key) {
            Ok(Some(raw)) => raw,
            // Absent entirely: a whole section added in a later version. The
            // first-run seed already covers fresh installs; this covers upgrades.
            Ok(None) => {
                if let Err(e) = db.set_setting(key, &seed_section.to_string()) {
                    tracing::error!(%e, key, "settings backfill write failed");
                } else {
                    added_total += 1;
                    tracing::info!(key, "settings: added missing section from the seed");
                }
                continue;
            }
            Err(e) => {
                tracing::error!(%e, key, "settings read failed; skipping backfill for this key");
                continue;
            }
        };
        let Ok(mut stored) = serde_json::from_str::<serde_json::Value>(&stored_raw) else {
            continue; // not JSON we wrote; leave it alone
        };
        let added = backfill_missing(&mut stored, seed_section);
        if added > 0 {
            match db.set_setting(key, &stored.to_string()) {
                Ok(()) => {
                    added_total += added;
                    tracing::info!(key, added, "settings: backfilled new keys from the seed");
                }
                Err(e) => tracing::error!(%e, key, "settings backfill write failed"),
            }
        }
    }
    if added_total > 0 {
        tracing::info!(added = added_total, "settings backfill complete (upgraded install)");
    }
}

/// Recursively copy keys present in `seed` but absent from `stored`. Returns how
/// many were added. Never replaces an existing key at any depth; `$comment`
/// keys are documentation and are skipped. Pure, so the additive-only rule is
/// testable without a DB.
fn backfill_missing(stored: &mut serde_json::Value, seed: &serde_json::Value) -> usize {
    let (Some(stored_map), Some(seed_map)) = (stored.as_object_mut(), seed.as_object()) else {
        return 0;
    };
    let mut added = 0;
    for (k, v) in seed_map {
        if k.starts_with('$') {
            continue;
        }
        match stored_map.get_mut(k) {
            // Present: recurse only where BOTH sides are objects, so a scalar
            // the user changed is never touched and a type change never merges
            // two unrelated shapes.
            Some(existing) => {
                if existing.is_object() && v.is_object() {
                    added += backfill_missing(existing, v);
                }
            }
            None => {
                stored_map.insert(k.clone(), v.clone());
                added += 1;
            }
        }
    }
    added
}

/// Read one top-level settings section as JSON (missing/unparseable ⇒ `{}` —
/// callers fall back to their defaults).
fn read_settings_section(db: &aperture_db::Db, key: &str) -> serde_json::Value {
    db.get_setting(key)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// The gateway's connector-registry seam (doc 09 §4): the cloud can only
/// *suggest*; connectors — reached through this lookup — are the only actors.
struct RegistryLookup(Arc<aperture_connectors::ConnectorRegistry>);

impl aperture_reasoning_gateway::suggestion_validator::ConnectorLookup for RegistryLookup {
    fn by_type(&self, connector_type: &str) -> Option<&dyn aperture_contracts::Connector> {
        self.0.by_type(connector_type)
    }
}

/// Build the reasoning gateway from settings (doc 09 §3, NG8: transports, model
/// id, and headers come from settings — never code) and inject the DB-backed
/// audit log (doc 13 §3). Also returns the first *push* transport target in the
/// configured order — the preview's intended-transport line (MCP is pull-only).
pub(crate) fn build_gateway(
    db: &Arc<aperture_db::Db>,
    connectors: Arc<aperture_connectors::ConnectorRegistry>,
) -> (
    aperture_reasoning_gateway::Gateway,
    aperture_contracts::TransportTarget,
) {
    use aperture_contracts::{ReasoningTransport, TransportTarget};
    use aperture_reasoning_gateway::transports::api::{ApiSettings, ApiTransport};
    use aperture_reasoning_gateway::transports::cli::CliTransport;
    use aperture_reasoning_gateway::transports::mcp::McpTransport;

    let reasoning = read_settings_section(db, "reasoning");
    let str_of = |key: &str, default: &str| -> String {
        reasoning
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| default.to_string())
    };

    let order: Vec<String> = reasoning
        .get("transport_order")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| {
            // ADR-025 MCP-primary default, mirroring settings.default.json.
            vec![
                "claude-desktop-mcp".into(),
                "claude-cli".into(),
                "messages-api".into(),
            ]
        });

    // The API key comes from settings or the standard env var — never code.
    // Absent ⇒ the transport reports NeedsSetup and the order falls through.
    let api_key = reasoning
        .get("messages_api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .unwrap_or_default();

    let mut transports: Vec<Box<dyn ReasoningTransport>> = Vec::new();
    let mut push_target: Option<TransportTarget> = None;
    for name in &order {
        match name.as_str() {
            "claude-desktop-mcp" => {
                // Claude Desktop's config file, for tool registration (doc 09 §3).
                let config_path = std::env::var("APPDATA")
                    .map(|a| format!("{a}\\Claude\\claude_desktop_config.json"))
                    .unwrap_or_else(|_| "claude_desktop_config.json".to_string());
                transports.push(Box::new(McpTransport::new(config_path)));
                // pull-only: never the push target
            }
            "claude-cli" => {
                transports.push(Box::new(CliTransport::new(str_of("claude_cli_path", "claude"))));
                push_target.get_or_insert(TransportTarget::ClaudeCli);
            }
            "messages-api" => {
                transports.push(Box::new(ApiTransport::new(
                    ApiSettings {
                        endpoint: str_of(
                            "messages_api_endpoint",
                            "https://api.anthropic.com/v1/messages",
                        ),
                        model: str_of("messages_api_model", "claude-opus-5"),
                        anthropic_version: str_of("messages_api_version", "2023-06-01"),
                        beta_headers: reasoning
                            .get("messages_api_beta_headers")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        cache_ttl: str_of("messages_api_cache_ttl", "5m"),
                        max_tokens: reasoning
                            .get("messages_api_max_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1024) as u32,
                    },
                    api_key.clone(),
                )));
                push_target.get_or_insert(TransportTarget::MessagesApi);
            }
            other => tracing::warn!(transport = other, "unknown transport in settings order"),
        }
    }
    tracing::info!(?order, "reasoning gateway composed (doc 09 §3)");

    let gateway = aperture_reasoning_gateway::Gateway::new(
        transports,
        Box::new(RegistryLookup(connectors)),
    )
    .with_audit(Arc::new(aperture_privacy::audit_log::AuditLog::new(
        Arc::clone(db),
    )));
    (gateway, push_target.unwrap_or(TransportTarget::ClaudeCli))
}

/// Voice settings (doc 07 §2-§3): PTT chord + the STT model label, from the
/// seeded settings with the crate defaults as fallback.
fn voice_config(db: &aperture_db::Db) -> aperture_voice::VoiceConfig {
    let voice = read_settings_section(db, "voice");
    let loadout = read_settings_section(db, "loadout");
    let mut config = aperture_voice::VoiceConfig::default();
    if let Some(chord) = voice.get("ptt_hotkey").and_then(|v| v.as_str()) {
        config.chord = aperture_voice::hotkey::HotkeyChord { spec: chord.to_string() };
    }
    if let Some(model) = loadout.get("stt_model").and_then(|v| v.as_str()) {
        config.stt_model = model.to_string();
    }
    config
}

/// Re-apply the persisted capture decision at startup (doc 13 §8, M9).
///
/// `capture_enabled` is durable consent, not a per-session flag: a user who
/// turned capture on and then rebooted expects it on. Without this the shell
/// would silently drop back to OFF every launch while the stored state still
/// said ON — a quiet disagreement between what the user chose and what runs.
///
/// The restore is stamped on the audit trail, because the trail's job is to
/// answer "when was it watching?" and this genuinely is a watching window.
/// `ToggleReason::Consent` is the right label: it is the stored consent taking
/// effect, not a fresh user action.
///
/// Capture never starts when consent says OFF — including a first run, where the
/// default is OFF and this is a no-op.
fn restore_capture(state: &AppState) {
    let consent = Arc::clone(&state.consent);
    let orchestration = Arc::clone(&state.orchestration);
    tauri::async_runtime::spawn(async move {
        let mut consent = consent.lock().await;
        if !consent.state().capture_allowed() {
            return;
        }
        if let Err(e) = consent.restore_capture(pipeline::epoch_ms()) {
            // Don't start watching if we could not record that we started.
            tracing::error!(%e, "could not audit the capture restore — leaving capture OFF");
            return;
        }
        drop(consent); // never hold the consent lock across the toggle lock
        tracing::info!("restoring capture from stored consent (doc 13 §8)");
        orchestration.lock().await.toggle().turn_on().await;
    });
}

/// Register Aperture's MCP server in Claude Desktop's config (doc 09 §3) with
/// the RESOLVED `aperture-mcp.exe` path — installed (next to aperture.exe) or
/// dev (same target dir). Merge-only and best-effort: no Claude Desktop on the
/// box just means the MCP transport reports NeedsSetup.
fn register_mcp_server() {
    let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let command = exe_dir.join("aperture-mcp.exe");
    if !command.exists() {
        tracing::warn!(path = %command.display(), "aperture-mcp.exe not found — MCP registration skipped");
        return;
    }
    let config_path = std::env::var("APPDATA")
        .map(|a| format!("{a}\\Claude\\claude_desktop_config.json"))
        .unwrap_or_else(|_| "claude_desktop_config.json".to_string());
    let transport =
        aperture_reasoning_gateway::transports::mcp::McpTransport::new(config_path.clone());
    match transport.register_with_command(&command.to_string_lossy()) {
        Ok(()) => tracing::info!(config = %config_path, "MCP server registered with Claude Desktop"),
        Err(e) => tracing::warn!(%e, "MCP registration failed (Claude Desktop absent?)"),
    }
}

/// Re-assert the stored start-at-login choice at every launch (doc 13 §8 spirit:
/// mechanism follows the recorded decision).
///
/// `ui.autostart` present → drive the registry to match (re-enabling repairs a
/// moved/updated exe path). Absent → an install that predates the setting: the
/// product contract is "present from login", so default ON — release builds
/// only, because a dev run must never write a target\debug path into HKCU Run.
/// First runs also land here as a no-op (first-run completion writes the row).
fn sync_autostart(app: &tauri::AppHandle, state: &AppState) {
    let pref = read_settings_section(&state.db, "ui")
        .get("autostart")
        .and_then(|v| v.as_bool());
    let desired = match pref {
        Some(v) => v,
        None => {
            if cfg!(debug_assertions) {
                return;
            }
            let first_run_done = tauri::async_runtime::block_on(async {
                state.consent.lock().await.state().first_run_completed
            });
            if !first_run_done {
                return; // first-run completion opts in explicitly
            }
            true
        }
    };
    match commands::apply_autostart(app, desired) {
        Ok(()) => {
            if pref.is_none() {
                let _ = commands::persist_autostart(&state.db, desired);
            }
            tracing::info!(enabled = desired, "start-at-login synced");
        }
        Err(e) => tracing::warn!(%e, "start-at-login sync failed"),
    }
}

/// Seed the curated default exclusion rules (decision #20, amending ADR-029)
/// into the durable `exclusion_list` table, once per install.
///
/// Additive-only and never fail-open: on ANY read error we log and return —
/// existing rules are never touched, and the seed simply retries next launch.
/// The seeded flag is written only after every insert succeeds, so an
/// interrupted seed resumes idempotently (`defaults_needing_seed` skips rows
/// already present — including disabled ones, so a retry can never re-enable a
/// rule the user turned off). Once the flag is set the seeder never runs again:
/// a default the user deletes stays deleted.
fn seed_default_exclusions(db: &aperture_db::Db) {
    use aperture_capture::exclusion::{defaults_needing_seed, EXCLUSION_DEFAULTS_SEEDED_KEY};
    let seeded = match db.get_setting(EXCLUSION_DEFAULTS_SEEDED_KEY) {
        Ok(flag) => flag.is_some(),
        Err(e) => {
            tracing::error!(%e, "exclusion-defaults seed: flag read failed; retrying next launch");
            return;
        }
    };
    let rows = match db.read_exclusion_list() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(%e, "exclusion-defaults seed: list read failed; retrying next launch");
            return;
        }
    };
    let pending = defaults_needing_seed(seeded, &rows);
    for (kind, pattern) in &pending {
        if let Err(e) = db.add_exclusion_rule(kind, pattern) {
            // Flag deliberately NOT written: the remaining rows seed next launch.
            tracing::error!(%e, kind, pattern, "exclusion-defaults seed: insert failed; will retry");
            return;
        }
    }
    if let Err(e) = db.set_setting(EXCLUSION_DEFAULTS_SEEDED_KEY, "\"2026-08-16\"") {
        tracing::error!(%e, "exclusion-defaults seed: flag write failed; will retry (idempotent)");
        return;
    }
    if !pending.is_empty() {
        tracing::info!(count = pending.len(), "default exclusion rules seeded (decision #20)");
    }
}

/// Compile the user's confirmed exclusion rules out of the encrypted
/// `exclusion_list` table into the capture gate's matcher (doc 13 §4, M9).
///
/// Each row is one `(match_kind, pattern)` pair; disabled rows are skipped.
///
/// **A read failure is FATAL, deliberately.** Falling back to the empty default
/// would silently run the whole session with zero exclusions — the user's
/// excluded apps would start being captured, with a single `tracing::error!`
/// line as the only signal, and no recovery until restart (the list is read
/// once). "Capture is OFF at startup" is no defence: the user can enable it at
/// any point in that same session. Refusing to start is the only outcome that
/// cannot silently weaken a protection the user configured.
fn load_exclusions(db: &aperture_db::Db) -> aperture_capture::exclusion::ExclusionList {
    use aperture_capture::exclusion::{rules_from_rows, ExclusionList};
    let rows = db.read_exclusion_list().unwrap_or_else(|e| {
        panic!(
            "could not read the exclusion list ({e}) — refusing to start rather than run \
             with the user's protections silently dropped (doc 13 §4)"
        )
    });
    let rules = rules_from_rows(rows);
    tracing::info!(count = rules.len(), "exclusion rules loaded (doc 13 §4)");
    ExclusionList::compile(rules)
}

/// Local-only structured logging (doc 13). Never logs payload contents or wire
/// bytes — only metadata + audit summaries.
fn init_tracing() {
    tracing_subscriber::fmt().with_env_filter("aperture=info").init();
}

/// The embedding backend (doc 03 §5): nomic-embed-text-v1.5 by default (weights
/// in `models/`; `--no-default-features` or a failed load falls back to the
/// non-semantic HashEmbedder dev path). ONE instance is shared by ingest and
/// voice retrieval so their vectors are comparable.
fn build_embedder() -> Arc<dyn aperture_embedding::Embedder> {
    #[cfg(feature = "nomic")]
    let embedder: Arc<dyn aperture_embedding::Embedder> = {
        match aperture_embedding::NomicEmbedder::load(models_dir()) {
            Ok(e) => Arc::new(e),
            Err(e) => {
                tracing::error!(%e, "nomic backend failed; falling back to HashEmbedder");
                Arc::new(aperture_embedding::HashEmbedder)
            }
        }
    };
    #[cfg(not(feature = "nomic"))]
    let embedder: Arc<dyn aperture_embedding::Embedder> =
        Arc::new(aperture_embedding::HashEmbedder);
    tracing::info!(backend = embedder.id(), "embedding backend");
    embedder
}

/// Resolve the sidecar binaries + STT weights for both layouts (doc 12 §5):
/// **installed** — everything sits next to `aperture.exe` (`stt-host.exe` from
/// externalBin, `whisper\` + `models\` from resources) — and **dev** — the
/// workspace target dir (cargo puts `aperture-stt-host.exe` beside
/// `aperture.exe`) with `src-tauri\binaries\whisper` + `models\` under the
/// repo-root CWD. First existing candidate wins; the crate default (bare name,
/// PATH lookup) is the last resort — EXCEPT the VLM weights (decision #30):
/// when absent, their paths resolve to the canonical download destination
/// (`vlm_download_dir`) instead of the bare crate default, so the Dashboard
/// fetch lands exactly where the next spawn reads and VLM comes up without a
/// restart. Until then the spawn failure soft-degrades to OCR-only (doc 06 §6).
fn sidecar_config() -> aperture_orchestration::model_lifecycle::SidecarConfig {
    use std::path::PathBuf;
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    let pick = |cands: &[PathBuf], fallback: PathBuf| -> PathBuf {
        cands.iter().find(|p| p.exists()).cloned().unwrap_or(fallback)
    };

    let mut config = aperture_orchestration::model_lifecycle::SidecarConfig::default();
    let mut stt_bins = Vec::new();
    let mut vlm_bins = Vec::new();
    let mut whisper_bins = vec![PathBuf::from("src-tauri/binaries/whisper/whisper-server.exe")];
    let mut llama_bins = vec![PathBuf::from("src-tauri/binaries/llama/llama-server.exe")];
    let mut stt_models = vec![PathBuf::from("models/ggml-base.en.bin")];
    let mut vlm_models = vec![config.vlm_model_gguf.clone()];
    let mut vlm_mmprojs = vec![config.vlm_mmproj_gguf.clone()];
    if let Some(dir) = &exe_dir {
        stt_bins.push(dir.join("stt-host.exe")); // installed: triple stripped by the bundler
        stt_bins.push(dir.join("aperture-stt-host.exe")); // dev: same target dir
        vlm_bins.push(dir.join("vlm-host.exe"));
        vlm_bins.push(dir.join("aperture-vlm-host.exe"));
        whisper_bins.insert(0, dir.join("whisper").join("whisper-server.exe"));
        llama_bins.insert(0, dir.join("llama").join("llama-server.exe"));
        stt_models.push(dir.join("models").join("ggml-base.en.bin"));
        // The VLM weights are ~3 GB — too big for the NSIS installer (2 GB
        // cap), so installed layouts fetch them via the Dashboard (decision #30).
        vlm_models.push(dir.join("models").join(vlm_fetch::VLM_MODEL_FILE));
        vlm_mmprojs.push(dir.join("models").join(vlm_fetch::VLM_MMPROJ_FILE));
    }
    config.stt_host_bin = pick(&stt_bins, config.stt_host_bin);
    config.vlm_host_bin = pick(&vlm_bins, config.vlm_host_bin);
    config.whisper_bin = pick(&whisper_bins, config.whisper_bin);
    config.llama_bin = pick(&llama_bins, config.llama_bin);
    config.stt_model = pick(&stt_models, config.stt_model);
    // Weights absent ⇒ resolve to where the Dashboard download will land, so
    // the spawner's stored path becomes valid the moment the fetch finishes.
    let dl_dir = vlm_download_dir(exe_dir.as_deref());
    config.vlm_model_gguf = pick(&vlm_models, dl_dir.join(vlm_fetch::VLM_MODEL_FILE));
    config.vlm_mmproj_gguf = pick(&vlm_mmprojs, dl_dir.join(vlm_fetch::VLM_MMPROJ_FILE));
    if !config.vlm_model_gguf.exists() || !config.vlm_mmproj_gguf.exists() {
        // Not silent (decision #30): the Dashboard mirrors this as the
        // "OCR-only mode" notice with the download button.
        tracing::warn!(
            "VLM weights not installed — screen understanding runs OCR-only until the \
             Dashboard download completes (decision #30)"
        );
    }
    tracing::info!(
        stt_host = %config.stt_host_bin.display(),
        whisper = %config.whisper_bin.display(),
        stt_model = %config.stt_model.display(),
        vlm_host = %config.vlm_host_bin.display(),
        llama = %config.llama_bin.display(),
        vlm_model = %config.vlm_model_gguf.display(),
        "sidecar paths resolved"
    );
    config
}

/// The canonical VLM weight destination (decision #30): the installed layout's
/// `models\` next to the exe when it exists (absolute — correct regardless of
/// how the app was launched: Start menu CWD = install dir, autostart CWD =
/// system32), else the checkout's `models/` (the dev path — cargo runs from
/// the repo root, and `target\debug\models` never exists).
fn vlm_download_dir(exe_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    if let Some(dir) = exe_dir {
        let installed = dir.join("models");
        if installed.is_dir() {
            return installed;
        }
    }
    std::path::PathBuf::from("models")
}

/// Where the embedding weights live (doc 03 §5): the repo's `models/` when run
/// from a checkout (dev), else a stable per-user dir next to the history DB.
/// NEVER bare-relative to an arbitrary CWD — a Start-menu or autostart launch
/// runs with a CWD we don't control; fastembed downloads into an empty dir on
/// the first packaged run and caches thereafter.
#[cfg(feature = "nomic")]
fn models_dir() -> std::path::PathBuf {
    // Keyed on the nomic cache subdir, not the bare `models/` dir: the installed
    // layout ALSO has a `models\` next to the exe (whisper weights, a resource),
    // and treating it as the fastembed cache would split the cache by launch
    // method (Start menu CWD = install dir; autostart CWD = system32).
    let dev = std::path::PathBuf::from("models");
    if dev.join("models--nomic-ai--nomic-embed-text-v1.5").is_dir() {
        return dev;
    }
    aperture_db::default_db_path()
        .parent()
        .map(|p| p.join("models"))
        .unwrap_or(dev)
}

/// The M2 frame sink: OCR (Windows.Media.Ocr, en fallback) + the shared
/// embedder. If no OCR engine is constructible (missing language packs),
/// degrade to event-only capture (doc 06 §6) with a one-time notice.
fn build_frame_sink(
    db: Arc<aperture_db::Db>,
    current_session: Arc<std::sync::atomic::AtomicI64>,
    orchestration: Arc<tokio::sync::Mutex<aperture_orchestration::OrchestratedSystem>>,
    embedder: Arc<dyn aperture_embedding::Embedder>,
) -> Arc<dyn aperture_capture::sampler::FrameSink> {
    match aperture_vision_ocr::windows_media_ocr::WindowsMediaOcr::new("en-US") {
        Ok(engine) => Arc::new(pipeline::OcrStoreSink {
            db,
            processor: aperture_vision_ocr::FrameProcessor::new(Box::new(engine), embedder),
            current_session,
            orchestration: Some(orchestration),
        }),
        Err(e) => {
            tracing::error!(%e, "OCR engine unavailable — event-only capture (doc 06 §6)");
            Arc::new(aperture_capture::sampler::DropSink)
        }
    }
}

/// React to the orchestration toggle broadcast: ON → capture STARTING path,
/// OFF → the ≤3 s release (doc 05 §5, doc 12 §6).
///
/// The indicator is emitted from the OBSERVED outcome — never the requested
/// state (doc 13 §8: "the indicator is always truthful"). A failed start
/// reverts the single writer to Off so every reader (indicator, pattern
/// engine rule 7) agrees, and the next toggle(true) can re-broadcast a retry
/// (turn_on is idempotent only against a latched On).
fn spawn_capture_driver(
    capture: Arc<aperture_capture::CaptureSubsystem>,
    orchestration: Arc<tokio::sync::Mutex<aperture_orchestration::OrchestratedSystem>>,
    consent: Arc<tokio::sync::Mutex<aperture_privacy::consent::ConsentManager>>,
    voice_tx: tokio::sync::mpsc::UnboundedSender<voice::VoiceCmd>,
    mut rx: tokio::sync::broadcast::Receiver<aperture_orchestration::toggle_owner::CaptureState>,
    app: tauri::AppHandle,
) {
    tokio::spawn(async move {
        use aperture_orchestration::toggle_owner::CaptureState;
        loop {
            match rx.recv().await {
                Ok(CaptureState::On) => match capture.start().await {
                    Ok(()) => {
                        let _ = events::emit_capture_indicator(&app, events::CaptureIndicator::On);
                        // Voice rides the capture toggle (doc 12 §6) but only
                        // with the separate mic opt-in (doc 13 §8).
                        if consent.lock().await.state().voice_opt_in {
                            let _ = voice_tx.send(voice::VoiceCmd::Enable);
                        }
                    }
                    Err(e) => {
                        tracing::error!(%e, "capture start failed — reverting to OFF (doc 05 §7)");
                        orchestration.lock().await.toggle().turn_off().await;
                        let _ =
                            events::emit_capture_indicator(&app, events::CaptureIndicator::Off);
                    }
                },
                Ok(CaptureState::Off) => {
                    if let Err(e) = capture.stop().await {
                        // ToggleSlaBreach: force path already ran; surface once.
                        tracing::error!(%e, "capture stop breached the SLA (doc 05 §7)");
                    }
                    // OFF makes PTT inert (chord unregistered, mic released).
                    let _ = voice_tx.send(voice::VoiceCmd::Disable);
                    let _ = events::emit_capture_indicator(&app, events::CaptureIndicator::Off);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Idle-unload sweep timer (doc 04 §5): every 15 s, unload any sidecar idle for
/// more than `IDLE_UNLOAD` (60 s), returning its VRAM to the budget so the next
/// admission projects against the freed headroom. Warm-kept sidecars (M6 PTT pin)
/// are honored inside `idle_sweep`. Grabs the lifecycle handle once, then holds
/// only the lifecycle mutex per tick — never the whole orchestration facade.
fn spawn_idle_sweep(
    orchestration: Arc<tokio::sync::Mutex<aperture_orchestration::OrchestratedSystem>>,
) {
    tokio::spawn(async move {
        let lifecycle = orchestration.lock().await.lifecycle();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tick.tick().await;
            lifecycle.lock().await.idle_sweep(pipeline::epoch_ms()).await;
        }
    });
}

/// Retention on startup + a daily timer (doc 03 §6, doc 16 M2). The policy is
/// re-read from `privacy.retention_days` each pass (2026-08-15 review — the
/// settings block existed but the runtime silently used the defaults).
fn spawn_retention(db: Arc<aperture_db::Db>) {
    tokio::spawn(async move {
        loop {
            let policy = retention_policy_from_settings(&db);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            match aperture_db::retention::run_nightly_prune(&db, now, &policy) {
                Ok(report) => tracing::info!(?report, "retention prune"),
                Err(e) => tracing::error!(%e, "retention prune failed"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
        }
    });
}

/// `privacy.retention_days` from the settings store, defaults where absent or
/// invalid. A zero/negative value is rejected (a typo must never mean "delete
/// everything today").
fn retention_policy_from_settings(db: &aperture_db::Db) -> aperture_db::retention::RetentionPolicy {
    let mut policy = aperture_db::retention::RetentionPolicy::default();
    let Ok(Some(raw)) = db.get_setting("privacy") else { return policy };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else { return policy };
    let Some(days) = v.get("retention_days") else { return policy };
    let read = |key: &str, dst: &mut u32| {
        if let Some(n) = days.get(key).and_then(serde_json::Value::as_u64) {
            if n >= 1 {
                *dst = n.min(u32::MAX as u64) as u32;
            }
        }
    };
    read("events", &mut policy.events_days);
    read("ocr_text", &mut policy.ocr_text_days);
    read("voice", &mut policy.voice_days);
    read("suggestions", &mut policy.suggestions_days);
    read("audit", &mut policy.audit_days);
    policy
}

/// Build and run the Tauri app: manage [`AppState`], register the IPC command
/// contract, create the overlay + spawn the WebView forwarders in `setup`, run.
fn run_tauri(
    state: AppState,
    feedback_rx: tokio::sync::mpsc::UnboundedReceiver<(
        i64,
        aperture_pattern_engine::FeedbackEvent,
    )>,
    settings_reload_rx: tokio::sync::broadcast::Receiver<Vec<String>>,
    current_session: Arc<std::sync::atomic::AtomicI64>,
    voice_deps: voice::VoiceDeps,
    _rt: &tokio::runtime::Runtime,
) {
    let setup_state = state.clone();
    let mut voice_deps = Some(voice_deps);
    // Moved into Tauri's setup closure (which is FnMut) alongside feedback_rx.
    let mut settings_reload_rx = Some(settings_reload_rx);
    tauri::Builder::default()
        // FIRST plugin, deliberately: a second launch (autostart + a shortcut
        // double-click) must hand off to the running instance — two instances
        // would race one DB and double-run capture. The running instance
        // responds by surfacing its dashboard, so the click still "does" something.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            let _ = events::emit_dashboard_open(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent, // ignored on Windows
            None,
        ))
        .manage(state)
        .manage(hit_test::HitTestState::default())
        // Which window hosts the preview panel (decision #13 routing).
        .manage(overlay::PreviewHost::default())
        .invoke_handler(tauri::generate_handler![
            commands::toggle_capture,
            commands::list_suggestions,
            commands::bubble_click,
            commands::record_feedback,
            commands::set_snooze,
            commands::get_snooze,
            commands::request_preview,
            commands::list_trail_events,
            commands::transport_health,
            commands::preview_set_approved,
            commands::preview_cancel,
            commands::preview_send,
            commands::preview_retarget,
            commands::agent_status,
            commands::agent_start_task,
            commands::agent_decide,
            commands::agent_answer,
            commands::agent_undo_close_windows,
            commands::agent_dismiss,
            commands::agent_list_tasks,
            commands::agent_task_steps,
            commands::agent_purge_task,
            commands::voice_ptt_down,
            commands::voice_ptt_up,
            commands::voice_run_transcript,
            commands::voice_dismiss,
            commands::focus_overlay,
            commands::open_privacy,
            // Decision #13 — control surfaces follow the cursor's monitor.
            commands::open_dashboard,
            commands::set_preview_host,
            commands::get_settings,
            commands::set_settings,
            commands::get_autostart,
            commands::set_autostart,
            // Dashboard — read-only views over the local history.
            commands::dashboard_stats,
            commands::list_events,
            commands::list_patterns,
            commands::list_suggestion_history,
            // Decision #30 — VLM weight install (status + user-initiated fetch).
            commands::vlm_status,
            commands::vlm_download,
            // M9 — privacy surface (doc 13).
            commands::set_overlay_interactive,
            commands::set_hit_test_rects,
            commands::reset_overlay_interactivity,
            commands::get_consent,
            commands::complete_first_run,
            commands::grant_voice_consent,
            commands::list_audit,
            commands::purge_all,
            commands::list_exclusions,
            commands::add_exclusion,
            commands::set_exclusion,
            commands::suggest_exclusions,
        ])
        .setup(move |app| {
            use tauri::Manager;
            // Overlay setup (doc 11 §2, M8): one click-through, capture-excluded
            // window per monitor (the primary reuses the config `overlay` window).
            // A failure here degrades to the primary staying up — never fatal.
            match overlay::create_overlays(app.handle()) {
                Ok(windows) => tracing::info!(count = windows.len(), "overlays created (per-monitor)"),
                Err(e) => {
                    tracing::error!(%e, "per-monitor overlay fan-out failed; hardening the primary only");
                    if let Some(window) = app.get_webview_window(overlay::OVERLAY_LABEL) {
                        overlay::harden(&window);
                    }
                }
            }
            // Capture starts OFF (doc 13 §8) — the indicator must say so. If the
            // user previously consented AND left capture on, it is restored below,
            // AFTER the capture driver subscribes (see `restore_capture`).
            let _ = events::emit_capture_indicator(
                app.handle(),
                events::CaptureIndicator::Off,
            );
            // The capture driver + pattern-engine consumer (doc 02 §4) need
            // the app handle (indicator / bubble_spec events); spawned here.
            let (driver_rx, engine_rx) = tauri::async_runtime::block_on(async {
                let orch = setup_state.orchestration.lock().await;
                (orch.subscribe_capture(), orch.subscribe_capture())
            });
            spawn_capture_driver(
                Arc::clone(&setup_state.capture),
                Arc::clone(&setup_state.orchestration),
                Arc::clone(&setup_state.consent),
                setup_state.voice.tx.clone(),
                driver_rx,
                app.handle().clone(),
            );
            // The voice thread (doc 07, M6): dedicated OS thread for the !Send
            // subsystem — hotkey pump + mic + STT pipeline live there.
            if let Some(deps) = voice_deps.take() {
                voice::spawn(app.handle().clone(), deps);
            }
            pipeline::spawn_pattern_task(
                &setup_state.bus,
                std::sync::Arc::clone(&setup_state.db),
                engine_rx,
                feedback_rx,
                settings_reload_rx
                    .take()
                    .unwrap_or_else(|| setup_state.settings_reload_tx.subscribe()),
                Arc::clone(&setup_state.snooze_until),
                Arc::clone(&current_session),
                app.handle().clone(),
            );
            // Bubble hit-testing (doc 11 §2): the cursor poller that flips
            // click-through only while the cursor is over a published rect.
            hit_test::spawn_poller(app.handle().clone());
            // The gpu_busy forwarder (doc 11 §6, doc 14 §5) — closes the
            // events.rs TODO(M3): mutex-held → glass swaps to opaque fallback.
            {
                let mut busy_rx = tauri::async_runtime::block_on(async {
                    setup_state.orchestration.lock().await.gpu_busy()
                });
                let busy_app = app.handle().clone();
                tokio::spawn(async move {
                    loop {
                        match busy_rx.recv().await {
                            Ok(busy) => {
                                let _ = events::emit_gpu_busy(&busy_app, busy);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                });
            }
            // The system tray — the only findable/quittable handle on an app
            // whose window skips the taskbar. Non-fatal: the overlay HUD still
            // carries the same controls.
            if let Err(e) = tray::create(app.handle()) {
                tracing::error!(%e, "tray creation failed — HUD remains the only control surface");
            }
            // The MCP pipe server (doc 09 §3): Claude Desktop's tool calls land
            // here, behind the approval gate. Registration writes our resolved
            // `aperture-mcp.exe` path into claude_desktop_config.json (merge,
            // never overwrite) so a moved/updated install self-repairs.
            mcp_bridge::spawn(app.handle().clone());
            register_mcp_server();
            // Re-assert the stored start-at-login choice (repairs a moved exe;
            // upgraded installs that predate the setting default to ON).
            sync_autostart(app.handle(), &setup_state);
            // Restore the user's persisted capture decision (doc 13 §8, M9).
            // MUST run after `spawn_capture_driver`: `turn_on` broadcasts, and a
            // broadcast sent before the driver subscribes is simply lost — capture
            // would then report ON while nothing was actually running.
            restore_capture(&setup_state);
            // Connector capture (Path A step 4, doc 02 §4) — bus consumer, no
            // AppHandle needed, but spawned here with its siblings.
            pipeline::spawn_connector_task(
                &setup_state.bus,
                std::sync::Arc::clone(&setup_state.db),
                Arc::clone(&setup_state.connectors),
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Aperture overlay shell");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the backfill: a key added to the seed after this
    /// install was created must appear, so the settings UI can show it.
    #[test]
    fn a_new_seed_key_is_added() {
        let mut stored = serde_json::json!({ "bubble_dwell_sec": 20 });
        let seed = serde_json::json!({
            "bubble_dwell_sec": 20,
            "bubble_freshness_half_life_sec": 600
        });
        assert_eq!(backfill_missing(&mut stored, &seed), 1);
        assert_eq!(stored["bubble_freshness_half_life_sec"], 600);
    }

    /// The non-negotiable half: a stored value is NEVER replaced, including
    /// when the user deliberately set something other than the default.
    #[test]
    fn a_stored_value_is_never_overwritten() {
        let mut stored = serde_json::json!({ "bubble_dwell_sec": 45, "unknown_key": 1 });
        let seed = serde_json::json!({ "bubble_dwell_sec": 20 });
        assert_eq!(backfill_missing(&mut stored, &seed), 0);
        assert_eq!(stored["bubble_dwell_sec"], 45, "the user's value survives");
        assert_eq!(stored["unknown_key"], 1, "keys we no longer ship are left alone");
    }

    /// Nested blocks are where the real gaps are (`loadout.vlm_download`), and
    /// they must merge key-by-key rather than wholesale.
    #[test]
    fn nested_blocks_merge_key_by_key() {
        let mut stored = serde_json::json!({
            "vlm_download": { "model": { "url": "mine", "bytes": 1 } }
        });
        let seed = serde_json::json!({
            "vlm_download": {
                "model": { "url": "shipped", "bytes": 2 },
                "mmproj": { "url": "shipped-mm", "bytes": 3 }
            }
        });
        assert_eq!(backfill_missing(&mut stored, &seed), 1, "only mmproj is missing");
        assert_eq!(stored["vlm_download"]["model"]["url"], "mine", "not clobbered");
        assert_eq!(stored["vlm_download"]["mmproj"]["url"], "shipped-mm");
    }

    /// A type change means the two shapes are unrelated — merging them would
    /// produce a config neither side wrote.
    #[test]
    fn a_type_mismatch_is_left_alone_rather_than_merged() {
        let mut stored = serde_json::json!({ "retention_days": 90 });
        let seed = serde_json::json!({ "retention_days": { "events": 90 } });
        assert_eq!(backfill_missing(&mut stored, &seed), 0);
        assert_eq!(stored["retention_days"], 90);
    }

    #[test]
    fn comment_keys_are_documentation_not_settings() {
        let mut stored = serde_json::json!({});
        let seed = serde_json::json!({ "$comment": "explaining", "real": 1 });
        assert_eq!(backfill_missing(&mut stored, &seed), 1);
        assert!(stored.get("$comment").is_none());
    }

    /// Every section the shipped seed defines must survive a round-trip through
    /// the backfill unchanged when it is already fully present — i.e. a normal
    /// launch on a current install writes nothing.
    #[test]
    fn a_current_install_is_a_no_op() {
        let seed: serde_json::Value =
            serde_json::from_str(include_str!("../../config/settings.default.json")).unwrap();
        for (key, section) in seed.as_object().unwrap() {
            if key.starts_with('$') {
                continue;
            }
            let mut stored = section.clone();
            assert_eq!(
                backfill_missing(&mut stored, section),
                0,
                "section `{key}` should need no backfill against itself"
            );
        }
    }
}
