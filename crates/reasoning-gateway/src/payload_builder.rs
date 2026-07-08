//! Context-payload assembly (doc 09 §5, doc 13 §5).
//!
//! Builds the **one** object that is built, previewed, and sent (contract law,
//! doc 15 §2). The pipeline, in order:
//!
//! 1. Gather typed [`PayloadItem`]s for the intent (OCR text is the default
//!    context currency; a screenshot is opt-in enrichment, doc 09 §5).
//! 2. **Run `aperture_privacy::redaction` over every text item BEFORE preview**
//!    (doc 13 §5) — ordered deterministic rules (secrets, cards, IBAN, email,
//!    phone, user terms); every hit increments [`Redaction`] shown in the preview.
//! 3. Cap the `event_trail` at [`EVENT_TRAIL_MAX`] (= 50, doc 03 §4); on oversize,
//!    truncation **drops the oldest events first** (doc 09 §6).
//! 4. Warn in the preview when the serialized size exceeds
//!    [`PAYLOAD_SIZE_WARN_BYTES`] (> 50 KB, doc 09 §5).
//!
//! On Send, [`record_cloud_send`] writes the `cloud_send` audit row with the
//! SHA-256 of the **wire bytes** (doc 13 §3) — the previewed bytes are the wire
//! bytes (preview == wire).

// TODO(M7:) assembly + redaction-before-preview + truncation land in M7.

use aperture_contracts::{
    ContextPayload, Intent, PayloadItem, TransportTarget, EVENT_TRAIL_MAX, PAYLOAD_SIZE_WARN_BYTES,
};

/// Errors raised while assembling a payload (doc 09 §5/§6).
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Even after truncation the payload exceeds the chosen transport's hard
    /// limit (e.g. the CLI stdin caveat, doc 09 §3/§6).
    #[error("payload still oversized after truncation: {0}")]
    Oversized(String),
    /// The redaction stage (`aperture_privacy::redaction`) failed (doc 13 §5).
    #[error("redaction failed: {0}")]
    Redaction(String),
    /// Serialization to wire bytes failed.
    #[error("serialize failed: {0}")]
    Serialize(String),
}

/// Outcome of an assembly pass, surfaced to the preview panel (doc 09 §5).
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// Serialized size of the payload in bytes (the size/token estimate line).
    pub serialized_bytes: usize,
    /// `true` when `serialized_bytes > PAYLOAD_SIZE_WARN_BYTES` — the preview
    /// shows the > 50 KB warning (doc 09 §5).
    pub oversize_warning: bool,
    /// How many `event_trail` events were dropped (oldest-first) to fit (doc 09 §6).
    pub events_truncated: usize,
}

/// Assemble the single previewable/sendable [`ContextPayload`] for `intent`
/// from gathered `items`, targeted at `transport_target` (doc 09 §5).
///
/// Ordering is load-bearing: **redaction runs before the payload is handed to
/// the preview** (doc 13 §5), the `event_trail` is capped at
/// [`EVENT_TRAIL_MAX`], and oversize truncation drops the oldest events first
/// (doc 09 §6). The returned payload has `user_approved == false` — only
/// [`crate::preview`] may flip it (doc 13 §2/§3).
pub fn build(
    _intent: Intent,
    _items: Vec<PayloadItem>,
    _transport_target: TransportTarget,
) -> Result<(ContextPayload, BuildReport), BuildError> {
    // TODO(M7:)
    //   1. assemble PayloadItem vec; OCR text default, screenshot only if enrichment opted-in
    //      (pre-downscaled <=1568 px / ~1.15 MP upstream, doc 09 §5).
    //   2. for each text-bearing item: aperture_privacy::redaction::redact(text) -> (text, hits);
    //      push every rule+count into payload.redactions (doc 13 §5).
    //   3. enforce EVENT_TRAIL_MAX on any EventTrail item; if serialized > size limit,
    //      drop oldest event_trail entries first until it fits (doc 09 §6).
    //   4. warn at > PAYLOAD_SIZE_WARN_BYTES (doc 09 §5).
    //   5. construct ContextPayload { user_approved: false, .. } — preview owns the flag.
    let _ = (EVENT_TRAIL_MAX, PAYLOAD_SIZE_WARN_BYTES);
    todo!("M7: gather -> redact (before preview) -> cap event_trail -> truncate oldest-first -> size-warn")
}

/// Drop the oldest `event_trail` events first until the payload serializes under
/// `max_bytes` (doc 09 §6). Returns the number of events dropped. Used by
/// [`build`] and re-run after a preview edit so the wire bytes always equal the
/// re-rendered previewed bytes.
pub fn truncate_oldest_first(
    _payload: &mut ContextPayload,
    _max_bytes: usize,
) -> Result<usize, BuildError> {
    // TODO(M7:) locate the EventTrail item, pop from the front (oldest) until under max_bytes.
    todo!("M7: drop oldest event_trail items first until under the byte budget")
}

/// Record the `cloud_send` audit row at Send: SHA-256 of the **wire bytes**,
/// the transport target, and the byte count (doc 13 §3, doc 09 §5).
///
/// Computes `sha256(wire_bytes)` locally (this crate owns `sha2`) and forwards
/// to `aperture_privacy::audit_log`. Audit rows survive Purge All for 30 d
/// (doc 13 §7).
pub fn record_cloud_send(
    _wire_bytes: &[u8],
    _transport: TransportTarget,
) -> Result<(), BuildError> {
    // TODO(M9:) sha2::Sha256 over wire_bytes; aperture_privacy::audit_log::record_cloud_send(
    //           AuditCloudSend { sha256, transport, byte_count }). // [VERIFY] privacy API surface.
    todo!("M9: sha256(wire_bytes) + transport + byte count -> aperture_privacy::audit_log")
}
