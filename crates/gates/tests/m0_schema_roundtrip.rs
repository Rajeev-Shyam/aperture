//! M0 gate — schema round-trips every event type; fakes compile (doc 16 M0).
//!
//! Doc 16 M0 gate condition: *"schema round-trips all event types; fakes compile
//! against every contract."* This test is the executable form of that condition
//! and runs on every CI build (it needs neither a GPU nor a network, so unlike
//! SC5-strict/SC6 it is **not** `#[ignore]`-gated).
//!
//! What it proves:
//!   1. The authoritative DDL (`aperture_db::migrations::MIGRATIONS`,
//!      `crates/db/migrations/0001_init.sql`, doc 03 §3) applies cleanly —
//!      through the REAL `aperture_db::Db` open path (Step 0 tightened this from
//!      the original raw-SQL harness, per its own TODO), including the
//!      sqlite-vec `ctx_vec` virtual table.
//!   2. For **every** [`EventType::ALL`] variant, an [`Event`] inserts via the
//!      typed `Db::insert_event` and reads back identical via `Db::read_event` —
//!      the `type` column and the JSON `payload` survive the storage round-trip
//!      (doc 15 §1: "SQLite is the durable form ... the DB is the truth").
//!   3. The `EventType` snake_case wire string the DB stores is the same string
//!      `serde` produces — the column and the bus message agree (event.rs §).
//!   4. The contracts test fakes (doc 15 §7), behind the `fakes` feature, compile
//!      and instantiate against every contract.

use aperture_contracts::event::{redaction_flags, Event, EventType};

/// Build a representative [`Event`] for a given type, with a type-tagged JSON
/// payload so the round-trip also proves the `serde_json::Value` payload survives.
fn sample_event(ty: EventType) -> Event {
    Event {
        id: 0, // assigned by the DB on insert (event.rs §)
        ts: 1_700_000_000_000,
        r#type: ty,
        app: Some("Aperture.Test".to_string()),
        process: Some("aperture-gates.exe".to_string()),
        window_title: Some(format!("round-trip: {ty:?}")),
        payload: serde_json::json!({ "type_under_test": format!("{ty:?}"), "k": 1 }),
        connector_id: None,
        session_id: Some(42),
        redaction_flags: redaction_flags::EXCLUDED, // exercise a non-zero flag column
    }
}

#[test]
fn m0_every_event_type_round_trips_through_the_schema() {
    let db = aperture_db::Db::open_in_memory().expect("open + migrate in-memory DB");

    // Guard: keep this gate honest if a variant is ever added without ALL being
    // updated (doc 16 M0 demands *every* type round-trips).
    assert_eq!(
        EventType::ALL.len(),
        13,
        "EventType::ALL drifted from the taxonomy in event.rs; update the gate"
    );

    for ty in EventType::ALL {
        let ev = sample_event(ty);

        // The wire string the DB stores must be the exact serde snake_case form
        // (proved indirectly by read_event parsing it back; assert it directly too).
        let type_str = aperture_db::event_type_str(ty);
        assert_eq!(
            serde_json::to_value(ty).unwrap().as_str().unwrap(),
            type_str,
            "{ty:?}: DB wire string == serde wire string"
        );

        let id = db.insert_event(&ev).unwrap_or_else(|e| panic!("insert {ty:?}: {e}"));
        let got = db.read_event(id).unwrap_or_else(|e| panic!("read back {ty:?}: {e}"));

        // Round-trip assertions: everything but the DB-assigned `id` is identical.
        assert_eq!(got.ts, ev.ts, "{ty:?}: ts");
        assert_eq!(got.r#type, ev.r#type, "{ty:?}: type enum survived");
        assert_eq!(got.app, ev.app, "{ty:?}: app");
        assert_eq!(got.process, ev.process, "{ty:?}: process");
        assert_eq!(got.window_title, ev.window_title, "{ty:?}: window_title");
        assert_eq!(got.payload, ev.payload, "{ty:?}: payload JSON survived");
        assert_eq!(got.connector_id, ev.connector_id, "{ty:?}: connector_id");
        assert_eq!(got.session_id, ev.session_id, "{ty:?}: session_id");
        assert_eq!(
            got.redaction_flags, ev.redaction_flags,
            "{ty:?}: redaction_flags"
        );
        assert!(got.id > 0, "{ty:?}: DB assigned a positive id");
    }
}

/// The `ctx_vec` virtual table (sqlite-vec) must actually load and accept a
/// 768-d write + KNN query — doc 03 §3 pins the dimension; the M2 gate builds on
/// this. Kept in the M0 gate so a broken sqlite-vec build fails fast.
#[test]
fn m0_sqlite_vec_loads_and_ctx_vec_round_trips() {
    let db = aperture_db::Db::open_in_memory().expect("open");
    assert!(
        db.vec_available(),
        "sqlite-vec failed to register — ctx_vec (doc 03 §3) is unavailable"
    );

    let mut vec = vec![0.0f32; 768];
    vec[7] = 1.0;
    let id = db
        .insert_event_with_context(&sample_event(EventType::WindowFocus), None, Some(&vec))
        .expect("atomic event + embedding insert");

    let hits = db.knn(&vec, 1, 0).expect("knn query");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, id, "nearest neighbour of a vector is itself");
}

/// The contracts fakes (doc 15 §7) must compile and instantiate against every
/// contract — the second half of the M0 gate condition. This is mostly a
/// compile-time assertion; constructing each fake is the runtime proof.
#[test]
fn m0_contracts_fakes_compile_and_instantiate() {
    use aperture_contracts::fakes::{
        FakeConnector, FakeScheduler, FakeTransport, ScriptedEventPlayer,
    };
    use aperture_contracts::{Connector, Health, OpenOutcome, StructuredSuggestions};
    use std::time::Duration;

    // Event-envelope fake: a scripted player drained to exhaustion.
    let script = EventType::ALL.iter().map(|&ty| sample_event(ty)).collect();
    let mut player = ScriptedEventPlayer::new(script);
    let mut drained = 0;
    while player.next().is_some() {
        drained += 1;
    }
    assert_eq!(drained, EventType::ALL.len(), "scripted player replays all");

    // Connector fake — exercise the trait object form (the seam G3 uses).
    let fake_conn = FakeConnector {
        id: "fake",
        capture_result: None,
        open_result: OpenOutcome::Resumed,
    };
    let conn_obj: &dyn Connector = &fake_conn;
    assert_eq!(conn_obj.id(), "fake");
    assert!(conn_obj.staleness_ttl() > Duration::ZERO);

    // GPU-job fake — a refusal scheduler (drives the doc 04 R3 degrade ladder).
    let _sched = FakeScheduler {
        latency: Duration::from_millis(0),
        refuse_with_projection_gb: Some(7.9),
        canned: None,
    };

    // Gateway fake — canned transport, no network (doc 15 §7). An empty
    // StructuredSuggestions is a valid "the model returned nothing actionable"
    // result; both fields are `#[serde(default)]` so this is the minimal shape.
    let _transport = FakeTransport {
        health: Health::Ready,
        canned: Ok(StructuredSuggestions {
            suggestions: Vec::new(),
            answer_text: None,
        }),
    };

    // TODO(M7:): once `fakes::golden::redaction_fixture()` is implemented (it is a
    // `todo!()` today), assert here that its `redactions` list matches the SC5
    // preview/wire fixture — this is the shared fixture sc5_network_monitor.rs uses.
}
