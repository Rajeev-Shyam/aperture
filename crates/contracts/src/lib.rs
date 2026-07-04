//! # Aperture interface contracts (doc 15)
//!
//! Five contracts make every subsystem independently buildable and testable.
//! Components may depend on **these and nothing else** across subsystem
//! boundaries:
//!
//! 1. [`event`] — the Event envelope: the bus message **and** the DB row (doc 15 §1).
//! 2. [`context_payload`] — the one object that is built, previewed, and sent
//!    (`aperture/context-payload/v1`, doc 03 §4 / doc 15 §2).
//! 3. [`connector`] — the [`connector::Connector`] trait, the expansion seam (doc 10 / doc 15 §3).
//! 4. [`gpu_job`] — the GPU job contract; callers never touch the GPU directly (doc 15 §4).
//! 5. [`reasoning`] — the [`reasoning::ReasoningTransport`] trait + the source-agnostic
//!    [`suggestions::StructuredSuggestions`] shape (doc 09 / doc 15 §5).
//!
//! ## Compatibility law (doc 15 §6)
//! Additive-only fields everywhere; unknown-field tolerance is mandatory; a
//! breaking change requires a new schema `$id` / `payload_version` plus a
//! migration. Because every boundary type lives in this one crate, drift is a
//! compile error — not a runtime surprise.

pub mod connector;
pub mod context_payload;
pub mod event;
pub mod gpu_job;
pub mod reasoning;
pub mod suggestions;

#[cfg(feature = "fakes")]
pub mod fakes;

/// Re-export the surface most callers want.
pub use connector::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};
pub use context_payload::{
    ContextPayload, Intent, PayloadItem, Redaction, TransportTarget, EVENT_TRAIL_MAX,
    PAYLOAD_SIZE_WARN_BYTES,
};
pub use event::{Event, EventType};
pub use gpu_job::{GpuJob, GpuJobKind, GpuScheduler, JobError, JobOutput};
pub use reasoning::{Health, ReasoningTransport, TransportError, TransportId};
pub use suggestions::{BubbleSpec, StructuredSuggestions, SuggestionCandidate};
