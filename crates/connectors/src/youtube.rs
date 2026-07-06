//! YouTube video-timestamp connector — the US1 flagship (doc 10 §3).
//!
//! Detects `youtube.com/watch` and `youtu.be/` navigations plus extension-fed
//! `media_state` events, parses the video id and position, and (on resume)
//! reconstructs `watch?v=<id>&t=<s>s` — or, when the position is unknown, a
//! plain watch URL with the bubble honestly saying "from the start" (US1
//! acceptance d).
//!
//! Position capture follows the R2 hierarchy (doc 10 §3, ADR-027 — RK3 resolved
//! by *building the extension*, not spiking):
//!   1. the **browser-extension content script reads `video.currentTime`** and
//!      it arrives here as a `media_state` event — the primary, reliable source;
//!   2. `t=` present in the observed URL (e.g. after "copy at current time") —
//!      exact fallback;
//!   3. otherwise unknown ⇒ `position_s = null` ⇒ "from the start".

use std::time::Duration;

use aperture_contracts::connector::ConnectorError;
use aperture_contracts::event::{Event, EventType};
use aperture_contracts::{Connector, ConnectorState, OpenOutcome, ResumeArtifact};

use crate::deeplinker;

/// Payload v1 schema (doc 10 §3): `{video_id, url, title, position_s|null, observed_ts}`
/// plus the additive `position_source` field (doc 15 §6 — additive-only, unknown
/// fields tolerated) so bubble copy and SC7 telemetry can be honest about certainty.
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
    /// How `position_s` was obtained (additive; defaults to `None`-source when
    /// absent in stored payloads written before this field existed).
    #[serde(default)]
    pub position_source: PositionSource,
}

/// How the position in [`YoutubePayloadV1::position_s`] was obtained — recorded
/// so the bubble copy and SC7 telemetry can be honest about certainty (doc 10 §3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionSource {
    /// `t=` was present in the observed URL — exact.
    UrlTimestamp,
    /// From the extension content script's `video.currentTime` (rung 1, ADR-027)
    /// riding a `media_state` event — the primary, reliable source.
    MediaState,
    /// Unknown — `position_s` is null.
    #[default]
    None,
}

/// The YouTube connector — **built first at M4** (Q75) since it exercises the
/// whole extension + native-messaging path earliest.
#[derive(Debug, Default, Clone)]
pub struct YoutubeConnector;

const TTL_7D: Duration = Duration::from_secs(7 * 24 * 60 * 60);

impl YoutubeConnector {
    pub fn new() -> Self {
        Self
    }

    /// Parse `(video_id, position_s)` from a watch URL, honoring the position
    /// hierarchy's `t=` rung. Pure + unit-testable; no I/O.
    ///
    /// Accepts `youtube.com/watch?v=<id>` (www./m. hosts) and `youtu.be/<id>`,
    /// with `t=` in `123`, `123s`, and `1h2m3s` forms ([VERIFY] confirmed —
    /// YouTube emits both the bare-seconds and h/m/s forms).
    pub fn parse_watch_url(url: &str) -> Option<(String, Option<u32>)> {
        let parsed = url::Url::parse(url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return None;
        }
        let host = parsed.host_str()?.to_ascii_lowercase();
        let host = host.strip_prefix("www.").unwrap_or(&host);

        let video_id = match host {
            "youtube.com" | "m.youtube.com" if parsed.path() == "/watch" => parsed
                .query_pairs()
                .find(|(k, _)| k == "v")
                .map(|(_, v)| v.into_owned())?,
            "youtu.be" => parsed.path_segments()?.next()?.to_string(),
            _ => return None,
        };
        if !is_plausible_video_id(&video_id) {
            return None;
        }
        let position_s = parsed
            .query_pairs()
            .find(|(k, _)| k == "t")
            .and_then(|(_, v)| parse_t_param(&v));
        Some((video_id, position_s))
    }

    /// Build the resume URL from id + position (doc 10 §3 reconstruct rule).
    /// `Some(s)` ⇒ `https://www.youtube.com/watch?v=<id>&t=<s>s`;
    /// `None` ⇒ plain `https://www.youtube.com/watch?v=<id>` (degrade path).
    /// Always the canonical `www.youtube.com/watch` host, regardless of the
    /// captured short form; `&t=` because `?v=` is always present.
    pub fn build_resume_url(video_id: &str, position_s: Option<u32>) -> String {
        match position_s {
            Some(s) => format!("https://www.youtube.com/watch?v={video_id}&t={s}s"),
            None => format!("https://www.youtube.com/watch?v={video_id}"),
        }
    }
}

/// YouTube ids are URL-safe base64-ish; require that charset and a sane length
/// so garbage query values never become a "resumable" state.
fn is_plausible_video_id(id: &str) -> bool {
    (6..=16).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Parse a `t=` value: `123`, `123s`, or `1h2m3s` (any subset/order-forward of
/// h/m/s). Returns `None` for anything unparseable — no position beats a wrong one.
fn parse_t_param(raw: &str) -> Option<u32> {
    if raw.is_empty() {
        return None;
    }
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw.parse().ok();
    }
    let mut total: u64 = 0;
    let mut acc: u64 = 0;
    let mut saw_digit = false;
    for b in raw.bytes() {
        match b {
            b'0'..=b'9' => {
                acc = acc.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
                saw_digit = true;
            }
            b'h' | b'H' => {
                if !saw_digit {
                    return None;
                }
                total = total.checked_add(acc.checked_mul(3600)?)?;
                acc = 0;
                saw_digit = false;
            }
            b'm' | b'M' => {
                if !saw_digit {
                    return None;
                }
                total = total.checked_add(acc.checked_mul(60)?)?;
                acc = 0;
                saw_digit = false;
            }
            b's' | b'S' => {
                if !saw_digit {
                    return None;
                }
                total = total.checked_add(acc)?;
                acc = 0;
                saw_digit = false;
            }
            _ => return None,
        }
    }
    // A trailing bare number (e.g. "90" already handled; "1m30" tolerated as 30 s tail).
    total = total.checked_add(acc)?;
    u32::try_from(total).ok()
}

/// Cheap payload sniffs shared by `can_capture`/`capture`.
fn payload_str<'a>(ev: &'a Event, key: &str) -> Option<&'a str> {
    ev.payload.get(key).and_then(|v| v.as_str())
}

fn looks_like_watch_url(url: &str) -> bool {
    url.contains("youtube.com/watch") || url.contains("youtu.be/")
}

impl Connector for YoutubeConnector {
    fn id(&self) -> &'static str {
        "youtube"
    }

    fn can_capture(&self, ev: &Event) -> bool {
        // Cheap predicate (doc 10 §1): full parsing happens in `capture`.
        // Registered ahead of BrowserConnector so the more-specific match wins.
        match ev.r#type {
            EventType::Navigation => payload_str(ev, "url").is_some_and(looks_like_watch_url),
            EventType::MediaState => payload_str(ev, "video_id").is_some(),
            _ => false,
        }
    }

    fn capture(&self, ev: &Event) -> Option<ConnectorState> {
        // The R2 position hierarchy (doc 10 §3, ADR-027):
        //   media_state (extension currentTime) > URL t= > null.
        let (video_id, url, position_s, position_source) = match ev.r#type {
            EventType::MediaState => {
                let video_id = payload_str(ev, "video_id")?.to_string();
                if !is_plausible_video_id(&video_id) {
                    return None;
                }
                let url = payload_str(ev, "url").unwrap_or_default().to_string();
                match ev.payload.get("position_s").and_then(|v| v.as_f64()) {
                    Some(pos) if pos.is_finite() && pos >= 0.0 => {
                        (video_id, url, Some(pos as u32), PositionSource::MediaState)
                    }
                    // No position on the media event: fall to the URL's t= rung.
                    _ => match Self::parse_watch_url(&url) {
                        Some((_, Some(t))) => (video_id, url, Some(t), PositionSource::UrlTimestamp),
                        _ => (video_id, url, None, PositionSource::None),
                    },
                }
            }
            EventType::Navigation => {
                let url = payload_str(ev, "url")?.to_string();
                let (video_id, t) = Self::parse_watch_url(&url)?;
                let source = if t.is_some() {
                    PositionSource::UrlTimestamp
                } else {
                    PositionSource::None
                };
                (video_id, url, t, source)
            }
            _ => return None,
        };

        let title = payload_str(ev, "title")
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| ev.window_title.clone())
            .unwrap_or_default();

        let payload = YoutubePayloadV1 {
            video_id,
            url,
            title,
            position_s,
            observed_ts: ev.ts,
            position_source,
        };
        Some(crate::build_state(
            self.id(),
            serde_json::to_value(payload).ok()?,
            ev.ts,
            self.staleness_ttl(),
        ))
    }

    fn staleness_ttl(&self) -> Duration {
        // TTL 7 d (doc 10 §3 [ASSUMPTION]).
        TTL_7D
    }

    fn reconstruct(&self, st: &ConnectorState) -> Result<ResumeArtifact, ConnectorError> {
        // Payloads are additive-only (doc 15 §6), so any version parses as v1.
        let payload: YoutubePayloadV1 = serde_json::from_value(st.reconstruct_payload.clone())
            .map_err(|e| ConnectorError::DispatchFailed(format!("bad youtube payload: {e}")))?;
        // `None` position is NOT an error — it's the honest "from the start"
        // degrade; `open` downgrades the outcome so SC7 stays honest.
        Ok(ResumeArtifact::Url(Self::build_resume_url(
            &payload.video_id,
            payload.position_s,
        )))
    }

    fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome, ConnectorError> {
        let outcome = deeplinker::open(a)?;
        // A watch URL without `t=` means the position was unknown: honest
        // Degraded outcome → "from the start" copy + SC7 telemetry (doc 10 §3, §6).
        if let (OpenOutcome::Resumed, ResumeArtifact::Url(url)) = (&outcome, a) {
            if !url.contains("&t=") {
                return Ok(OpenOutcome::Degraded {
                    reason: "position unknown — opened from the start".into(),
                });
            }
        }
        Ok(outcome)
    }

    fn validate(&self, cloud_payload: &serde_json::Value) -> Option<ConnectorState> {
        // Gate for Claude-suggested YouTube actions (doc 09 §4, ADR-035): require
        // a plausible video_id; clamp position to a sane u32; reject otherwise so
        // the action button is withheld. The VLM/cloud only *suggests* — this
        // connector re-derives a well-formed state or nothing.
        let video_id = cloud_payload.get("video_id").and_then(|v| v.as_str())?;
        if !is_plausible_video_id(video_id) {
            return None;
        }
        let position_s = cloud_payload
            .get("position_s")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        let now_ms = crate::epoch_ms();
        let payload = YoutubePayloadV1 {
            video_id: video_id.to_string(),
            url: Self::build_resume_url(video_id, position_s),
            title: cloud_payload
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            position_s,
            observed_ts: now_ms,
            position_source: if position_s.is_some() {
                PositionSource::UrlTimestamp
            } else {
                PositionSource::None
            },
        };
        Some(crate::build_state(
            self.id(),
            serde_json::to_value(payload).ok()?,
            now_ms,
            self.staleness_ttl(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nav_event(url: &str, title: Option<&str>) -> Event {
        Event {
            id: 1,
            ts: 1_000_000,
            r#type: EventType::Navigation,
            app: Some("browser".into()),
            process: Some("chrome.exe".into()),
            window_title: title.map(str::to_string),
            payload: json!({ "url": url, "browser": "chrome" }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    fn media_event(video_id: &str, url: &str, position_s: Option<f64>) -> Event {
        let mut payload = json!({
            "url": url,
            "video_id": video_id,
            "state": "playing",
            "title": "Some video",
        });
        if let Some(p) = position_s {
            payload["position_s"] = json!(p);
        }
        Event {
            id: 2,
            ts: 2_000_000,
            r#type: EventType::MediaState,
            app: Some("browser".into()),
            process: Some("chrome.exe".into()),
            window_title: None,
            payload,
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    #[test]
    fn parses_watch_urls() {
        assert_eq!(
            YoutubeConnector::parse_watch_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            Some(("dQw4w9WgXcQ".into(), None))
        );
        assert_eq!(
            YoutubeConnector::parse_watch_url("https://youtube.com/watch?v=dQw4w9WgXcQ&t=754"),
            Some(("dQw4w9WgXcQ".into(), Some(754)))
        );
        assert_eq!(
            YoutubeConnector::parse_watch_url("https://m.youtube.com/watch?v=dQw4w9WgXcQ&t=90s"),
            Some(("dQw4w9WgXcQ".into(), Some(90)))
        );
        assert_eq!(
            YoutubeConnector::parse_watch_url("https://youtu.be/dQw4w9WgXcQ?t=1h2m3s"),
            Some(("dQw4w9WgXcQ".into(), Some(3723)))
        );
    }

    #[test]
    fn rejects_non_watch_urls() {
        assert_eq!(YoutubeConnector::parse_watch_url("https://example.com/watch?v=abc123"), None);
        assert_eq!(YoutubeConnector::parse_watch_url("https://www.youtube.com/feed/history"), None);
        assert_eq!(YoutubeConnector::parse_watch_url("not a url"), None);
        // Implausible id.
        assert_eq!(YoutubeConnector::parse_watch_url("https://www.youtube.com/watch?v=a"), None);
    }

    #[test]
    fn builds_resume_urls() {
        assert_eq!(
            YoutubeConnector::build_resume_url("dQw4w9WgXcQ", Some(754)),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=754s"
        );
        assert_eq!(
            YoutubeConnector::build_resume_url("dQw4w9WgXcQ", None),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn media_state_position_wins_over_url_t() {
        // Rung 1 (extension currentTime) beats rung 2 (t= param) — ADR-027.
        let c = YoutubeConnector::new();
        let ev = media_event("dQw4w9WgXcQ", "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10", Some(123.9));
        let st = c.capture(&ev).expect("captured");
        let p: YoutubePayloadV1 = serde_json::from_value(st.reconstruct_payload).unwrap();
        assert_eq!(p.position_s, Some(123));
        assert_eq!(p.position_source, PositionSource::MediaState);
    }

    #[test]
    fn media_state_without_position_falls_to_url_t() {
        let c = YoutubeConnector::new();
        let ev = media_event("dQw4w9WgXcQ", "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10", None);
        let st = c.capture(&ev).expect("captured");
        let p: YoutubePayloadV1 = serde_json::from_value(st.reconstruct_payload).unwrap();
        assert_eq!(p.position_s, Some(10));
        assert_eq!(p.position_source, PositionSource::UrlTimestamp);
    }

    #[test]
    fn navigation_without_t_captures_null_position() {
        let c = YoutubeConnector::new();
        let ev = nav_event("https://www.youtube.com/watch?v=dQw4w9WgXcQ", Some("Video - YouTube"));
        let st = c.capture(&ev).expect("captured");
        assert_eq!(st.connector_type, "youtube");
        assert_eq!(st.payload_version, 1);
        assert_eq!(st.stale_after_ts, Some(ev.ts + 7 * 24 * 60 * 60 * 1000));
        let p: YoutubePayloadV1 = serde_json::from_value(st.reconstruct_payload).unwrap();
        assert_eq!(p.position_s, None);
        assert_eq!(p.position_source, PositionSource::None);
        assert_eq!(p.title, "Video - YouTube");
    }

    #[test]
    fn reconstructs_with_and_without_position() {
        let c = YoutubeConnector::new();
        let st = c
            .capture(&media_event("dQw4w9WgXcQ", "https://youtu.be/dQw4w9WgXcQ", Some(754.0)))
            .unwrap();
        match c.reconstruct(&st).unwrap() {
            ResumeArtifact::Url(u) => {
                assert_eq!(u, "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=754s")
            }
            other => panic!("expected Url, got {other:?}"),
        }
        let st = c
            .capture(&nav_event("https://www.youtube.com/watch?v=dQw4w9WgXcQ", None))
            .unwrap();
        match c.reconstruct(&st).unwrap() {
            ResumeArtifact::Url(u) => assert_eq!(u, "https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            other => panic!("expected Url, got {other:?}"),
        }
    }

    #[test]
    fn can_capture_is_a_cheap_sniff() {
        let c = YoutubeConnector::new();
        assert!(c.can_capture(&nav_event("https://www.youtube.com/watch?v=dQw4w9WgXcQ", None)));
        assert!(!c.can_capture(&nav_event("https://example.com/", None)));
        assert!(c.can_capture(&media_event("dQw4w9WgXcQ", "", Some(1.0))));
    }

    #[test]
    fn validate_gates_cloud_payloads() {
        let c = YoutubeConnector::new();
        // Well-formed → state.
        let st = c
            .validate(&json!({ "video_id": "dQw4w9WgXcQ", "position_s": 90, "title": "T" }))
            .expect("valid");
        let p: YoutubePayloadV1 = serde_json::from_value(st.reconstruct_payload).unwrap();
        assert_eq!(p.position_s, Some(90));
        // Missing/garbage id → button withheld.
        assert!(c.validate(&json!({ "position_s": 90 })).is_none());
        assert!(c.validate(&json!({ "video_id": "***" })).is_none());
    }

    #[test]
    fn t_param_parser_edge_cases() {
        assert_eq!(parse_t_param("754"), Some(754));
        assert_eq!(parse_t_param("754s"), Some(754));
        assert_eq!(parse_t_param("1h2m3s"), Some(3723));
        assert_eq!(parse_t_param("2m"), Some(120));
        assert_eq!(parse_t_param(""), None);
        assert_eq!(parse_t_param("abc"), None);
        assert_eq!(parse_t_param("1x"), None);
    }
}
