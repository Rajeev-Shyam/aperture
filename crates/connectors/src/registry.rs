//! Connector registry (doc 10 §1): self-registration + lookup.
//!
//! Connectors self-register at startup; the pattern engine and Bubble UI hold a
//! `&ConnectorRegistry` and resolve by `connector_id` (Path B step 1, doc 02 §5)
//! or fan an event across all connectors to find one whose `can_capture` matches
//! (Path A step 4). Because everything is keyed off the [`Connector`] trait,
//! adding a v2 connector touches only [`crate::default_registry`].

use std::collections::HashMap;

use aperture_contracts::event::Event;
use aperture_contracts::{Connector, ConnectorState};

/// Holds the registered connectors, indexed by `id()` for O(1) Path-B lookup.
///
/// `id()` doubles as the `connector_type` written onto [`ConnectorState`]
/// (`"browser" | "youtube" | "document" | "ide"`), so the two lookups the doc
/// names — *by id* and *by connector_type* — are the same map.
#[derive(Default)]
pub struct ConnectorRegistry {
    by_id: HashMap<&'static str, Box<dyn Connector>>,
    /// Registration order, so capture fan-out is deterministic (YouTube before
    /// the generic browser connector — doc 10 §3 is the more specific match).
    order: Vec<&'static str>,
}

impl ConnectorRegistry {
    /// An empty registry. Prefer [`crate::default_registry`] for the v1 set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Self-registration entry point (doc 10 §1). Later registration of the same
    /// `id` replaces the earlier one.
    pub fn register(&mut self, connector: Box<dyn Connector>) {
        let id = connector.id();
        if self.by_id.insert(id, connector).is_none() {
            self.order.push(id);
        } else {
            tracing::debug!(connector_id = id, "connector re-registered (replaced)");
        }
        tracing::debug!(connector_id = id, "connector registered");
    }

    /// Lookup by `connector_id` — Path B step 1 (doc 02 §5). The Bubble UI
    /// resolves an `action_ref` to an id, then asks for the connector here.
    pub fn by_id(&self, id: &str) -> Option<&dyn Connector> {
        self.by_id.get(id).map(|c| c.as_ref())
    }

    /// Lookup by `connector_type`. Identical key space to [`Self::by_id`] (the
    /// type *is* the id); named separately to match the doc-10 §1 vocabulary and
    /// to keep call sites self-documenting.
    pub fn by_type(&self, connector_type: &str) -> Option<&dyn Connector> {
        self.by_id(connector_type)
    }

    /// All registered ids, in registration order.
    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.order.iter().copied()
    }

    /// Path A step 4 (doc 02 §5): find the first connector that claims this event
    /// and snapshot its resumable handle. Order matters — the most specific
    /// connector (YouTube) is registered ahead of the generic browser one.
    /// The Tier-0 pipeline (src-tauri `spawn_connector_task`) persists the
    /// returned state as a `connector_state` row (~10 ms budget, doc 02 §4).
    pub fn capture(&self, ev: &Event) -> Option<ConnectorState> {
        for id in &self.order {
            let connector = self.by_id.get(id)?;
            if connector.can_capture(ev) {
                if let Some(state) = connector.capture(ev) {
                    return Some(state);
                }
            }
        }
        None
    }
}
