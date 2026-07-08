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
/// caller (gateway) passes the same serialization it transmits — this is the hash
/// the SC5 gate checks equals `sha256(previewed bytes)` ("preview == wire").
///
/// Implemented at M7 (the gateway/SC5 needs it); the audit-row DB *persistence*
/// ([`AuditLog::record_cloud_send`]) remains the M9 privacy-milestone piece.
pub fn sha256_hex(wire_bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(wire_bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_the_known_empty_and_abc_vectors() {
        // NIST/standard test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(b"abc").len(), 64, "lowercase hex is 64 chars");
    }
}

/// Map an audit record to its [`EventType`] — the column the retention pruner
/// keys on for the 30-day audit survival window (doc 13 §7).
pub const CAPTURE_TOGGLE: EventType = EventType::CaptureToggle;
/// See [`CAPTURE_TOGGLE`].
pub const CLOUD_SEND: EventType = EventType::CloudSend;
