//! M4 gate — US1 state resumption, offline half (doc 16 M4).
//!
//! Doc 16's M4 gate: "US1 end-to-end: YouTube reopens at the right timestamp
//! via the extension content-script `currentTime`, and the 'from the start'
//! degrade is honest; document/IDE/browser resume each pass on 3 real apps;
//! extension installs, native messaging works (loopback whitelisted in SC5),
//! exclusions honored through it."
//!
//! Split, honestly:
//! * **This file** — the deterministic, offline half: connector capture →
//!   `connector_state` persistence → freshest-lookup → render → reconstruct,
//!   over synthetic events and a scratch DB. No dispatch (`open()` would launch
//!   real apps — that is the on-target half).
//! * **`aperture-capture` tests** — the native-messaging bridge: pipe auth,
//!   toggle-OFF halting forwarding (FIX 2.1), `url_pattern` exclusion through
//!   the extension path (FIX 2.2). The gate arm runs them.
//! * **On-target (manual, recorded in the gate report)** — real Chrome +
//!   unpacked extension + `aperture-nm-host install`; a real YouTube video
//!   reopened at the right timestamp; document/IDE/browser resume on 3 real
//!   apps each.

use aperture_connectors::{default_registry, natural_key, ResumeArtifact};
use aperture_contracts::event::{Event, EventType};
use aperture_db::Db;

fn media_event(video_id: &str, position_s: f64, ts: i64) -> Event {
    Event {
        id: 1,
        ts,
        r#type: EventType::MediaState,
        app: Some("browser".into()),
        process: Some("chrome.exe".into()),
        window_title: None,
        payload: serde_json::json!({
            "url": format!("https://www.youtube.com/watch?v={video_id}"),
            "video_id": video_id,
            "position_s": position_s,
            "state": "playing",
            "title": "Rust lifetimes explained",
            "browser": "chrome",
        }),
        connector_id: None,
        session_id: None,
        redaction_flags: 0,
    }
}

/// US1 core: the extension-fed position (`media_state`, rung 1) reconstructs
/// to the exact `&t=<s>s` watch URL.
#[test]
fn us1_youtube_reconstructs_the_exact_timestamp() {
    let registry = default_registry();
    let ev = media_event("dQw4w9WgXcQ", 754.6, 1_000_000);
    let state = registry.capture(&ev).expect("youtube connector claims media_state");
    assert_eq!(state.connector_type, "youtube");

    let connector = registry.by_type("youtube").unwrap();
    match connector.reconstruct(&state).expect("reconstruct") {
        ResumeArtifact::Url(url) => {
            assert_eq!(url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=754s");
        }
        other => panic!("expected Url artifact, got {other:?}"),
    }
}

/// US1 acceptance d: no position ⇒ plain watch URL and the bubble SAYS
/// "from the start" — the degrade is stated, never hidden.
#[test]
fn us1_null_position_degrade_is_honest() {
    let registry = default_registry();
    let ev = Event {
        payload: serde_json::json!({
            "url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "browser": "chrome",
            "title": "Rust lifetimes explained",
        }),
        r#type: EventType::Navigation,
        ..media_event("dQw4w9WgXcQ", 0.0, 2_000_000)
    };
    let state = registry.capture(&ev).expect("captured");

    // Reconstruct: plain watch URL, no fabricated t=.
    let connector = registry.by_type("youtube").unwrap();
    match connector.reconstruct(&state).expect("reconstruct") {
        ResumeArtifact::Url(url) => {
            assert_eq!(url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        }
        other => panic!("expected Url artifact, got {other:?}"),
    }

    // Render: the copy says so (US1 acceptance d).
    let cand = aperture_contracts::SuggestionCandidate {
        action_template: "Continue {title} — {position}".into(),
        connector_id: state.id.clone(),
        confidence: 0.9,
        pattern_id: 1,
    };
    let spec = aperture_suggestion_generator::render(&cand, &state, 2_100_000);
    assert!(
        spec.title.contains("from the start"),
        "bubble copy must state the degrade, got: {}",
        spec.title
    );
}

/// Path A step 4 → Path B steps 2-3 through the real DB surface the pipeline
/// uses: persist the captured state, find it as the freshest for its type,
/// load it by id (the bubble's `action_ref`), reconstruct.
#[test]
fn path_a_capture_persists_and_path_b_reads_back() {
    let db = Db::open_in_memory().expect("scratch db");
    let registry = default_registry();

    // Two captures of the SAME video: the second (fresher position) must win.
    let st1 = registry.capture(&media_event("dQw4w9WgXcQ", 100.0, 1_000)).unwrap();
    db.insert_connector_state(&st1).unwrap();
    let st2 = registry.capture(&media_event("dQw4w9WgXcQ", 200.0, 2_000)).unwrap();
    // Same resource — the pipeline coalesces via natural_key: refresh in place.
    assert_eq!(
        natural_key(&st1.connector_type, &st1.reconstruct_payload),
        natural_key(&st2.connector_type, &st2.reconstruct_payload),
        "same video ⇒ same natural key"
    );
    assert!(db
        .refresh_connector_state(&st1.id, &st2.reconstruct_payload, st2.captured_ts, st2.stale_after_ts)
        .unwrap());

    // Trigger rule 3's lookup: freshest non-stale youtube state.
    let fresh = db.fresh_connector_states("youtube", 2_000, 8).unwrap();
    assert_eq!(fresh.len(), 1, "coalesced: one row for one video");
    let action_ref = fresh[0].id.clone();

    // Path B: action_ref → row → reconstruct at the REFRESHED position.
    let st = db.read_connector_state(&action_ref).unwrap().expect("row exists");
    let connector = registry.by_type(&st.connector_type).unwrap();
    match connector.reconstruct(&st).unwrap() {
        ResumeArtifact::Url(url) => {
            assert_eq!(url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=200s");
        }
        other => panic!("expected Url artifact, got {other:?}"),
    }

    // Staleness boundary: past the 7 d TTL the state stops being offered.
    let past_ttl = 2_000 + 7 * 24 * 60 * 60 * 1000 + 1;
    assert!(
        db.fresh_connector_states("youtube", past_ttl, 8).unwrap().is_empty(),
        "stale states are never offered (doc 08 §5)"
    );
}

/// Document + IDE connectors resume real (temp) files; a deleted target is
/// TargetGone at reconstruct (validate-on-click, ADR-035), never a dispatch.
#[test]
fn document_and_ide_resume_and_revalidate_on_click() {
    let dir = std::env::temp_dir().join("aperture-m4-gate");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("report.docx");
    std::fs::write(&doc, b"x").unwrap();
    let code = dir.join("main.rs");
    std::fs::write(&code, b"fn main() {}").unwrap();

    let registry = default_registry();

    // Document: capture from a synthesized document_state event.
    let doc_ev = Event {
        id: 2,
        ts: 10_000,
        r#type: EventType::DocumentState,
        app: Some("office".into()),
        process: Some("winword.exe".into()),
        window_title: Some("report.docx - Word".into()),
        payload: serde_json::json!({
            "path": doc.display().to_string(),
            "app": "winword.exe",
            "title": "report.docx - Word",
        }),
        connector_id: None,
        session_id: None,
        redaction_flags: 0,
    };
    let doc_state = registry.capture(&doc_ev).expect("document captured");
    let document = registry.by_type("document").unwrap();
    match document.reconstruct(&doc_state).unwrap() {
        ResumeArtifact::FileOpen { path, .. } => assert_eq!(path, doc.display().to_string()),
        other => panic!("expected FileOpen, got {other:?}"),
    }

    // IDE: vscode://file URI with the exact path.
    let ide_ev = Event {
        id: 3,
        ts: 11_000,
        r#type: EventType::IdeState,
        app: Some("ide".into()),
        process: Some("Code.exe".into()),
        window_title: Some("main.rs - aperture - Visual Studio Code".into()),
        payload: serde_json::json!({
            "path": code.display().to_string(),
            "workspace": "aperture",
        }),
        connector_id: None,
        session_id: None,
        redaction_flags: 0,
    };
    let ide_state = registry.capture(&ide_ev).expect("ide captured");
    let ide = registry.by_type("ide").unwrap();
    match ide.reconstruct(&ide_state).unwrap() {
        ResumeArtifact::ProtocolUri(uri) => {
            assert!(uri.starts_with("vscode://file/"), "got {uri}");
            assert!(uri.ends_with("main.rs"), "got {uri}");
        }
        other => panic!("expected ProtocolUri, got {other:?}"),
    }

    // Validate-on-click: the target vanishes between capture and click ⇒
    // TargetGone (the bubble degrades; nothing executes unvalidated).
    std::fs::remove_file(&doc).unwrap();
    assert!(document.reconstruct(&doc_state).is_err(), "deleted target must not reconstruct");
}

/// Browser connector: the generic "Reopen page" resume, and the youtube-first
/// registration order (a watch URL is never claimed by the browser connector).
#[test]
fn browser_resumes_and_youtube_claims_watch_urls_first() {
    let registry = default_registry();
    let nav = |url: &str| Event {
        id: 4,
        ts: 12_000,
        r#type: EventType::Navigation,
        app: Some("browser".into()),
        process: Some("chrome.exe".into()),
        window_title: Some("Docs".into()),
        payload: serde_json::json!({ "url": url, "browser": "chrome", "title": "Docs" }),
        connector_id: None,
        session_id: None,
        redaction_flags: 0,
    };

    let page = registry.capture(&nav("https://docs.rs/tokio")).expect("browser claims");
    assert_eq!(page.connector_type, "browser");

    let watch = registry
        .capture(&nav("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=90"))
        .expect("youtube claims");
    assert_eq!(watch.connector_type, "youtube", "registration order: youtube first (doc 10 §3)");
}
