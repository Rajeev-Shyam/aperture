//! The consent gate (doc 13 §3).
//!
//! This module is the **only** place in the whole system that sets
//! [`ContextPayload::user_approved`]` = true` (contract law, doc 15 §2(b);
//! two-emitter rule, doc 13 §2). The gateway re-checks the flag before egress,
//! but the *authority* to grant it lives here and nowhere else.
//!
//! # Invariants
//! - **preview == wire** (doc 13 §3): exactly one serialized object is built,
//!   previewed, edited, and transmitted. Edits (remove an item, add a term,
//!   re-truncate) mutate *this* object; the bytes the user sees are the bytes
//!   that ship. The preview always shows every item (expandable, removable),
//!   every redaction (rule + count), the transport target, and the size/token
//!   estimate (doc 13 §3).
//! - **Send** is the only egress trigger; **Cancel leaves zero residue** — the
//!   payload is dropped, no socket opened, no CLI spawned, nothing persisted
//!   (doc 13 §3, doc 09 §6).
//! - On the MCP (pull) transport the *same* gate runs inside the
//!   `aperture_get_context` tool handler — Claude Desktop's call blocks on this
//!   decision (doc 09 §3, doc 13 §3). See [`crate::transports::mcp`].

// TODO(M7:) the preview panel <-> this gate handshake lands in M7 (UI in doc 11).

use aperture_contracts::ContextPayload;

/// The user's decision on a previewed payload (doc 13 §3).
#[derive(Debug, Clone)]
pub enum PreviewDecision {
    /// User pressed **Send** — approve exactly the (possibly edited) payload.
    Send,
    /// User pressed **Cancel** — drop everything; zero residue (doc 13 §3).
    Cancel,
}

/// An in-flight preview session over one [`ContextPayload`] (doc 13 §3).
///
/// Wraps the single object the preview renders and edits. Holding the payload
/// by value here is what guarantees Cancel leaves zero residue: drop the
/// `PreviewSession` and the only copy is gone.
pub struct PreviewSession {
    /// The single object — `user_approved` starts `false` and is flipped to
    /// `true` *only* by [`PreviewSession::approve`].
    payload: ContextPayload,
}

impl PreviewSession {
    /// Begin previewing `payload`. Asserts the incoming flag is `false`: nothing
    /// upstream of this gate is allowed to pre-approve (doc 13 §2).
    pub fn new(payload: ContextPayload) -> Self {
        debug_assert!(
            !payload.user_approved,
            "a payload reached the preview already approved — two-emitter rule violation (doc 13 §2)"
        );
        Self { payload }
    }

    /// Borrow the object the preview renders (every item, redaction, target,
    /// size estimate — doc 13 §3).
    pub fn payload(&self) -> &ContextPayload {
        &self.payload
    }

    /// Mutable access for in-preview edits (remove/edit an item, add a redaction
    /// term). After any edit the caller must re-run
    /// [`crate::payload_builder::truncate_oldest_first`] / re-render so the
    /// previewed bytes still equal the wire bytes (doc 13 §3, doc 09 §6).
    pub fn payload_mut(&mut self) -> &mut ContextPayload {
        // TODO(M7:) on every mutation, re-serialize + refresh the BuildReport (size/redaction lines).
        &mut self.payload
    }

    /// Apply the user's decision (doc 13 §3).
    ///
    /// - [`PreviewDecision::Send`] -> the **only** call site that sets
    ///   `user_approved = true`, then yields the approved object for
    ///   [`crate::Gateway::send_with_preview`].
    /// - [`PreviewDecision::Cancel`] -> consume `self` and return `None`; the
    ///   payload is dropped (zero residue).
    pub fn approve(mut self, decision: PreviewDecision) -> Option<ContextPayload> {
        match decision {
            PreviewDecision::Send => {
                // THE GATE: the sole place `user_approved` becomes true (doc 13 §2/§3).
                self.payload.user_approved = true;
                Some(self.payload)
            }
            // Cancel: `self` (and its only payload copy) drops here — zero residue.
            PreviewDecision::Cancel => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::{Intent, PayloadItem, TransportTarget};

    fn unapproved() -> ContextPayload {
        ContextPayload {
            payload_id: uuid::Uuid::nil(),
            created_ts: 0,
            intent: Intent::SummarizeCurrent,
            items: vec![PayloadItem::UserAddition { text: "hi".into() }],
            redactions: vec![],
            enrichment_offered: false,
            transport_target: TransportTarget::MessagesApi,
            user_approved: false,
        }
    }

    #[test]
    fn send_is_the_only_thing_that_approves() {
        let session = PreviewSession::new(unapproved());
        assert!(!session.payload().user_approved, "starts unapproved");
        let approved = session.approve(PreviewDecision::Send).expect("Send yields the payload");
        assert!(approved.user_approved, "Send is the sole gate that flips the flag (doc 13 §2/§3)");
    }

    #[test]
    fn cancel_yields_nothing_zero_residue() {
        let session = PreviewSession::new(unapproved());
        assert!(session.approve(PreviewDecision::Cancel).is_none(), "Cancel drops the only copy");
    }

    #[test]
    fn edits_apply_before_approval() {
        let mut session = PreviewSession::new(unapproved());
        session
            .payload_mut()
            .items
            .push(PayloadItem::UserAddition { text: "one more thing".into() });
        let approved = session.approve(PreviewDecision::Send).unwrap();
        assert_eq!(approved.items.len(), 2, "the edited object is exactly what ships (preview == wire)");
    }
}
