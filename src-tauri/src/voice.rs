//! Voice composition-root wiring (doc 07, doc 16 M6).
//!
//! [`aperture_voice::VoiceSubsystem`] is **`!Send`** (it owns a `cpal::Stream`
//! and a `GlobalHotKeyManager`), so the shell drives it on ONE dedicated OS
//! thread. That thread must also pump Win32 messages — `RegisterHotKey`
//! delivers `WM_HOTKEY` to the registering thread's message queue, and
//! `global-hotkey` translates it into its crossbeam channel from that pump.
//!
//! The loop therefore multiplexes, non-blocking, at ~60 Hz:
//!   1. the Win32 message pump (hotkey delivery),
//!   2. the chord's press/release queue (`try_next_ptt_event`),
//!   3. the shell command channel ([`VoiceCmd`] — enable/disable/PTT/run),
//!   4. the 30 s max-utterance ceiling ([`aperture_voice::MAX_UTTERANCE`]),
//!   5. the warm-keep lapse check (≥2 PTT/5 min pins STT, ADR-030/Q36).
//!
//! Async pipeline sections (`ptt_up`, retrieval) run on a current-thread tokio
//! runtime via `block_on` — one utterance at a time is the doc 07 contract, so
//! blocking the loop for its duration is correct, not a compromise.
//!
//! Invariants: this module opens no sockets and spawns nothing (two-emitter,
//! doc 13 §2); STT runs only as a GpuJob on the injected scheduler (doc 12 §1);
//! capture-toggle OFF disables the hotkey + mic in the same broadcast the
//! capture driver consumes (doc 12 §6).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aperture_orchestration::model_lifecycle::{ModelLifecycle, SidecarKind};
use aperture_orchestration::warm_keep::PttWarmKeep;
use aperture_voice::hotkey::PttEvent;
use aperture_voice::{UtteranceOutcome, VoiceConfig, VoiceSubsystem, MAX_UTTERANCE};

use crate::events;

/// Commands the shell sends the voice thread.
#[derive(Debug)]
pub enum VoiceCmd {
    /// Capture toggle ON + voice consent: register the chord, probe the mic.
    Enable,
    /// Capture toggle OFF: unregister + drop any live capture (PTT inert).
    Disable,
    /// UI-driven PTT press (the keyboard chord arrives via the hotkey queue).
    PttDown,
    /// UI-driven PTT release.
    PttUp,
    /// Confirm-chip "Run": re-issue a confirmed transcript through the query
    /// path at confidence 1.0. The utterance was ALREADY stored at STT time —
    /// this must not re-store (doc 07 §4.4).
    RunTranscript(String),
}

/// The cloneable handle `AppState` carries.
#[derive(Clone)]
pub struct VoiceHandle {
    pub tx: tokio::sync::mpsc::UnboundedSender<VoiceCmd>,
    /// The last transcript that reached a confirm chip / escalation — seeds the
    /// `answer_query` preview's `user_addition` so "Ask Claude" carries the
    /// user's actual question (doc 07 §5).
    pub last_transcript: Arc<Mutex<Option<String>>>,
}

/// Everything the voice thread needs, assembled in `main` before Tauri runs;
/// the thread itself spawns in `setup` once the `AppHandle` exists.
pub struct VoiceDeps {
    pub rx: tokio::sync::mpsc::UnboundedReceiver<VoiceCmd>,
    pub scheduler: Arc<dyn aperture_contracts::gpu_job::GpuScheduler>,
    pub lifecycle: Arc<tokio::sync::Mutex<ModelLifecycle>>,
    pub db: Arc<aperture_db::Db>,
    pub embedder: Arc<dyn aperture_embedding::Embedder>,
    pub config: VoiceConfig,
    pub last_transcript: Arc<Mutex<Option<String>>>,
}

/// Build the channel + handle pair (`main`) …
pub fn channel() -> (VoiceHandle, tokio::sync::mpsc::UnboundedReceiver<VoiceCmd>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let last_transcript = Arc::new(Mutex::new(None));
    (VoiceHandle { tx, last_transcript }, rx)
}

/// … then spawn the dedicated voice thread (`setup`, once `AppHandle` exists).
pub fn spawn(app: tauri::AppHandle, deps: VoiceDeps) {
    std::thread::Builder::new()
        .name("aperture-voice".into())
        .spawn(move || run(app, deps))
        .map(|_| ())
        .unwrap_or_else(|e| tracing::error!(%e, "voice thread failed to spawn — PTT unavailable"));
}

/// Loop cadence: fast enough that a PTT press feels instant, slow enough to be
/// invisible in a profiler.
const TICK: Duration = Duration::from_millis(16);
/// How often the warm-keep pin is re-evaluated for lapse.
const WARM_LAPSE_CHECK: Duration = Duration::from_secs(5);

fn run(app: tauri::AppHandle, mut deps: VoiceDeps) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(%e, "voice runtime failed to build — PTT unavailable");
            return;
        }
    };
    let mut vs = VoiceSubsystem::new(
        Arc::clone(&deps.scheduler),
        Arc::clone(&deps.db),
        Arc::clone(&deps.embedder),
        deps.config.clone(),
    );
    let mut warm = PttWarmKeep::new();
    let mut warm_pinned = false;
    let mut last_lapse_check = std::time::Instant::now();

    loop {
        pump_win32_messages();

        // 2. Keyboard chord transitions.
        while let Some(ev) = vs.try_next_ptt_event() {
            match ev {
                PttEvent::Down => on_ptt_down(&app, &rt, &mut vs, &mut warm, &mut warm_pinned, &deps),
                PttEvent::Up => finish_utterance(&app, &rt, &mut vs, &deps),
            }
        }

        // 3. Shell commands.
        loop {
            match deps.rx.try_recv() {
                Ok(VoiceCmd::Enable) => {
                    if vs.is_enabled() {
                        continue;
                    }
                    enable_with_fallbacks(&app, &mut vs, &deps);
                }
                Ok(VoiceCmd::Disable) => {
                    if vs.is_enabled() || vs.is_recording() {
                        vs.disable();
                        emit(&app, serde_json::json!({ "surface": "hidden" }));
                        tracing::info!("voice disabled (chord unregistered, mic released)");
                    }
                    // Voice off also drops the escalation seed — a transcript
                    // must not outlive the consent that produced it.
                    *deps
                        .last_transcript
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = None;
                }
                Ok(VoiceCmd::PttDown) => {
                    on_ptt_down(&app, &rt, &mut vs, &mut warm, &mut warm_pinned, &deps)
                }
                Ok(VoiceCmd::PttUp) => finish_utterance(&app, &rt, &mut vs, &deps),
                Ok(VoiceCmd::RunTranscript(t)) => run_transcript(&app, &rt, &deps, &t),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    tracing::info!("voice command channel closed — voice thread exiting");
                    return;
                }
            }
        }

        // 4. The 30 s ceiling ends capture regardless of key state (doc 07 §2).
        if vs.recording_elapsed().is_some_and(|el| el >= MAX_UTTERANCE) {
            tracing::info!("max utterance ceiling hit — finalizing capture");
            finish_utterance(&app, &rt, &mut vs, &deps);
        }

        // 5. Warm-keep lapse: the pin drops as presses age out (doc 12 §7).
        if warm_pinned && last_lapse_check.elapsed() >= WARM_LAPSE_CHECK {
            last_lapse_check = std::time::Instant::now();
            if !warm.is_warm(crate::pipeline::epoch_ms()) {
                warm_pinned = false;
                set_warm_kept(&rt, &deps.lifecycle, false);
            }
        }

        std::thread::sleep(TICK);
    }
}

/// Chords tried when the configured one is owned by another app (or by Windows
/// itself — Ctrl+Win+Space is the input-language switcher, observed live).
const FALLBACK_CHORDS: [&str; 3] = ["Ctrl+Alt+Space", "Ctrl+Shift+Space", "Alt+F9"];

/// Enable voice, walking a chord-fallback ladder on registration conflicts
/// (doc 07 §6). A mic failure stops the ladder — no chord fixes a missing
/// microphone. The chord that actually binds is persisted back to settings so
/// the UI shows the truth and the next launch tries the working one first.
fn enable_with_fallbacks(app: &tauri::AppHandle, vs: &mut VoiceSubsystem, deps: &VoiceDeps) {
    use aperture_voice::hotkey::HotkeyChord;
    let configured = deps.config.chord.spec.clone();
    let mut candidates = vec![configured.clone()];
    candidates.extend(
        FALLBACK_CHORDS
            .iter()
            .filter(|s| !s.eq_ignore_ascii_case(&configured))
            .map(|s| s.to_string()),
    );

    let mut last_err: Option<aperture_voice::VoiceError> = None;
    for spec in candidates {
        vs.set_chord(HotkeyChord { spec: spec.clone() });
        match vs.enable() {
            Ok(()) => {
                tracing::info!(chord = %spec, "voice enabled (chord registered, mic probed)");
                if !spec.eq_ignore_ascii_case(&configured) {
                    persist_chord(&deps.db, &spec);
                    emit(app, serde_json::json!({
                        "surface": "empty",
                        "message": format!(
                            "Voice ready — hold {spec} to talk. (Your configured hotkey \
                             {configured} is taken by another app, so it was rebound.)"
                        ),
                    }));
                }
                return;
            }
            Err(e @ aperture_voice::VoiceError::MicUnavailable(_)) => {
                last_err = Some(e);
                break; // a different chord won't produce a microphone
            }
            Err(e) => {
                tracing::warn!(%e, chord = %spec, "chord unavailable; trying the next");
                last_err = Some(e);
            }
        }
    }
    let e = last_err.map(|e| e.to_string()).unwrap_or_else(|| "unknown".into());
    tracing::error!(%e, "voice enable failed");
    emit(app, serde_json::json!({
        "surface": "empty",
        "message": format!("Voice unavailable: {e}"),
    }));
}

/// Persist the chord that actually bound into the `voice` settings section
/// (merge, not replace — the section carries other tunables).
fn persist_chord(db: &aperture_db::Db, spec: &str) {
    let mut section = db
        .get_setting("voice")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = section.as_object_mut() {
        obj.insert("ptt_hotkey".into(), serde_json::json!(spec));
    }
    if let Err(e) = db.set_setting("voice", &section.to_string()) {
        tracing::error!(%e, "could not persist the rebound PTT chord");
    }
}

/// PTT down: open the mic, show the listening pill, feed the warm-keep counter.
fn on_ptt_down(
    app: &tauri::AppHandle,
    rt: &tokio::runtime::Runtime,
    vs: &mut VoiceSubsystem,
    warm: &mut PttWarmKeep,
    warm_pinned: &mut bool,
    deps: &VoiceDeps,
) {
    if !vs.is_enabled() || vs.is_recording() {
        return;
    }
    // ≥2 presses / 5 min pins the STT sidecar against idle-unload (ADR-030/Q36).
    let now_warm = warm.record_press(crate::pipeline::epoch_ms());
    if now_warm != *warm_pinned {
        *warm_pinned = now_warm;
        set_warm_kept(rt, &deps.lifecycle, now_warm);
    }
    match vs.ptt_down() {
        Ok(()) => emit(app, serde_json::json!({ "surface": "listening" })),
        Err(e) => {
            tracing::error!(%e, "ptt_down failed");
            emit(app, serde_json::json!({
                "surface": "empty",
                "message": format!("Microphone unavailable: {e}"),
            }));
        }
    }
}

/// PTT up (or the 30 s ceiling): run the whole per-utterance pipeline and map
/// the outcome onto the `voice_surface` contract (`ui/src/lib/ipc.ts`).
fn finish_utterance(
    app: &tauri::AppHandle,
    rt: &tokio::runtime::Runtime,
    vs: &mut VoiceSubsystem,
    deps: &VoiceDeps,
) {
    if !vs.is_recording() {
        return;
    }
    // STT + intent + store can take seconds (model swap): say so, honestly.
    emit(app, serde_json::json!({ "surface": "thinking" }));
    match rt.block_on(vs.ptt_up()) {
        Ok(UtteranceOutcome::DiscardedTap) => {
            // NOT silent (user report 2026-08-14): a held-and-spoken press that
            // yields no detected speech must SAY so, or voice looks broken.
            emit(app, serde_json::json!({
                "surface": "empty",
                "message": "Didn't catch that — hold the keys, speak, then release. \
                            (If you spoke, check Windows mic permissions for desktop \
                            apps and the default microphone.)",
            }));
        }
        Ok(UtteranceOutcome::ConfirmChip { transcript }) => {
            remember(deps, &transcript);
            emit(app, serde_json::json!({
                "surface": "transcript",
                "text": transcript,
                // The chip exists BECAUSE confidence was below the 0.6 floor
                // (doc 07 §4.4); the UI renders the "Did you say…?" affordance,
                // not the number.
                "confidence": 0.0,
            }));
        }
        Ok(UtteranceOutcome::Answer(bubble)) => emit_answer(app, &bubble),
        Ok(UtteranceOutcome::EscalationDraft { transcript }) => {
            remember(deps, &transcript);
            // NEVER auto-sent (doc 07 §4.2): surface an answer card whose only
            // affordance is "Ask Claude" — that opens the preview→Send gate.
            emit(app, serde_json::json!({
                "surface": "answer",
                "title": transcript,
                "source": "ask claude — review before sending",
                "action_ref": null,
                "can_ask_claude": true,
            }));
        }
        Ok(UtteranceOutcome::StoredSilently) => {
            // Telemetry-only utterances are UI-silent by design (doc 07 §4.3),
            // but never LOG-silent — see the user report above.
            tracing::info!("utterance stored silently (telemetry intent)");
            emit(app, serde_json::json!({ "surface": "hidden" }));
        }
        Err(e) => {
            tracing::error!(%e, "utterance pipeline failed");
            emit(app, serde_json::json!({
                "surface": "empty",
                "message": format!("{e}"),
            }));
        }
    }
}

/// Confirm-chip "Run" (doc 07 §4.4): the user confirmed the words, so classify
/// at confidence 1.0 and act — WITHOUT re-storing (stored at STT time).
fn run_transcript(
    app: &tauri::AppHandle,
    rt: &tokio::runtime::Runtime,
    deps: &VoiceDeps,
    transcript: &str,
) {
    use aperture_voice::intent_classifier::{self, Intent};
    let classified = intent_classifier::classify(transcript, 1.0);
    match classified.intent {
        Intent::Escalation => {
            let query = classified
                .escalation_query
                .unwrap_or_else(|| transcript.to_string());
            remember(deps, &query);
            emit(app, serde_json::json!({
                "surface": "answer",
                "title": query,
                "source": "ask claude — review before sending",
                "action_ref": null,
                "can_ask_claude": true,
            }));
        }
        Intent::Query => {
            emit(app, serde_json::json!({ "surface": "thinking" }));
            let result = rt.block_on(aperture_voice::retrieval::run(
                &deps.db,
                deps.embedder.as_ref(),
                transcript,
                crate::pipeline::epoch_ms(),
            ));
            match result {
                Ok(bubble) => emit_answer(app, &bubble),
                Err(e) => {
                    tracing::error!(%e, "confirmed-transcript retrieval failed");
                    emit(app, serde_json::json!({
                        "surface": "empty",
                        "message": format!("{e}"),
                    }));
                }
            }
        }
        Intent::Telemetry => emit(app, serde_json::json!({ "surface": "hidden" })),
    }
}

/// Map an [`aperture_voice::AnswerBubble`] onto the `answer`/`empty` surface.
fn emit_answer(app: &tauri::AppHandle, bubble: &aperture_voice::AnswerBubble) {
    if bubble.empty_state {
        emit(app, serde_json::json!({ "surface": "empty", "message": bubble.title }));
        return;
    }
    let source = if bubble.when.is_empty() {
        bubble.source.clone()
    } else {
        format!("{} · {}", bubble.source, bubble.when)
    };
    emit(app, serde_json::json!({
        "surface": "answer",
        "title": bubble.title,
        "source": source,
        "action_ref": bubble.resume_action_ref,
        "can_ask_claude": true,
    }));
}

fn remember(deps: &VoiceDeps, transcript: &str) {
    *deps
        .last_transcript
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(transcript.to_string());
}

fn emit(app: &tauri::AppHandle, payload: serde_json::Value) {
    if let Err(e) = events::emit_voice_surface(app, &payload) {
        tracing::error!(%e, "voice_surface emit failed");
    }
}

fn set_warm_kept(
    rt: &tokio::runtime::Runtime,
    lifecycle: &Arc<tokio::sync::Mutex<ModelLifecycle>>,
    warm: bool,
) {
    rt.block_on(async {
        lifecycle.lock().await.set_warm_kept(SidecarKind::SttHost, warm);
    });
    tracing::debug!(warm, "stt warm-keep pin updated (ADR-030/Q36)");
}

/// Drain this thread's Win32 message queue so `WM_HOTKEY` reaches the
/// `global-hotkey` translator. No-op off Windows.
fn pump_win32_messages() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
        };
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
