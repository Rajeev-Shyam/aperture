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

use aperture_contracts::{
    ContextPayload, Intent, PayloadItem, TransportTarget, EVENT_TRAIL_MAX, PAYLOAD_SIZE_WARN_BYTES,
};
use aperture_privacy::audit_log::{sha256_hex, CloudSendRecord};
use aperture_privacy::redaction::Redactor;
use uuid::Uuid;

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
    intent: Intent,
    items: Vec<PayloadItem>,
    transport_target: TransportTarget,
    redactor: &Redactor,
    created_ts: i64,
) -> Result<(ContextPayload, BuildReport), BuildError> {
    // 1. Assemble. A screenshot present ⇒ enrichment was opted in (doc 09 §5).
    let enrichment_offered = items
        .iter()
        .any(|i| matches!(i, PayloadItem::Screenshot { .. }));
    let mut payload = ContextPayload {
        payload_id: Uuid::new_v4(),
        created_ts,
        intent,
        items,
        redactions: Vec::new(),
        enrichment_offered,
        transport_target,
        // Contract 2(b): only the preview panel may flip this. Assembly leaves it false.
        user_approved: false,
    };

    // 2. Redact BEFORE preview (doc 13 §5) — mutates text items + fills `redactions`.
    redactor.redact_payload(&mut payload);

    // 3. Cap the event_trail at EVENT_TRAIL_MAX, dropping the OLDEST first (doc 03 §4).
    let events_truncated = cap_event_trail(&mut payload);

    // 4. Size + the > 50 KB preview warning (doc 09 §5).
    let serialized_bytes = serialized_len(&payload)?;
    Ok((
        payload,
        BuildReport {
            serialized_bytes,
            oversize_warning: serialized_bytes > PAYLOAD_SIZE_WARN_BYTES,
            events_truncated,
        },
    ))
}

/// Serialized (wire) byte length of the payload. `user_approved` is
/// `#[serde(skip_serializing)]`, so this is exactly the wire form ("preview ==
/// wire", doc 13 §3) — the same bytes [`record_cloud_send`] hashes.
fn serialized_len(payload: &ContextPayload) -> Result<usize, BuildError> {
    serde_json::to_vec(payload)
        .map(|v| v.len())
        .map_err(|e| BuildError::Serialize(e.to_string()))
}

/// Cap the first `EventTrail` item to the newest [`EVENT_TRAIL_MAX`] events,
/// dropping the oldest (front) first. Returns how many were dropped.
fn cap_event_trail(payload: &mut ContextPayload) -> usize {
    for item in &mut payload.items {
        if let PayloadItem::EventTrail { events } = item {
            if events.len() > EVENT_TRAIL_MAX {
                let drop = events.len() - EVENT_TRAIL_MAX;
                events.drain(0..drop); // oldest-first
                return drop;
            }
        }
    }
    0
}

/// Drop the oldest `event_trail` events first until the payload serializes under
/// `max_bytes` (doc 09 §6). Returns the number of events dropped. Re-run after a
/// preview edit / by a transport with a hard limit so the wire bytes always equal
/// the re-rendered previewed bytes. Errors [`BuildError::Oversized`] if nothing
/// left to drop and it still doesn't fit.
pub fn truncate_oldest_first(
    payload: &mut ContextPayload,
    max_bytes: usize,
) -> Result<usize, BuildError> {
    let mut dropped = 0;
    loop {
        if serialized_len(payload)? <= max_bytes {
            return Ok(dropped);
        }
        // Find the event_trail; pop the oldest (front). If there's nothing left to
        // shed, the payload is irreducibly too big for this transport (doc 09 §6).
        let popped = payload.items.iter_mut().find_map(|item| match item {
            PayloadItem::EventTrail { events } if !events.is_empty() => {
                events.remove(0);
                Some(())
            }
            _ => None,
        });
        match popped {
            Some(()) => dropped += 1,
            None => {
                return Err(BuildError::Oversized(format!(
                    "{} B still exceeds the {max_bytes} B limit with no event_trail left to trim",
                    serialized_len(payload)?
                )))
            }
        }
    }
}

/// Build the `cloud_send` audit record at Send: SHA-256 of the **wire bytes**, the
/// transport target, the payload id, and the byte count (doc 13 §3, doc 09 §5).
///
/// `wire_bytes` MUST be the exact bytes the transport transmitted (preview ==
/// wire) so the recorded hash is verifiable; `transport` is the transport that
/// **actually** egressed (after any push fall-through), not the payload's intended
/// target. Persisting the row to the encrypted DB via
/// `aperture_privacy::audit_log::AuditLog` is the M9 step; the hash + record are
/// computed here now (the SC5 gate checks them).
pub fn record_cloud_send(
    payload: &ContextPayload,
    wire_bytes: &[u8],
    transport: TransportTarget,
) -> CloudSendRecord {
    CloudSendRecord {
        payload_id: payload.payload_id,
        wire_sha256: sha256_hex(wire_bytes),
        transport,
        byte_count: wire_bytes.len() as u64,
        ts: payload.created_ts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(&[]).expect("built-in rules compile")
    }

    #[test]
    fn build_redacts_before_preview_and_reports_size() {
        let items = vec![PayloadItem::OcrText {
            source_event_id: 1,
            text: "email me at leak@corp.com".into(),
            redacted: false,
        }];
        let (payload, report) = build(
            Intent::SummarizeCurrent,
            items,
            TransportTarget::MessagesApi,
            &redactor(),
            42,
        )
        .unwrap();
        assert!(!payload.user_approved, "assembly never pre-approves (contract 2b)");
        assert!(!payload.redactions.is_empty(), "the redaction ran before preview");
        // The wire bytes carry the placeholder, never the raw address.
        let wire = String::from_utf8(serde_json::to_vec(&payload).unwrap()).unwrap();
        assert!(!wire.contains("leak@corp.com") && wire.contains("email#1"), "wire: {wire}");
        assert!(!report.oversize_warning, "a tiny payload is under 50 KB");
    }

    #[test]
    fn event_trail_is_capped_to_fifty_oldest_first() {
        let events: Vec<serde_json::Value> = (0..60).map(|i| serde_json::json!({ "i": i })).collect();
        let (payload, report) = build(
            Intent::ExplainPattern,
            vec![PayloadItem::EventTrail { events }],
            TransportTarget::ClaudeCli,
            &redactor(),
            0,
        )
        .unwrap();
        assert_eq!(report.events_truncated, 10, "60 - 50 dropped");
        match &payload.items[0] {
            PayloadItem::EventTrail { events } => {
                assert_eq!(events.len(), EVENT_TRAIL_MAX);
                // Oldest (i=0..9) dropped; the newest survive (front is now i=10).
                assert_eq!(events[0], serde_json::json!({ "i": 10 }));
            }
            _ => panic!("item 0 is the event trail"),
        }
    }

    #[test]
    fn truncate_oldest_first_sheds_until_it_fits() {
        let events: Vec<serde_json::Value> =
            (0..40).map(|i| serde_json::json!({ "i": i, "pad": "x".repeat(50) })).collect();
        let (mut payload, _) = build(
            Intent::Custom,
            vec![PayloadItem::EventTrail { events }],
            TransportTarget::ClaudeCli,
            &redactor(),
            0,
        )
        .unwrap();
        let before = serde_json::to_vec(&payload).unwrap().len();
        let dropped = truncate_oldest_first(&mut payload, before / 2).unwrap();
        assert!(dropped > 0, "some oldest events were shed");
        assert!(serde_json::to_vec(&payload).unwrap().len() <= before / 2, "now fits the budget");
    }

    #[test]
    fn record_cloud_send_hashes_the_exact_wire_bytes() {
        let (payload, _) = build(
            Intent::AnswerQuery,
            vec![PayloadItem::UserAddition { text: "hi".into() }],
            TransportTarget::ClaudeDesktopMcp,
            &redactor(),
            7,
        )
        .unwrap();
        let wire = serde_json::to_vec(&payload).unwrap();
        let rec = record_cloud_send(&payload, &wire, TransportTarget::ClaudeCli);
        assert_eq!(rec.byte_count as usize, wire.len());
        assert_eq!(rec.payload_id, payload.payload_id);
        assert_eq!(rec.transport, TransportTarget::ClaudeCli, "records the actual egress transport");
        assert_eq!(rec.wire_sha256, sha256_hex(&wire), "hash is over the transmitted bytes");
        assert_eq!(rec.wire_sha256.len(), 64);
    }
}
