//! YouTube video-timestamp connector — the US1 flagship (doc 10 §3).
//!
//! Detects `youtube.com/watch` and `youtu.be/` navigations, parses the video id
//! and any `t=` timestamp, and (on resume) reconstructs `watch?v=<id>&t=<s>s` —
//! or, when the position is unknown, a plain watch URL with the bubble honestly
//! saying "from the start" (US1 acceptance d).
//!
//! Reliability class **RK3** — the least-certain connector. Position capture
//! follows an **honest hierarchy** (doc 10 §3):
//!   1. `t=` present in the observed URL (e.g. after "copy at current time") — exact;
//!   2. periodic `media_state` heuristics where obtainable (player UIA exposure
//!      is unreliable [VERIFY]);
//!   3. otherwise unknown ⇒ `position_s = null`.
//!
//! This is the M4 de-risk spike: if no reliable position source exists without a
//! companion extension, v1 ships the degrade path *knowingly*.

use std::time::Duration;

use aperture_contracts::connector::ConnectorError;
use aperture_contracts::event::{Event, EventType};
use aperture_contracts::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};

use crate::deeplinker;

/// Payload v1 schema (doc 10 §3): `{video_id, url, title, position_s|null, observed_ts}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct YoutubePayloadV1 {
    /// The `v=` / `youtu.be/` path id.
    pub video_id: String,
    /// The observed watch URL (kept verbatim for fidelity/debugging).
    pub url: String,
    pub title: String,
    /// Playback position in whole seconds, or `None` when unknown (the degrade
    /// case — reconstruct emits a plain watch URL and the bubble says
    /// "from the start").
    pub position_s: Option<u32>,
    /// Epoch ms the navigation/position was observed.
    pub observed_ts: i64,
}

/// How the position in [`YoutubePayloadV1::position_s`] was obtained — recorded
/// so the bubble copy and SC7 telemetry can be honest about certainty (doc 10 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionSource {
    /// `t=` was present in the observed URL — exact.
    UrlTimestamp,
    /// Derived from a `media_state` heuristic — best-effort.
    MediaState,
    /// Unknown — `position_s` is null.
    None,
}

/// The YouTube connector. Reliability class **RK3** (doc 10 §3).
#[derive(Debug, Default, Clone)]
pub struct YoutubeConnector;

impl YoutubeConnector {
    pub fn new() -> Self {
        Self
    }

    /// Parse `(video_id, position_s)` from a watch URL, honoring the position
    /// hierarchy's first rung (the `t=` param). Pure + unit-testable; no I/O.
    ///
    /// Accepts `youtube.com/watch?v=<id>&t=<n>[s]` and `youtu.be/<id>?t=<n>[s]`.
    // TODO(M4): implement with the `url` crate; strip a trailing `s` on `t=`;
    //   tolerate `t=1h2m3s` forms [VERIFY]. Return None if no recognizable id.
    pub fn parse_watch_url(_url: &str) -> Option<(String, Option<u32>)> {
        todo!("M4: parse v=/youtu.be id + optional t= seconds via the `url` crate")
    }

    /// Build the resume URL from id + position (doc 10 §3 reconstruct rule).
    /// `Some(s)` ⇒ `https://www.youtube.com/watch?v=<id>&t=<s>s`;
    /// `None` ⇒ plain `https://www.youtube.com/watch?v=<id>` (degrade path).
    // TODO(M4): use `&t=` since `?v=` is always present; keep it the canonical
    //   `www.youtube.com/watch` host regardless of the captured short form.
    pub fn build_resume_url(_video_id: &str, _position_s: Option<u32>) -> String {
        todo!("M4: format watch?v=<id>(&t=<s>s) | from-the-start plain URL")
    }
}

impl Connector for YoutubeConnector {
    fn id(&self) -> &'static str {
        "youtube"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // Claims navigation/media_state events whose URL is a YouTube watch host.
        // Registered ahead of BrowserConnector so the more-specific match wins.
        // TODO(M4): sniff `ev.payload.url` for youtube.com/watch | youtu.be/.
        matches!(ev.r#type, EventType::Navigation | EventType::MediaState)
    }

    fn capture(&self, _ev: &Event) -> Option<ConnectorState> {
        // TODO(M4): apply the position hierarchy (doc 10 §3):
        //   1. t= in the observed URL ⇒ PositionSource::UrlTimestamp;
        //   2. else a fresh media_state heuristic ⇒ PositionSource::MediaState
        //      (player UIA is unreliable [VERIFY]);
        //   3. else position_s = None ⇒ PositionSource::None.
        //   Build YoutubePayloadV1 (observed_ts = ev.ts), serialize into
        //   `reconstruct_payload`, payload_version = 1,
        //   stale_after_ts = captured_ts + TTL_7D.
        todo!("M4: position-hierarchy capture (RK3) → YoutubePayloadV1 / ConnectorState")
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 7 d (doc 10 §3 [ASSUMPTION]).
        Duration::from_secs(7 * 24 * 60 * 60)
    }

    fn reconstruct(&self, _st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        // TODO(M4): deserialize YoutubePayloadV1 (dispatch on payload_version),
        //   then ResumeArtifact::Url(build_resume_url(video_id, position_s)).
        //   `None` position is NOT an error — it's the honest "from the start"
        //   degrade; the OpenOutcome (Resumed vs Degraded) is decided in `open`
        //   based on whether a position was present.
        todo!("M4: YoutubePayloadV1 → ResumeArtifact::Url (with-or-without &t=)")
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        // TODO(M4): dispatch via deeplinker::open; if the reconstructed URL had
        //   no `t=` (null position), surface OpenOutcome::Degraded{reason:
        //   "position unknown — opened from the start"} so the bubble copy and
        //   SC7 telemetry stay honest (doc 10 §3, §6). A private/removed video
        //   surfaces as Failed once the launch target is unreachable.
        deeplinker::open(a)
    }

    fn validate(&self, _cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // TODO(M7): gate Claude-suggested YouTube actions (doc 09 §4) — require a
        //   parseable video_id; clamp/parse position_s; reject otherwise so the
        //   button is withheld.
        todo!("M7: validate cloud payload → require video_id → ConnectorState or None")
    }
}
