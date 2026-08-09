//! Consent state (doc 13 §8).
//!
//! The consent rules, all [ASSUMPTION] in the spec (re-evaluate after dogfood):
//!   - **Capture defaults OFF** until the user explicitly opts in on first run.
//!     The indicator is always truthful (doc 05 §5); INVARIANT (3): toggling OFF
//!     releases capture and kills sidecars (VRAM->~0 in <3 s) — that mechanism
//!     lives in capture/orchestration; this struct holds the *state* and gates it.
//!   - **Every cloud send is approved — individually, or under a scoped allow
//!     (ADR-026, supersedes R1's "no always-allow").** A scoped allow is per
//!     app+intent and only automates the *Send click*: the exact payload is
//!     still displayed, a cancel window (default 3 s) still precedes egress,
//!     and the SHA-256 is still audit-logged. `ContextPayload::user_approved`
//!     is set by the preview panel on explicit Send OR by an active scoped
//!     allow whose cancel window elapsed. Scoped-allow state lands at M7.
//!   - **Voice is opt-in at first PTT** (mic permission flow).
//!
//! INVARIANT (2): consent is local state. It never reaches across the cloud
//! boundary; it only governs whether local capture runs and whether the gateway
//! is permitted to act on an approved payload.

use serde::{Deserialize, Serialize};

use crate::audit_log::{CaptureToggleRecord, ToggleReason};
use crate::PrivacyError;

/// The persisted first-run / consent state (stored in the encrypted settings
/// table, doc 13 §6). Additive-only per the compatibility law (doc 15 §6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentState {
    /// Has the user completed the first-run consent flow at all?
    #[serde(default)]
    pub first_run_completed: bool,

    /// Master capture gate. **Defaults to `false`** (OFF until opt-in, doc 13 §8).
    /// Mirrored by the truthful indicator (doc 05 §5).
    #[serde(default)]
    pub capture_enabled: bool,

    /// Has the user granted microphone access for voice? Opt-in at first PTT
    /// (doc 13 §8); until `true`, PTT prompts the OS mic-permission flow.
    #[serde(default)]
    pub voice_opt_in: bool,

    /// epoch ms of the capture opt-in (audit/UX); `None` if never enabled.
    #[serde(default)]
    pub capture_opt_in_ts: Option<i64>,
}

impl Default for ConsentState {
    /// First-run defaults: nothing consented, capture OFF (doc 13 §8).
    fn default() -> Self {
        Self {
            first_run_completed: false,
            capture_enabled: false,
            voice_opt_in: false,
            capture_opt_in_ts: None,
        }
    }
}

impl ConsentState {
    /// May local capture run right now? Equals [`Self::capture_enabled`]; the
    /// single source of truth the capture layer and indicator read (INVARIANT 3).
    pub fn capture_allowed(&self) -> bool {
        self.capture_enabled
    }

    /// May voice/STT capture audio? Requires the first-PTT opt-in **and** capture
    /// to be enabled (doc 13 §8).
    pub fn voice_allowed(&self) -> bool {
        self.voice_opt_in && self.capture_enabled
    }
}

/// The `settings` key the consent blob lives under (doc 13 §6).
pub const CONSENT_SETTINGS_KEY: &str = "consent";

/// Manages reading/writing [`ConsentState`] and emitting the matching audit
/// rows (capture toggles, doc 13 §3). Persists to the encrypted settings table.
pub struct ConsentManager {
    state: ConsentState,
    db: std::sync::Arc<aperture_db::Db>,
    audit: crate::audit_log::AuditLog,
}

impl ConsentManager {
    /// Load consent state, defaulting to first-run OFF when absent (doc 13 §8).
    ///
    /// A corrupt/unparseable blob also yields the safe default (capture OFF)
    /// rather than an error: consent must fail **closed**, and an unreadable
    /// consent record is not evidence that the user consented.
    pub fn load(db: std::sync::Arc<aperture_db::Db>) -> Result<Self, PrivacyError> {
        let stored = db
            .get_setting(CONSENT_SETTINGS_KEY)
            .map_err(|e| PrivacyError::Audit(e.to_string()))?;
        let state = match stored.as_deref() {
            None => ConsentState::default(),
            Some(text) => serde_json::from_str(text).unwrap_or_else(|e| {
                tracing::warn!(%e, "consent record unparseable — failing closed to capture OFF (doc 13 §8)");
                ConsentState::default()
            }),
        };
        let audit = crate::audit_log::AuditLog::new(std::sync::Arc::clone(&db));
        Ok(Self { state, db, audit })
    }

    /// Record completion of the first-run flow and the user's capture decision.
    /// Enabling here also stamps a `capture_toggle` audit row
    /// ([`crate::audit_log::ToggleReason::Consent`], doc 13 §3).
    pub fn complete_first_run(&mut self, enable_capture: bool, now_ms: i64) -> Result<(), PrivacyError> {
        self.state.first_run_completed = true;
        self.apply_capture(enable_capture, ToggleReason::Consent, now_ms)
    }

    /// Toggle capture. On every transition, persist and write a `capture_toggle`
    /// audit row (doc 13 §3). INVARIANT (3): the OFF transition's sidecar-kill /
    /// VRAM release is performed by capture/orchestration in response to this.
    pub fn set_capture_enabled(&mut self, enabled: bool, now_ms: i64) -> Result<(), PrivacyError> {
        self.apply_capture(enabled, ToggleReason::UserAction, now_ms)
    }

    /// Re-apply the stored capture decision at startup (doc 13 §8).
    ///
    /// Distinct from [`Self::set_capture_enabled`] purely so the audit row is
    /// labelled honestly: this is **stored consent taking effect**, not a fresh
    /// user action, and a trail that calls a boot-time restore "user_action"
    /// misreports when the user actually did something.
    pub fn restore_capture(&mut self, now_ms: i64) -> Result<(), PrivacyError> {
        self.apply_capture(true, ToggleReason::Consent, now_ms)
    }

    /// Shared body of the two capture-decision entry points. Persists first, then
    /// audits: a row claiming capture flipped must never outlive a failed write.
    ///
    /// The audit row is written on **every** call, not only on a state *change* —
    /// "when was it watching?" is answered by the trail, and a re-affirmed ON
    /// (e.g. re-enabling after a failed start) is real user activity.
    fn apply_capture(
        &mut self,
        enabled: bool,
        reason: ToggleReason,
        now_ms: i64,
    ) -> Result<(), PrivacyError> {
        let previous = self.state.clone();
        self.state.capture_enabled = enabled;
        if enabled && self.state.capture_opt_in_ts.is_none() {
            self.state.capture_opt_in_ts = Some(now_ms);
        }
        // Roll back on a failed write. Otherwise in-memory state and the settings
        // row disagree, and the NEXT launch reads the stale row — e.g. a failed
        // OFF persist leaves `capture_enabled: true` on disk, so `restore_capture`
        // silently turns capture back on against the user's last instruction.
        if let Err(e) = self.persist() {
            self.state = previous;
            return Err(e);
        }
        self.audit.record_capture_toggle(CaptureToggleRecord { enabled, reason, ts: now_ms })
    }

    /// Grant microphone/voice consent at first PTT (doc 13 §8).
    pub fn grant_voice(&mut self) -> Result<(), PrivacyError> {
        self.state.voice_opt_in = true;
        self.persist()
    }

    /// The current state (read-only view for the indicator / UI).
    pub fn state(&self) -> &ConsentState {
        &self.state
    }

    fn persist(&self) -> Result<(), PrivacyError> {
        let text = serde_json::to_string(&self.state)?;
        self.db
            .set_setting(CONSENT_SETTINGS_KEY, &text)
            .map_err(|e| PrivacyError::Audit(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn db() -> Arc<aperture_db::Db> {
        Arc::new(aperture_db::Db::open_in_memory().expect("db"))
    }

    #[test]
    fn first_run_defaults_to_everything_off() {
        let mgr = ConsentManager::load(db()).expect("load");
        assert!(!mgr.state().first_run_completed);
        assert!(!mgr.state().capture_allowed(), "capture is OFF until opt-in (doc 13 §8)");
        assert!(!mgr.state().voice_allowed());
    }

    #[test]
    fn consent_survives_a_reload_and_audits_the_transition() {
        let db = db();
        {
            let mut mgr = ConsentManager::load(Arc::clone(&db)).expect("load");
            mgr.complete_first_run(true, 1_000).expect("complete");
        }
        let mgr = ConsentManager::load(Arc::clone(&db)).expect("reload");
        assert!(mgr.state().first_run_completed);
        assert!(mgr.state().capture_allowed(), "the decision is durable");
        assert_eq!(mgr.state().capture_opt_in_ts, Some(1_000));

        let audit = crate::audit_log::AuditLog::new(db);
        let rows = audit.recent(10).expect("audit");
        assert_eq!(rows.len(), 1, "the opt-in is on the audit trail");
        assert_eq!(rows[0].payload["reason"], serde_json::json!("consent"));
        assert_eq!(rows[0].payload["enabled"], serde_json::json!(true));
    }

    #[test]
    fn toggling_capture_writes_one_audit_row_per_transition() {
        let db = db();
        let mut mgr = ConsentManager::load(Arc::clone(&db)).expect("load");
        mgr.set_capture_enabled(true, 10).unwrap();
        mgr.set_capture_enabled(false, 20).unwrap();
        mgr.set_capture_enabled(true, 30).unwrap();

        let rows = crate::audit_log::AuditLog::new(db).recent(10).expect("audit");
        assert_eq!(rows.len(), 3, "every transition is on the trail (doc 13 §3)");
        assert_eq!(rows[0].ts, 30, "newest first");
        assert_eq!(rows[0].payload["reason"], serde_json::json!("user_action"));
        // The opt-in stamp records the FIRST enable, not the latest.
        assert_eq!(mgr.state().capture_opt_in_ts, Some(10));
    }

    #[test]
    fn voice_requires_both_the_mic_opt_in_and_capture() {
        let db = db();
        let mut mgr = ConsentManager::load(Arc::clone(&db)).expect("load");
        mgr.grant_voice().unwrap();
        assert!(!mgr.state().voice_allowed(), "voice needs capture ON too (doc 13 §8)");
        mgr.set_capture_enabled(true, 1).unwrap();
        assert!(mgr.state().voice_allowed());

        // And it survives a reload.
        let reloaded = ConsentManager::load(db).expect("reload");
        assert!(reloaded.state().voice_opt_in);
    }

    #[test]
    fn a_corrupt_consent_record_fails_closed() {
        let db = db();
        db.set_setting(CONSENT_SETTINGS_KEY, "{ not json").unwrap();
        let mgr = ConsentManager::load(db).expect("load must not error");
        assert!(
            !mgr.state().capture_allowed(),
            "an unreadable consent record is NOT evidence of consent"
        );
    }
}

// NOTE (doc 13 §8, ADR-026): a **scoped allow** (per app+intent) IS sanctioned in
// v1 — but it only automates the Send click. The invariant-preserving parts are
// non-negotiable: exact payload still rendered, cancel window still precedes
// egress, SHA-256 still audited. A *silent* always-allow (no preview/cancel)
// remains forbidden — that would breach the transparency gate (INVARIANT 2).
// The ScopedAllow state/API lands at M7 alongside the gateway.
