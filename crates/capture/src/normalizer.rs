//! Event normalizer (doc 05 §6).
//!
//! The normalizer is the join point of the pipeline (doc 05 §6):
//! `hook thread → debouncer → sampler → (frame → OCR) + (event → normalizer → bus)`.
//!
//! For every raw [`crate::hooks::HookEvent`] it:
//! 1. attaches `app` / `process` / `window_title` (resolved via
//!    [`crate::hooks::window_identity`]);
//! 2. runs the **exclusion check** and sets `redaction_flags |= EXCLUDED` on a hit
//!    (doc 05 §4, doc 13 §4) — excluded events are metadata-only and can never
//!    enter a payload;
//! 3. for a **browser** focus/title change, attempts the UIA address-bar read
//!    (`navigation { url }`, RK4 fallback semantics — the extension becomes the
//!    primary source at M4, ADR-027); URLs run through the same exclusion gate
//!    incl. `url_pattern` (FIX 2.2);
//! 4. publishes the finished [`Event`] on the bus (doc 15 §1);
//! 5. `session_id` is stamped downstream by the pattern engine's sessionizer as
//!    it consumes the bus (doc 08 §3) — the normalizer leaves it `None`.
//!
//! It never opens a socket and never spawns a process — invariant (2) (doc 13 §2).

use std::sync::Arc;

use aperture_contracts::{Event, EventType};
use aperture_event_bus::EventBus;

use crate::exclusion::{ExclusionList, ExclusionVerdict};
use crate::hooks::{HookEvent, WindowIdentity};
use crate::uia::{self, AddressBarHints, AddressBarRead};

/// The durable-store seam (doc 15 §1: "SQLite is the durable form ... the
/// durable form is always written before the at-most-once notify"). The shell
/// implements this over `aperture_db::Db`; tests use an in-memory recorder.
/// Returning `None` (store unavailable) degrades to notify-only — capture keeps
/// working, durability resumes when the store does (doc 05 §7 resilience).
pub trait EventStore: Send + Sync {
    /// Persist one event; returns the DB-assigned id.
    fn persist(&self, ev: &Event) -> Option<i64>;
}

/// Turns raw hook/sample signals into normalized bus [`Event`]s (doc 05 §6).
/// Holds the store seam, the bus sender, the exclusion list, and the browser
/// address-bar hints.
pub struct Normalizer {
    bus: EventBus,
    exclusion: ExclusionList,
    hints: AddressBarHints,
    /// The durable form (doc 15 §1). `None` in bring-up harnesses.
    store: Option<Arc<dyn EventStore>>,
    /// Last successfully read URL per browser hwnd (RK4: fall back to last-known).
    last_urls: std::sync::Mutex<std::collections::HashMap<isize, String>>,
}

/// What the normalizer decided for one hook event — the pipeline uses this to
/// decide whether to also request a frame sample (doc 05 §4/§6).
#[derive(Debug, Clone)]
pub struct Normalized {
    pub event: Event,
    /// The resolved identity (reused by the sampler's exclusion gate).
    pub identity: WindowIdentity,
    /// `false` for excluded contexts: metadata-only, no frame, no OCR, no
    /// connector capture (doc 05 §4).
    pub capture_frame: bool,
}

impl Normalizer {
    /// Construct the normalizer with its downstream sinks.
    pub fn new(bus: EventBus, exclusion: ExclusionList) -> Self {
        Self {
            bus,
            exclusion,
            hints: AddressBarHints::default(),
            store: None,
            last_urls: Default::default(),
        }
    }

    /// Attach the durable store (doc 15 §1). The shell wires `aperture_db::Db`
    /// here at composition time; events then persist **before** they notify.
    pub fn with_store(mut self, store: Arc<dyn EventStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Persist-then-publish (doc 15 §1 ordering). Stamps the DB id onto the
    /// published event so downstream consumers (frame sink, pattern engine)
    /// reference the durable row.
    fn commit(&self, mut ev: Event) -> Event {
        if let Some(store) = &self.store {
            if let Some(id) = store.persist(&ev) {
                ev.id = id;
            }
        }
        let _ = self.bus.publish(ev.clone()); // no-subscriber send is fine (doc 15 §1)
        ev
    }

    /// Normalize a raw hook event into bus [`Event`]s and publish them
    /// (doc 05 §6). Returns what was published (primary event first) so the
    /// pipeline can drive the sampler off the same verdicts.
    ///
    /// Maps: `ForegroundChanged→WindowFocus`, `WindowOpened→WindowOpen`,
    /// `WindowClosed→WindowClose`, `TitleChanged→(title refresh; browser →
    /// an additional Navigation event via the UIA read, doc 05 §3)`.
    pub fn normalize_hook(
        &self,
        raw: &HookEvent,
        identity: WindowIdentity,
        now_ms: i64,
    ) -> Vec<Normalized> {
        let (ty, hwnd) = match raw {
            HookEvent::ForegroundChanged { hwnd } => (EventType::WindowFocus, *hwnd),
            HookEvent::WindowOpened { hwnd } => (EventType::WindowOpen, *hwnd),
            HookEvent::WindowClosed { hwnd } => (EventType::WindowClose, *hwnd),
            // A title refresh rides a focus-shaped event (same context, new title).
            HookEvent::TitleChanged { hwnd } => (EventType::WindowFocus, *hwnd),
        };

        let mut out = Vec::new();
        let mut primary = self.build_event(ty, &identity, serde_json::json!({}), now_ms);
        let verdict = self.apply_exclusion(&mut primary, None);
        let capture_frame =
            !verdict.is_excluded() && !matches!(raw, HookEvent::WindowClosed { .. });
        let primary = self.commit(primary); // persist THEN notify (doc 15 §1)
        out.push(Normalized { event: primary, identity: identity.clone(), capture_frame });

        // Browser navigation read on focus/title change (doc 05 §3; UIA is the
        // no-extension fallback, ADR-027). Never for excluded contexts.
        if !verdict.is_excluded()
            && matches!(
                raw,
                HookEvent::TitleChanged { .. } | HookEvent::ForegroundChanged { .. }
            )
        {
            if let Some(process) = identity.process.as_deref() {
                if uia::is_browser_process(process, &self.hints) {
                    if let Some(mut nav) = self.navigation_event(hwnd, &identity, now_ms) {
                        nav.event = self.commit(nav.event); // persist THEN notify
                        out.push(nav);
                    }
                }
            }
        }
        out
    }

    /// Attempt the UIA address-bar read and build a `navigation` event
    /// (doc 05 §3). RK4 semantics: `Unavailable` falls back to the last-known
    /// URL for this hwnd; no known URL ⇒ **no event** (never fabricate).
    fn navigation_event(
        &self,
        hwnd: isize,
        identity: &WindowIdentity,
        now_ms: i64,
    ) -> Option<Normalized> {
        let url = match uia::read_address_bar(hwnd, &self.hints) {
            AddressBarRead::Url(u) => {
                self.last_urls
                    .lock()
                    .expect("url cache lock")
                    .insert(hwnd, u.clone());
                u
            }
            AddressBarRead::Empty => return None, // new tab page: nothing to record
            AddressBarRead::Unavailable => {
                // RK4: last-known or skip. A wrong URL is worse than no URL.
                self.last_urls.lock().expect("url cache lock").get(&hwnd)?.clone()
            }
        };

        let browser = identity
            .process
            .as_deref()
            .map(|p| p.trim_end_matches(".exe").to_ascii_lowercase())
            .unwrap_or_default();
        let mut ev = self.build_event(
            EventType::Navigation,
            identity,
            serde_json::json!({ "url": url, "browser": browser }),
            now_ms,
        );
        // FIX 2.2: URLs traverse the same exclusion gate incl. `url_pattern`.
        let verdict = self.apply_exclusion(&mut ev, Some(&url));
        if verdict.is_excluded() {
            // Metadata-only: strip the URL itself (an excluded URL must never
            // persist in a payload-reachable field, doc 13 §4).
            ev.payload = serde_json::json!({ "browser": browser, "excluded": true });
        }
        Some(Normalized {
            capture_frame: !verdict.is_excluded(),
            identity: identity.clone(),
            event: ev,
        })
    }

    /// Build a normalized [`Event`] with identity attached. `id` is `0` on the
    /// bus (assigned by the DB on insert — doc 15 §1); `session_id` is stamped
    /// downstream (doc 08 §3); `redaction_flags` start at 0.
    pub fn build_event(
        &self,
        ty: EventType,
        identity: &WindowIdentity,
        payload: serde_json::Value,
        now_ms: i64,
    ) -> Event {
        Event {
            id: 0,
            ts: now_ms,
            r#type: ty,
            app: identity.app.clone(),
            process: identity.process.clone(),
            window_title: identity.window_title.clone(),
            payload,
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    /// Run the exclusion check and set flags on a hit (doc 05 §4, doc 13 §4).
    /// This is the normalizer-side enforcement; the sampler also gates frame
    /// capture independently (doc 05 §4). Both are required: a metadata-only
    /// event for an excluded context must still carry the flag.
    ///
    /// Excluded events also drop their `window_title` — the title itself can
    /// leak the sensitive context (doc 13 §4 "metadata-only").
    pub fn apply_exclusion(&self, ev: &mut Event, url: Option<&str>) -> ExclusionVerdict {
        let verdict = self.exclusion.is_excluded(
            ev.process.as_deref(),
            None, // window_class travels on identity; title/process cover the M1 gate
            ev.window_title.as_deref(),
            url,
        );
        if let ExclusionVerdict::Excluded { flags, .. } = &verdict {
            ev.redaction_flags |= flags;
            ev.window_title = None; // metadata-only (doc 13 §4)
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exclusion::ExclusionRule;
    use aperture_contracts::event::redaction_flags;

    fn identity(process: &str, title: &str) -> WindowIdentity {
        WindowIdentity {
            app: Some("App".into()),
            process: Some(process.into()),
            window_title: Some(title.into()),
            window_class: None,
        }
    }

    #[test]
    fn focus_event_reaches_the_bus_with_identity() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let n = Normalizer::new(bus, ExclusionList::shipped_defaults());

        let out = n.normalize_hook(
            &HookEvent::ForegroundChanged { hwnd: 1 },
            identity("code.exe", "main.rs — VS Code"),
            123,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].capture_frame);
        let ev = rx.try_recv().expect("published");
        assert_eq!(ev.r#type, EventType::WindowFocus);
        assert_eq!(ev.process.as_deref(), Some("code.exe"));
        assert_eq!(ev.ts, 123);
        assert_eq!(ev.id, 0, "DB assigns ids (doc 15 §1)");
    }

    #[test]
    fn excluded_context_is_metadata_only_and_never_samples() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let list = ExclusionList::compile(vec![ExclusionRule {
            process: Some("1password.exe".into()),
            label: "1Password".into(),
            ..Default::default()
        }]);
        let n = Normalizer::new(bus, list);

        let out = n.normalize_hook(
            &HookEvent::ForegroundChanged { hwnd: 2 },
            identity("1password.exe", "1Password — vault"),
            1,
        );
        assert!(!out[0].capture_frame, "no frame for excluded contexts (doc 05 §4)");
        let ev = rx.try_recv().expect("metadata-only event still published");
        assert_ne!(ev.redaction_flags & redaction_flags::EXCLUDED, 0);
        assert_eq!(ev.window_title, None, "title stripped (doc 13 §4)");
    }

    #[test]
    fn window_close_never_requests_a_frame() {
        let bus = EventBus::new();
        let n = Normalizer::new(bus, ExclusionList::shipped_defaults());
        let out = n.normalize_hook(
            &HookEvent::WindowClosed { hwnd: 3 },
            identity("code.exe", "gone"),
            1,
        );
        assert_eq!(out[0].event.r#type, EventType::WindowClose);
        assert!(!out[0].capture_frame, "closed window has nothing to sample");
    }
}
