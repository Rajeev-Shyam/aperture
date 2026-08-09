//! M9 validation gate — privacy hardening (doc 13, doc 16 M9).
//!
//! The milestone's four exit criteria, each asserted end-to-end over the REAL
//! subsystems (no fakes standing in for the thing under test):
//!
//!   1. **DB unreadable without the wrapped key** (doc 13 §6).
//!   2. **Purge All verified** (doc 13 §7).
//!   3. **Excluded apps are never captured — frame-level** (doc 13 §4, doc 05 §4).
//!   4. **The audit answers "what left the machine"** (doc 13 §3).
//!
//! Honest-gate policy (doc 16): the half of criterion 1 that needs real page
//! encryption only runs when the workspace is built with the `sqlcipher`
//! feature, because that build compiles OpenSSL from source and needs a native
//! Perl + NASM on PATH. The **truthfulness** half — that the code never claims
//! encryption it did not apply — is asserted unconditionally, which is what
//! stops a plaintext build from silently passing as an encrypted one.

use std::sync::Arc;

use aperture_capture::exclusion::{ExclusionList, ExclusionRule};
use aperture_capture::normalizer::Normalizer;
use aperture_capture::hooks::{HookEvent, WindowIdentity};
use aperture_contracts::event::redaction_flags;
use aperture_contracts::{Event, EventType, TransportTarget};
use aperture_db::retention::RetentionPolicy;
use aperture_db::Db;
use aperture_event_bus::EventBus;
use aperture_privacy::audit_log::{
    AuditLog, AuditSink, CaptureToggleRecord, CloudSendRecord, ToggleReason,
};
use aperture_privacy::consent::ConsentManager;

const DAY_MS: i64 = 86_400_000;

fn event_at(ts: i64, ty: EventType) -> Event {
    Event {
        id: 0,
        ts,
        r#type: ty,
        app: Some("App".into()),
        process: Some("app.exe".into()),
        window_title: Some("t".into()),
        payload: serde_json::json!({}),
        connector_id: None,
        session_id: None,
        redaction_flags: 0,
    }
}

// ---------------------------------------------------------------------------
// 1. DB unreadable without the wrapped key (doc 13 §6)
// ---------------------------------------------------------------------------

/// The non-negotiable half: the DB must never *claim* encryption it did not
/// apply. A build without the `sqlcipher` feature reports `is_encrypted() ==
/// false`, so this gate — and the operator reading it — can tell the difference.
#[test]
fn m9_encryption_status_is_reported_truthfully() {
    let db = Db::open_in_memory().expect("open");
    assert!(
        !db.is_encrypted(),
        "an in-memory DB is never encrypted; reporting otherwise would make the gate a lie"
    );
    assert_eq!(
        aperture_db::ENCRYPTION_AVAILABLE,
        cfg!(feature = "sqlcipher"),
        "the advertised capability must track the actual build"
    );

    if !aperture_db::ENCRYPTION_AVAILABLE {
        eprintln!(
            "M9 NOTE: at-rest encryption is NOT compiled in. \
             `m9_db_is_unreadable_without_the_key` is skipped, and the M9 gate is \
             INCOMPLETE until the workspace is built with --features sqlcipher \
             (needs a native Perl + NASM on PATH). See doc 13 §6."
        );
    }
}

/// Criterion 1 proper: a DB written under key A cannot be opened under key B,
/// nor without a key. Only runs on an encryption-capable build.
#[test]
#[cfg(feature = "sqlcipher")]
fn m9_db_is_unreadable_without_the_key() {
    let dir = std::env::temp_dir().join(format!("aperture-m9-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("history.db");
    let _ = std::fs::remove_file(&path);

    let key_a = [7u8; 32];
    let key_b = [9u8; 32];

    {
        let db = Db::open_encrypted(path.clone(), &key_a).expect("create under key A");
        assert!(db.is_encrypted(), "the sqlcipher build must actually encrypt");
        db.insert_event(&event_at(1_000, EventType::WindowFocus)).expect("write");
    }

    // Re-open under the right key: readable.
    {
        let db = Db::open_encrypted(path.clone(), &key_a).expect("reopen under key A");
        assert_eq!(db.recent_audit_events(10).map(|v| v.len()).unwrap_or(0), 0);
    }

    // Wrong key and no key: both must fail closed.
    assert!(
        Db::open_encrypted(path.clone(), &key_b).is_err(),
        "a wrong key MUST NOT open the DB (doc 13 §6)"
    );
    assert!(
        Db::open_encrypted(path.clone(), &[]).is_err(),
        "an empty key MUST NOT open the DB"
    );

    // And the raw bytes on disk must not contain the plaintext SQLite header.
    let raw = std::fs::read(&path).expect("read file");
    assert!(
        !raw.starts_with(b"SQLite format 3"),
        "an encrypted DB must not carry the plaintext SQLite header"
    );

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 2. Purge All verified (doc 13 §7)
// ---------------------------------------------------------------------------

#[test]
fn m9_purge_all_wipes_history_but_keeps_the_audit_window_and_protections() {
    let db = Arc::new(Db::open_in_memory().expect("open"));
    let now = 1_700_000_000_000 + 100 * DAY_MS;

    // History + a fresh audit row + a stale audit row + the user's protections.
    let history = db.insert_event(&event_at(now - DAY_MS, EventType::WindowFocus)).unwrap();
    let stale_audit = db
        .insert_event(&event_at(now - 60 * DAY_MS, EventType::CaptureToggle))
        .unwrap();
    let audit = AuditLog::new(Arc::clone(&db));
    audit
        .record_cloud_send(CloudSendRecord {
            payload_id: uuid::Uuid::new_v4(),
            wire_sha256: aperture_privacy::audit_log::sha256_hex(b"bytes"),
            transport: TransportTarget::ClaudeCli,
            byte_count: 5,
            ts: now - DAY_MS,
        })
        .unwrap();
    let rule = db.add_exclusion_rule("process", "1password.exe").unwrap();
    db.set_setting("consent", r#"{"capture_enabled":true}"#).unwrap();

    db.purge_all(now, &RetentionPolicy::default()).expect("purge");

    assert!(db.read_event(history).is_err(), "captured history is gone");
    assert!(db.read_event(stale_audit).is_err(), "audit past 30 d expires too");
    let surviving = audit.recent(50).expect("audit readable after purge");
    assert_eq!(surviving.len(), 1, "the 30-day accountability window survives (doc 13 §7)");
    assert_eq!(surviving[0].r#type, EventType::CloudSend);

    // A privacy control must never weaken itself in the course of a purge.
    assert!(
        db.read_exclusion_list().unwrap().iter().any(|(id, ..)| *id == rule),
        "purging must NOT resume capturing apps the user excluded"
    );
    assert!(db.get_setting("consent").unwrap().is_some(), "consent is not reset by a data purge");
}

/// Purge All must remove the data from **disk**, not merely from the logical
/// tables. This is the assertion the first implementation failed: in WAL mode a
/// bare `VACUUM` on a still-open connection rewrites into `history.db-wal` and
/// leaves every purged row verbatim-recoverable from the files — plaintext in a
/// build without the `sqlcipher` feature. Scans the real bytes for a sentinel.
#[test]
fn m9_purged_content_is_not_recoverable_from_the_files_on_disk() {
    const SENTINEL: &str = "APERTURE-M9-PURGE-SENTINEL-8f3a2c";

    let dir = std::env::temp_dir().join(format!("aperture-m9-purge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("history.db");

    let now = 1_700_000_000_000i64;
    {
        // The handle stays open across the purge, exactly as the shell holds it.
        let db = Db::open_encrypted(path.clone(), &[]).expect("open");
        for i in 0..400 {
            let ctx = aperture_db::ScreenContextInsert {
                ocr_text: Some(format!("{SENTINEL} row {i} — sensitive captured text")),
                ocr_confidence: Some(0.9),
                ..Default::default()
            };
            db.insert_event_with_context(&event_at(now + i, EventType::WindowFocus), Some(&ctx), None)
                .expect("write");
        }

        // Sanity: the sentinel really is on disk before the purge, or the test
        // would pass vacuously.
        let before: usize = ["", "-wal"]
            .iter()
            .map(|suffix| count_sentinel(&with_suffix(&path, suffix), SENTINEL))
            .sum();
        assert!(before > 0, "precondition: the sentinel must be on disk before purging");

        db.purge_all(now + 1_000, &RetentionPolicy::default()).expect("purge");

        for suffix in ["", "-wal", "-shm"] {
            let file = with_suffix(&path, suffix);
            let hits = count_sentinel(&file, SENTINEL);
            assert_eq!(
                hits,
                0,
                "purged content is still recoverable from {} ({hits} hits) — \
                 Purge All must be real on disk (doc 13 §7)",
                file.display()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn with_suffix(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    if suffix.is_empty() {
        return path.to_path_buf();
    }
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

/// Count non-overlapping occurrences of `needle` in a file's raw bytes.
/// A missing file counts as zero (the `-wal`/`-shm` may be absent entirely).
fn count_sentinel(path: &std::path::Path, needle: &str) -> usize {
    let Ok(bytes) = std::fs::read(path) else { return 0 };
    let needle = needle.as_bytes();
    if needle.is_empty() || bytes.len() < needle.len() {
        return 0;
    }
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// 3. Excluded apps are never captured — frame-level (doc 13 §4, doc 05 §4)
// ---------------------------------------------------------------------------

/// The gate the milestone actually names: an excluded app must never reach the
/// frame pull. `Normalized::capture_frame == false` IS that gate — the sampler
/// pulls a frame only when it is true, so proving it false proves no pixels are
/// ever read for this context. The published event must also be metadata-only.
#[test]
fn m9_excluded_app_never_reaches_a_frame_pull() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let list = ExclusionList::compile(vec![ExclusionRule {
        process: Some("1password.exe".into()),
        label: "1Password".into(),
        ..Default::default()
    }]);
    let n = Normalizer::new(bus, list);

    let out = n.normalize_hook(
        &HookEvent::ForegroundChanged { hwnd: 42 },
        WindowIdentity {
            app: Some("1Password".into()),
            process: Some("1password.exe".into()),
            window_title: Some("Personal vault — 1Password".into()),
            window_class: None,
        },
        1_000,
    );

    assert_eq!(out.len(), 1);
    assert!(!out[0].capture_frame, "M9: an excluded app must NEVER be framed (doc 05 §4)");

    let ev = rx.try_recv().expect("a metadata-only event is still published");
    assert_ne!(ev.redaction_flags & redaction_flags::EXCLUDED, 0, "flagged EXCLUDED");
    assert_eq!(ev.window_title, None, "the title is stripped — it can leak content");
}

/// Private/incognito windows are excluded even with ZERO configured rules —
/// the one case where protection is not opt-in (doc 13 §4).
#[test]
fn m9_private_windows_are_excluded_with_no_rules_configured() {
    let bus = EventBus::new();
    let n = Normalizer::new(bus, ExclusionList::shipped_defaults());
    let out = n.normalize_hook(
        &HookEvent::ForegroundChanged { hwnd: 7 },
        WindowIdentity {
            app: Some("Chrome".into()),
            process: Some("chrome.exe".into()),
            window_title: Some("something private - Incognito".into()),
            window_class: None,
        },
        2_000,
    );
    assert!(!out[0].capture_frame, "incognito is never framed (doc 13 §4)");
}

/// ADR-029/Q15 held: nothing ships excluded by default, and detect-and-suggest
/// only ever produces candidates — it must never auto-apply.
#[test]
fn m9_defaults_ship_empty_and_suggestions_are_not_applied() {
    assert!(ExclusionList::shipped_defaults().is_empty(), "ADR-029/Q15: empty defaults");

    let suggestions =
        aperture_privacy::detect_suggest::suggest_from(&["1password.exe".into()], &[]);
    assert_eq!(suggestions.len(), 1, "an installed password manager IS suggested");

    // The suggestion changes nothing until the user confirms it.
    let db = Db::open_in_memory().expect("open");
    assert!(
        db.read_exclusion_list().unwrap().is_empty(),
        "producing suggestions must not write any rule (ADR-029: never auto-excluded)"
    );
}

// ---------------------------------------------------------------------------
// 4. The audit answers "what left the machine" (doc 13 §3)
// ---------------------------------------------------------------------------

#[test]
fn m9_audit_answers_when_it_watched_and_what_left_the_machine() {
    let db = Arc::new(Db::open_in_memory().expect("open"));
    let audit = AuditLog::new(Arc::clone(&db));

    audit
        .record_capture_toggle(CaptureToggleRecord {
            enabled: true,
            reason: ToggleReason::Consent,
            ts: 1_000,
        })
        .unwrap();
    let payload_id = uuid::Uuid::new_v4();
    let wire = b"the exact bytes that egressed";
    audit
        .record_cloud_send(CloudSendRecord {
            payload_id,
            wire_sha256: aperture_privacy::audit_log::sha256_hex(wire),
            transport: TransportTarget::MessagesApi,
            byte_count: wire.len() as u64,
            ts: 2_000,
        })
        .unwrap();
    audit
        .record_capture_toggle(CaptureToggleRecord {
            enabled: false,
            reason: ToggleReason::UserAction,
            ts: 3_000,
        })
        .unwrap();

    let rows = audit.recent(50).expect("read the trail");
    assert_eq!(rows.len(), 3, "every toggle and every send is on the trail");

    // "When was it watching?" — an ON and an OFF, in order, with reasons.
    let toggles: Vec<_> = rows.iter().filter(|r| r.r#type == EventType::CaptureToggle).collect();
    assert_eq!(toggles.len(), 2);
    assert_eq!(toggles[0].payload["enabled"], serde_json::json!(false));
    assert_eq!(toggles[1].payload["reason"], serde_json::json!("consent"));

    // "What ever left this machine?" — the hash of the EXACT bytes, the
    // transport that carried them, and how many.
    let send = rows.iter().find(|r| r.r#type == EventType::CloudSend).expect("a cloud_send row");
    assert_eq!(send.payload["payload_id"], serde_json::json!(payload_id.to_string()));
    assert_eq!(send.payload["byte_count"], serde_json::json!(wire.len()));
    assert_eq!(send.payload["transport"], serde_json::json!("messages-api"));
    assert_eq!(
        send.payload["wire_sha256"],
        serde_json::json!(aperture_privacy::audit_log::sha256_hex(wire)),
        "the recorded hash must be over the bytes that actually egressed"
    );
}

/// Consent is the gate in front of all of it, and it fails closed (doc 13 §8).
#[test]
fn m9_consent_defaults_to_capture_off_and_audits_the_opt_in() {
    let db = Arc::new(Db::open_in_memory().expect("open"));
    let mut consent = ConsentManager::load(Arc::clone(&db)).expect("load");
    assert!(!consent.state().capture_allowed(), "capture is OFF before consent (doc 13 §8)");
    assert!(!consent.state().first_run_completed);

    consent.complete_first_run(true, 4_000).expect("opt in");
    assert!(consent.state().capture_allowed());

    // The opt-in itself is on the audit trail — "when did it start watching?"
    let rows = AuditLog::new(db).recent(10).expect("trail");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payload["reason"], serde_json::json!("consent"));
    assert_eq!(rows[0].ts, 4_000);
}
