//! Event normalizer (doc 05 §6).
//!
//! The normalizer is the join point of the pipeline (doc 05 §6):
//! `hook thread → debouncer → sampler → (frame → OCR) + (event → normalizer → bus)`.
//!
//! For every raw [`crate::hooks::HookEvent`] it:
//! 1. attaches `app` / `process` / `window_title` (resolved via
//!    [`crate::hooks::window_identity`]);
//! 2. assigns the `session_id` from the sessionizer (doc 08 §3);
//! 3. runs the **exclusion check** and sets `redaction_flags |= EXCLUDED` on a hit
//!    (doc 05 §4, doc 13 §4) — excluded events are metadata-only and can never
//!    enter a payload;
//! 4. forwards `can_capture` events to the **connector registry** (doc 10) so
//!    browser/youtube/document/ide connectors may snapshot resumable state;
//! 5. publishes the finished [`Event`] on the bus (doc 15 §1).
//!
//! It never opens a socket and never spawns a process — invariant (2) (doc 13 §2).

// TODO(M1): the normalizer lands in the M1 capture milestone.

use aperture_contracts::{Event, EventType};
use aperture_contracts::event::redaction_flags;

use crate::exclusion::ExclusionList;
use crate::hooks::{HookEvent, WindowIdentity};

/// Turns raw hook/sample signals into normalized bus [`Event`]s and forwards
/// capturable events to the connector registry (doc 05 §6). Holds the bus sender,
/// the exclusion list, and a handle to the sessionizer.
pub struct Normalizer {
    // bus: aperture_event_bus::Sender<Event>,
    // exclusion: ExclusionList,
    // sessionizer: aperture_pattern_engine::SessionizerHandle,  // wired M3 (doc 08 §3).
    // connectors: aperture_connectors::Registry,                // wired M4 (doc 10).
}

impl Normalizer {
    /// Construct the normalizer with its downstream sinks.
    pub fn new(
        // bus: aperture_event_bus::Sender<Event>,
        _exclusion: ExclusionList,
    ) -> Self {
        // TODO(M1): store bus sender + exclusion; M3 wire sessionizer; M4 wire registry.
        todo!("M1: construct the normalizer")
    }

    /// Normalize a raw hook event into a bus [`Event`] and publish it (doc 05 §6).
    ///
    /// Maps the hook kind to an [`EventType`], attaches identity, assigns
    /// `session_id`, applies the exclusion flag, then publishes and (if not
    /// excluded) forwards to the connector registry.
    pub async fn normalize_hook(&self, _raw: HookEvent, _identity: WindowIdentity) {
        // TODO(M1):
        //   1. map: ForegroundChanged→WindowFocus, WindowOpened→WindowOpen,
        //      WindowClosed→WindowClose, TitleChanged→(title refresh; browser→Navigation
        //      via uia::read_address_bar, doc 05 §3).
        //   2. build_event(...); assign_session_id; apply_exclusion.
        //   3. publish to the bus; if !EXCLUDED, forward_capturable.
        todo!("M1: normalize a hook event and publish")
    }

    /// Build a normalized [`Event`] with identity attached. `id` is `0` on the bus
    /// (assigned by the DB on insert — doc 15 §1); `session_id` is filled by
    /// [`Self::assign_session_id`]; `redaction_flags` start at 0.
    pub fn build_event(
        &self,
        _ty: EventType,
        _identity: &WindowIdentity,
        _payload: serde_json::Value,
    ) -> Event {
        // TODO(M1): construct Event { id:0, ts: now_ms(), type, app, process,
        //   window_title, payload, connector_id:None, session_id:None,
        //   redaction_flags:0 } from the identity (doc 15 §1).
        todo!("M1: assemble a normalized Event from identity + payload")
    }

    /// Assign the current `session_id` from the sessionizer (doc 08 §3). The
    /// sessionizer owns boundary logic; the normalizer only stamps the result.
    pub fn assign_session_id(&self, _ev: &mut Event) {
        // TODO(M3): query aperture-pattern-engine sessionizer for the active session.
        todo!("M3: stamp session_id from the sessionizer")
    }

    /// Run the exclusion check and set `redaction_flags |= EXCLUDED` on a hit
    /// (doc 05 §4, doc 13 §4). This is the normalizer-side enforcement; the
    /// sampler also gates frame capture earlier (doc 05 §4). Both are required: a
    /// metadata-only event for an excluded context must still carry the flag.
    pub fn apply_exclusion(&self, _ev: &mut Event) {
        // TODO(M1): if exclusion.is_excluded(process, class, title):
        //   ev.redaction_flags |= redaction_flags::EXCLUDED;
        //   (private/incognito → also redaction_flags::PRIVATE_WINDOW, doc 13 §4).
        let _ = redaction_flags::EXCLUDED;
        todo!("M1: set EXCLUDED on excluded contexts")
    }

    /// Forward a `can_capture` event to the connector registry so the matching
    /// connector may snapshot resumable state (doc 05 §6, doc 10). **Never** called
    /// for `EXCLUDED` events — excluded contexts run no connector capture (doc 05 §4).
    pub fn forward_capturable(&self, _ev: &Event) {
        // TODO(M4): for each connector where connector.can_capture(ev) is true,
        //   call connector.capture(ev) and persist the ConnectorState (doc 10 §1).
        todo!("M4: forward can_capture events to the connector registry")
    }
}
