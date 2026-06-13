//! Consent state (doc 13 §8).
//!
//! The consent rules, all [ASSUMPTION] in the spec (re-evaluate after dogfood):
//!   - **Capture defaults OFF** until the user explicitly opts in on first run.
//!     The indicator is always truthful (doc 05 §5); INVARIANT (3): toggling OFF
//!     releases capture and kills sidecars (VRAM->~0 in <3 s) — that mechanism
//!     lives in capture/orchestration; this struct holds the *state* and gates it.
//!   - **Every cloud send is individually approved — no "always allow" in v1.**
//!     Approval is per-payload, set only by the preview panel
//!     (`ContextPayload::user_approved`); this module exposes no "remember me".
//!   - **Voice is opt-in at first PTT** (mic permission flow).
//!
//! INVARIANT (2): consent is local state. It never reaches across the cloud
//! boundary; it only governs whether local capture runs and whether the gateway
//! is permitted to act on an approved payload.

use serde::{Deserialize, Serialize};

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

/// Manages reading/writing [`ConsentState`] and emitting the matching audit
/// rows (capture toggles, doc 13 §3). Persists to the encrypted settings table.
pub struct ConsentManager {
    // TODO(M9): hold ConsentState + a settings persistence handle + an AuditLog.
}

impl ConsentManager {
    /// Load consent state, defaulting to first-run OFF when absent (doc 13 §8).
    pub fn load() -> Self {
        // TODO(M9): read from encrypted settings; Default::default() on miss.
        todo!("M9: load consent state, default capture OFF (doc 13 §8)")
    }

    /// Record completion of the first-run flow and the user's capture decision.
    /// Enabling here also stamps a `capture_toggle` audit row
    /// ([`crate::audit_log::ToggleReason::Consent`], doc 13 §3).
    pub fn complete_first_run(&mut self, _enable_capture: bool) {
        // TODO(M9): set first_run_completed; apply capture decision via
        // set_capture_enabled; persist.
        todo!("M9: complete first-run consent (doc 13 §8)")
    }

    /// Toggle capture. On every transition, persist and write a `capture_toggle`
    /// audit row (doc 13 §3). INVARIANT (3): the OFF transition's sidecar-kill /
    /// VRAM release is performed by capture/orchestration in response to this.
    pub fn set_capture_enabled(&mut self, _enabled: bool) {
        // TODO(M9): update state + capture_opt_in_ts; persist; audit_log
        // .record_capture_toggle(...).
        todo!("M9: set capture enabled + audit (doc 13 §3, §8, invariant 3)")
    }

    /// Grant microphone/voice consent at first PTT (doc 13 §8).
    pub fn grant_voice(&mut self) {
        // TODO(M9): set voice_opt_in = true; persist.
        todo!("M9: voice opt-in at first PTT (doc 13 §8)")
    }

    /// The current state (read-only view for the indicator / UI).
    pub fn state(&self) -> &ConsentState {
        // TODO(M9): return &self.state.
        todo!("M9: expose current consent state (doc 13 §8)")
    }
}

// NOTE (doc 13 §8): there is deliberately **no** "always allow cloud send" API in
// v1 — per-send approval is enforced by `ContextPayload::user_approved`, which
// only the preview panel may set and only the gateway may read. Adding a
// remember-me here would breach the transparency gate (INVARIANT 2).
