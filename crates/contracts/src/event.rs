//! Contract 1 — the Event envelope (doc 15 §1, taxonomy in doc 03 §2).
//!
//! Transport: in-process `tokio::sync::broadcast` in the Rust core; the Tauri
//! `invoke`/event channel bridges core <-> WebView. **SQLite is the durable
//! form** — the bus is at-most-once, the DB is the truth.
//!
//! Versioning: `payload` is additive-only; consumers ignore unknown fields.

use serde::{Deserialize, Serialize};

/// The normalized event — identical shape on the bus and as the `events` DB row.
///
/// `id` is `0` for an in-flight bus message and is assigned by the DB on insert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    /// epoch milliseconds.
    pub ts: i64,
    #[serde(rename = "type")]
    pub r#type: EventType,
    pub app: Option<String>,
    pub process: Option<String>,
    pub window_title: Option<String>,
    /// Type-specific fields (doc 03 §2). Additive-only; tolerate unknown keys.
    pub payload: serde_json::Value,
    pub connector_id: Option<String>,
    /// Assigned by the sessionizer (doc 08 §3).
    pub session_id: Option<i64>,
    /// Bitflags; see [`redaction_flags`].
    pub redaction_flags: u32,
}

/// Redaction / handling bitflags stored on every event.
pub mod redaction_flags {
    /// The foreground app/window matched the exclusion list (doc 05 §4, doc 13 §4):
    /// metadata-only event, no frame, no OCR — and it can never enter a payload.
    pub const EXCLUDED: u32 = 1 << 0;
    /// A private/incognito browser window, treated as excluded (doc 13 §4).
    pub const PRIVATE_WINDOW: u32 = 1 << 1;
}

/// The event taxonomy (doc 03 §2). String-serialized so the DB `type` column and
/// the JSON bus message agree, and so adding a variant is additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    WindowFocus,
    WindowOpen,
    WindowClose,
    /// UIA address-bar read (doc 05 §3, RK4).
    Navigation,
    /// connector heuristics: `{ url, video_id, position_s?, state }`.
    MediaState,
    DocumentState,
    IdeState,
    /// STT output (doc 07); always written (telemetry role, locked decision B).
    VoiceUtterance,
    SuggestionShown,
    SuggestionClicked,
    SuggestionDismissed,
    /// Audit: capture toggled on/off (doc 12 §6). Survives Purge All for 30 d.
    CaptureToggle,
    /// Audit: bytes left the machine (doc 09). Survives Purge All for 30 d.
    CloudSend,
}

impl EventType {
    /// All variants — used by the M0 gate test that round-trips every event type
    /// through the schema (doc 16 M0).
    pub const ALL: [EventType; 13] = [
        EventType::WindowFocus,
        EventType::WindowOpen,
        EventType::WindowClose,
        EventType::Navigation,
        EventType::MediaState,
        EventType::DocumentState,
        EventType::IdeState,
        EventType::VoiceUtterance,
        EventType::SuggestionShown,
        EventType::SuggestionClicked,
        EventType::SuggestionDismissed,
        EventType::CaptureToggle,
        EventType::CloudSend,
    ];
}
