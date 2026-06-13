//! The local-only audit log (doc 13 §3, §7).
//!
//! Two questions the user must always be able to answer:
//!   - "When was it watching?"  -> [`EventType::CaptureToggle`] rows.
//!   - "What ever left this machine?" -> [`EventType::CloudSend`] rows.
//!
//! Audit rows are ordinary [`Event`]s written to the encrypted history DB, so
//! they are local-only. They **survive Purge All for 30 d**, then expire with
//! the rest (doc 13 §7, `db::retention::RetentionPolicy::audit_days`). Tampering
//! by a local admin is explicitly out of the threat model (doc 13 §1, §9).
//!
//! A `cloud_send` row records the SHA-256 of the **exact wire bytes**, the
//! transport, and the byte count (doc 13 §3) — the gateway computes the hash
//! over the same serialization it transmits ("preview == wire"), then hands the
//! record here. INVARIANT (2): this module only *records* egress; it never
//! performs it.

use aperture_contracts::context_payload::TransportTarget;
use aperture_contracts::event::{Event, EventType};

use crate::PrivacyError;

/// Why capture flipped (recorded in the `capture_toggle` payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleReason {
    /// User clicked the indicator / hotkey.
    UserAction,
    /// Capture released on shutdown / sleep.
    SystemSuspend,
    /// First-run default (OFF) or post-consent enable (doc 13 §8).
    Consent,
}

/// The structured `capture_toggle` audit record (doc 13 §3, §7). Serialized into
/// [`Event::payload`].
#[derive(Debug, Clone)]
pub struct CaptureToggleRecord {
    /// `true` = capture turned ON, `false` = OFF.
    pub enabled: bool,
    pub reason: ToggleReason,
    /// epoch milliseconds.
    pub ts: i64,
}

/// The structured `cloud_send` audit record (doc 13 §3). Built by the reasoning
/// gateway after a user-approved Send, over the exact transmitted bytes.
#[derive(Debug, Clone)]
pub struct CloudSendRecord {
    /// The payload that was sent (links the audit row to its context).
    pub payload_id: uuid::Uuid,
    /// SHA-256 of the wire bytes, lowercase hex (64 chars). See [`sha256_hex`].
    pub wire_sha256: String,
    /// Which transport carried the bytes (doc 09 §3).
    pub transport: TransportTarget,
    /// Number of bytes that left the machine.
    pub byte_count: u64,
    /// epoch milliseconds.
    pub ts: i64,
}

/// Writes audit rows into the encrypted DB. Holds (or borrows) a `db::Db` handle.
pub struct AuditLog {
    // TODO(M9): hold an `aperture_db::Db` handle (or a writer channel into the
    // single-writer Tier-0 pipeline, doc 03).
}

impl AuditLog {
    /// Record a capture on/off transition (doc 13 §3). Honors INVARIANT (3): the
    /// OFF transition that releases sidecars / drops VRAM is driven elsewhere;
    /// this just stamps the audit trail.
    pub fn record_capture_toggle(&self, _rec: CaptureToggleRecord) -> Result<(), PrivacyError> {
        // TODO(M9): build an Event { type: CaptureToggle, payload: rec } and
        // persist via the single writer; return Audit on failure.
        todo!("M9: write capture_toggle audit row (doc 13 §3)")
    }

    /// Record that bytes left the machine (doc 13 §3). Called by the gateway
    /// only, after Send, with the hash already computed over the wire bytes.
    pub fn record_cloud_send(&self, _rec: CloudSendRecord) -> Result<(), PrivacyError> {
        // TODO(M9): build an Event { type: CloudSend, payload: rec } and persist.
        todo!("M9: write cloud_send audit row (doc 13 §3)")
    }

    /// Read recent audit rows (both kinds) for the privacy/history UI, newest
    /// first. `limit` caps the returned rows.
    pub fn recent(&self, _limit: u32) -> Result<Vec<Event>, PrivacyError> {
        // TODO(M9): SELECT events WHERE type IN ('capture_toggle','cloud_send')
        // ORDER BY ts DESC LIMIT ?.
        todo!("M9: read recent audit rows (doc 13 §3)")
    }
}

/// Compute the lowercase-hex SHA-256 of the exact wire bytes (doc 13 §3). The
/// caller (gateway) passes the same serialization it transmits.
pub fn sha256_hex(_wire_bytes: &[u8]) -> String {
    // TODO(M9): sha2::Sha256 over wire_bytes; hex-encode lowercase (64 chars).
    todo!("M9: sha256 of wire bytes (doc 13 §3)")
}

/// Map an audit record to its [`EventType`] — the column the retention pruner
/// keys on for the 30-day audit survival window (doc 13 §7).
pub const CAPTURE_TOGGLE: EventType = EventType::CaptureToggle;
/// See [`CAPTURE_TOGGLE`].
pub const CLOUD_SEND: EventType = EventType::CloudSend;
