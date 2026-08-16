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

/// The structured `mcp_search` audit record (ADR-037): a cloud model ran a
/// query over local history. Written for EVERY search — hit, miss, or error —
/// so the trail answers "what has Claude asked about?" even when nothing was
/// ever staged or released.
#[derive(Debug, Clone)]
pub struct McpSearchRecord {
    /// The verbatim query string the model sent.
    pub query: String,
    /// How many rows the retrieval matched (never revealed to the model).
    pub hit_count: u64,
    /// The preview session the results were staged under, if any.
    pub payload_id: Option<uuid::Uuid>,
    /// epoch milliseconds.
    pub ts: i64,
}

impl ToggleReason {
    /// The stable wire string persisted in the audit payload.
    pub fn as_str(self) -> &'static str {
        match self {
            ToggleReason::UserAction => "user_action",
            ToggleReason::SystemSuspend => "system_suspend",
            ToggleReason::Consent => "consent",
        }
    }
}

/// The egress-recording seam the reasoning gateway holds (doc 09 §5, doc 13 §3).
///
/// A trait, not the concrete [`AuditLog`], for one reason: the gateway must be
/// unit-testable without standing up a database, and the gateway is the crate we
/// least want to complicate. INVARIANT (2) is unaffected — an audit sink only
/// *records* egress, it never performs it.
pub trait AuditSink: Send + Sync {
    /// Persist a `cloud_send` row. Called by the gateway AFTER a successful send.
    fn record_cloud_send(&self, rec: CloudSendRecord) -> Result<(), PrivacyError>;
}

/// The no-op sink: logs the record but persists nothing. Used by gateway tests
/// and by any composition that has no DB handle. Deliberately still *logs*, so a
/// misconfigured composition leaves a trace rather than silently losing the audit.
pub struct NullAuditSink;

impl AuditSink for NullAuditSink {
    fn record_cloud_send(&self, rec: CloudSendRecord) -> Result<(), PrivacyError> {
        tracing::warn!(
            payload_id = %rec.payload_id,
            sha256 = %rec.wire_sha256,
            bytes = rec.byte_count,
            "cloud_send NOT persisted — no audit sink configured (doc 13 §3)"
        );
        Ok(())
    }
}

/// Writes audit rows into the encrypted DB (doc 13 §3, §7).
///
/// Audit rows are ordinary [`Event`]s, so they inherit the encrypted-at-rest
/// storage and the retention pruner's audit-survival window for free — there is
/// no second, weaker store to keep in sync.
pub struct AuditLog {
    db: std::sync::Arc<aperture_db::Db>,
}

impl AuditLog {
    /// Build an audit log over the history DB handle.
    pub fn new(db: std::sync::Arc<aperture_db::Db>) -> Self {
        Self { db }
    }

    /// Record a capture on/off transition (doc 13 §3). Honors INVARIANT (3): the
    /// OFF transition that releases sidecars / drops VRAM is driven elsewhere;
    /// this just stamps the audit trail.
    pub fn record_capture_toggle(&self, rec: CaptureToggleRecord) -> Result<(), PrivacyError> {
        let ev = Event {
            id: 0,
            ts: rec.ts,
            r#type: EventType::CaptureToggle,
            app: None,
            process: None,
            window_title: None,
            // Schema shared with `aperture_capture::toggle::emit_toggle_event`,
            // which writes the *mechanism* row (capture actually started/stopped)
            // while this is the *decision* row. Same keys so the Activity &
            // Privacy view renders both; `source` distinguishes them. A decision
            // row with no matching mechanism row means capture was requested and
            // never actually ran — which the trail should show, not hide.
            payload: serde_json::json!({
                "enabled": rec.enabled,
                "reason": rec.reason.as_str(),
                "source": "consent",
            }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        };
        self.db
            .insert_event(&ev)
            .map(|_| ())
            .map_err(|e| PrivacyError::Audit(e.to_string()))
    }

    /// Record one MCP history search (ADR-037). Called by the bridge for every
    /// `aperture_search_history` call BEFORE the tool result is returned — an
    /// audit-write failure means the search must fail, not run unrecorded.
    pub fn record_mcp_search(&self, rec: McpSearchRecord) -> Result<(), PrivacyError> {
        let ev = Event {
            id: 0,
            ts: rec.ts,
            r#type: EventType::McpSearch,
            app: None,
            process: None,
            window_title: None,
            payload: serde_json::json!({
                "query": rec.query,
                "hit_count": rec.hit_count,
                "payload_id": rec.payload_id.map(|id| id.to_string()),
            }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        };
        self.db
            .insert_event(&ev)
            .map(|_| ())
            .map_err(|e| PrivacyError::Audit(e.to_string()))
    }

    /// Read recent audit rows (both kinds) for the Activity & Privacy view,
    /// newest first. `limit` caps the returned rows.
    pub fn recent(&self, limit: u32) -> Result<Vec<Event>, PrivacyError> {
        self.db
            .recent_audit_events(limit)
            .map_err(|e| PrivacyError::Audit(e.to_string()))
    }
}

impl AuditSink for AuditLog {
    /// Record that bytes left the machine (doc 13 §3). Called by the gateway
    /// only, after Send, with the hash already computed over the wire bytes.
    fn record_cloud_send(&self, rec: CloudSendRecord) -> Result<(), PrivacyError> {
        let ev = Event {
            id: 0,
            ts: rec.ts,
            r#type: EventType::CloudSend,
            app: None,
            process: None,
            window_title: None,
            payload: serde_json::json!({
                "payload_id": rec.payload_id.to_string(),
                "wire_sha256": rec.wire_sha256,
                "transport": rec.transport,
                "byte_count": rec.byte_count,
            }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        };
        self.db
            .insert_event(&ev)
            .map(|_| ())
            .map_err(|e| PrivacyError::Audit(e.to_string()))
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
    use std::sync::Arc;

    fn log() -> AuditLog {
        AuditLog::new(Arc::new(aperture_db::Db::open_in_memory().expect("db")))
    }

    #[test]
    fn capture_toggle_rows_persist_and_read_back_newest_first() {
        let log = log();
        log.record_capture_toggle(CaptureToggleRecord {
            enabled: true,
            reason: ToggleReason::Consent,
            ts: 1_000,
        })
        .expect("on");
        log.record_capture_toggle(CaptureToggleRecord {
            enabled: false,
            reason: ToggleReason::UserAction,
            ts: 2_000,
        })
        .expect("off");

        let rows = log.recent(10).expect("recent");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ts, 2_000, "newest first");
        assert_eq!(rows[0].payload["enabled"], serde_json::json!(false));
        assert_eq!(rows[0].payload["reason"], serde_json::json!("user_action"));
        assert_eq!(rows[1].payload["reason"], serde_json::json!("consent"));
    }

    #[test]
    fn cloud_send_row_records_the_hash_transport_and_byte_count() {
        let log = log();
        let id = uuid::Uuid::new_v4();
        log.record_cloud_send(CloudSendRecord {
            payload_id: id,
            wire_sha256: sha256_hex(b"abc"),
            transport: TransportTarget::ClaudeCli,
            byte_count: 3,
            ts: 5_000,
        })
        .expect("send row");

        let rows = log.recent(10).expect("recent");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].r#type, EventType::CloudSend);
        assert_eq!(rows[0].payload["payload_id"], serde_json::json!(id.to_string()));
        assert_eq!(rows[0].payload["byte_count"], serde_json::json!(3));
        // kebab-case, matching TransportTarget's serde repr.
        assert_eq!(rows[0].payload["transport"], serde_json::json!("claude-cli"));
        assert_eq!(
            rows[0].payload["wire_sha256"],
            serde_json::json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn recent_returns_only_audit_rows_and_honors_the_limit() {
        let log = log();
        // A non-audit event must never surface in the privacy view.
        log.db
            .insert_event(&Event {
                id: 0,
                ts: 9_000,
                r#type: EventType::WindowFocus,
                app: None,
                process: None,
                window_title: None,
                payload: serde_json::json!({}),
                connector_id: None,
                session_id: None,
                redaction_flags: 0,
            })
            .unwrap();
        for ts in 0..5 {
            log.record_capture_toggle(CaptureToggleRecord {
                enabled: true,
                reason: ToggleReason::UserAction,
                ts,
            })
            .unwrap();
        }

        let rows = log.recent(3).expect("recent");
        assert_eq!(rows.len(), 3, "limit honored");
        assert!(
            rows.iter().all(|r| matches!(r.r#type, EventType::CaptureToggle | EventType::CloudSend)),
            "only audit rows appear in the Activity & Privacy view"
        );
    }

    #[test]
    fn the_null_sink_never_errors_but_persists_nothing() {
        // Its only job is to keep a DB-less composition from breaking Send.
        NullAuditSink
            .record_cloud_send(CloudSendRecord {
                payload_id: uuid::Uuid::new_v4(),
                wire_sha256: sha256_hex(b""),
                transport: TransportTarget::MessagesApi,
                byte_count: 0,
                ts: 1,
            })
            .expect("null sink is infallible");
    }

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
