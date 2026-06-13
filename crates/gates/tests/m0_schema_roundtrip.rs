//! M0 gate — schema round-trips every event type; fakes compile (doc 16 M0).
//!
//! Doc 16 M0 gate condition: *"schema round-trips all event types; fakes compile
//! against every contract."* This test is the executable form of that condition
//! and runs on every CI build (it needs neither a GPU nor a network, so unlike
//! SC5-strict/SC6 it is **not** `#[ignore]`-gated).
//!
//! What it proves:
//!   1. The authoritative DDL (`aperture_db::migrations::MIGRATIONS`,
//!      `crates/db/migrations/0001_init.sql`, doc 03 §3) applies cleanly.
//!   2. For **every** [`EventType::ALL`] variant, an [`Event`] inserts into the
//!      `events` table and reads back byte-for-byte identical — the `type` column
//!      and the JSON `payload` survive the storage round-trip (doc 15 §1: "SQLite
//!      is the durable form ... the DB is the truth").
//!   3. The `EventType` snake_case wire string the DB stores is the same string
//!      `serde` produces — the column and the bus message agree (event.rs §).
//!   4. The contracts test fakes (doc 15 §7), behind the `fakes` feature, compile
//!      and instantiate against every contract.
//!
//! NOTE: this test runs at the *schema* level on an in-memory rusqlite connection
//! rather than through `aperture_db::Db`, because the typed `insert_event` /
//! `read_event` path on `Db` is a later milestone (`todo!("M0…")` today). The DDL
//! it exercises is the same embedded SQL `Db::open_encrypted` will run, so the
//! gate stays honest now and tightens (swap the raw SQL for the `Db` API) without
//! changing what it asserts.

use aperture_contracts::event::{redaction_flags, Event, EventType};
use rusqlite::Connection;

/// Apply the authoritative embedded migrations to a fresh in-memory DB.
///
/// sqlite-vec is *not* loaded here (the `ctx_vec` virtual table needs the
/// extension, doc 03 §3 / doc 13 §6); M0 only exercises the plain tables. The DDL
/// is applied statement-by-statement so the single `CREATE VIRTUAL TABLE ... USING
/// vec0(...)` statement — which would fail without the extension — can be skipped
/// without aborting the rest of the batch. Every other statement MUST succeed
/// (a failure there is a real schema regression the gate should catch). The first
/// migration also sets `PRAGMA journal_mode=WAL`, harmless on an in-memory conn.
fn open_scratch_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    for (version, sql) in aperture_db::migrations::MIGRATIONS {
        for stmt in split_sql_statements(sql) {
            let is_vec0 = stmt.contains("USING vec0") || stmt.contains("using vec0");
            match conn.execute_batch(&format!("{stmt};")) {
                Ok(()) => {}
                // TODO(M0:): load the sqlite-vec extension before this loop so the
                // `ctx_vec` (vec0) table is exercised too; until then it is the one
                // expected, tolerated failure (it is unused by the event round-trip).
                Err(_) if is_vec0 => {
                    eprintln!("note: skipping vec0 table in v{version} (sqlite-vec not loaded)");
                }
                Err(e) => panic!("apply migration v{version} statement `{stmt}`: {e}"),
            }
        }
    }
    conn
}

/// Naive `;`-splitter for the embedded DDL. The init schema (doc 03 §3) is plain
/// statements with no `;` inside string literals or triggers, so a literal split
/// is correct here. (If a future migration adds a trigger body with embedded `;`,
/// switch to loading sqlite-vec and using `execute_batch` on the whole file.)
fn split_sql_statements(sql: &str) -> impl Iterator<Item = String> + '_ {
    sql.split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

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
    let conn = open_scratch_db();

    // Guard: keep this gate honest if a variant is ever added without ALL being
    // updated (doc 16 M0 demands *every* type round-trips).
    assert_eq!(
        EventType::ALL.len(),
        13,
        "EventType::ALL drifted from the taxonomy in event.rs; update the gate"
    );

    for ty in EventType::ALL {
        let ev = sample_event(ty);

        // Serialize the enum exactly as the bus/DB would (snake_case wire string).
        let type_str = serde_json::to_value(ev.r#type)
            .expect("EventType serializes")
            .as_str()
            .expect("EventType serializes to a JSON string")
            .to_string();
        let payload_str = serde_json::to_string(&ev.payload).expect("payload serializes");

        // INSERT — `id` is left to AUTOINCREMENT, mirroring the doc 15 §1 rule that
        // the DB assigns `id` on insert.
        conn.execute(
            "INSERT INTO events \
             (ts, type, app, process, window_title, payload, connector_id, session_id, redaction_flags) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                ev.ts,
                type_str,
                ev.app,
                ev.process,
                ev.window_title,
                payload_str,
                ev.connector_id,
                ev.session_id,
                ev.redaction_flags,
            ],
        )
        .unwrap_or_else(|e| panic!("insert {ty:?}: {e}"));
        let row_id = conn.last_insert_rowid();

        // READ BACK and reconstruct the typed Event.
        let got: Event = conn
            .query_row(
                "SELECT id, ts, type, app, process, window_title, payload, \
                        connector_id, session_id, redaction_flags \
                 FROM events WHERE id = ?1",
                [row_id],
                |row| {
                    let type_text: String = row.get(2)?;
                    let payload_text: String = row.get(6)?;
                    Ok(Event {
                        id: row.get(0)?,
                        ts: row.get(1)?,
                        // Re-parse the stored wire string back into the enum — this
                        // is the half of the round-trip that proves the column and
                        // the `serde` representation agree.
                        r#type: serde_json::from_value(serde_json::Value::String(type_text))
                            .expect("stored type string parses back to EventType"),
                        app: row.get(3)?,
                        process: row.get(4)?,
                        window_title: row.get(5)?,
                        payload: serde_json::from_str(&payload_text)
                            .expect("stored payload parses back to JSON"),
                        connector_id: row.get(7)?,
                        session_id: row.get(8)?,
                        redaction_flags: row.get(9)?,
                    })
                },
            )
            .unwrap_or_else(|e| panic!("read back {ty:?}: {e}"));

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
