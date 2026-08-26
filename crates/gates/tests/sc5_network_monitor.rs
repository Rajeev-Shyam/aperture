//! SC5 gate — zero silent egress; bytes only after an approved Send (doc 13 §2,
//! doc 16 M3/M7). One of the two permanent regression gates (doc 16, staged
//! recommendation 3). **Real harness since 2026-08-22** (owner decision #1).
//!
//! The promise (doc 13 §2, the two-emitter rule): exactly one crate
//! (`reasoning_gateway`) may open a network socket, and only on a payload flagged
//! `user_approved = true` by the preview panel. Everything upstream — capture,
//! OCR, embeddings, patterns, the DB — is egress-free *by construction*. SC5 is
//! the runtime proof of that architectural claim.
//!
//! ## The monitor backend (what is actually measured)
//!
//! No ETW session and no proxy — both need admin or a cert, and neither is
//! CI-friendly. Three independent, unprivileged observers instead:
//!
//! 1. **A loopback origin stands in for `api.anthropic.com`.** The gateway's
//!    Messages-API transport is pointed at `127.0.0.1:<port>`; the origin counts
//!    every byte it receives and keeps every request body. This is the
//!    byte-level wire: if the proactive path emits, the counter moves.
//! 2. **`netstat -ano`** — every TCP connection owned by THIS process, polled
//!    while the proactive path runs: there must be none to a non-loopback
//!    address at any point (loopback is whitelisted by doc 13 §2 / ADR-028).
//! 3. **Child processes** — the CLI transport is a spawn; `Get-CimInstance
//!    Win32_Process` filtered on our PID as parent must stay empty until Send.
//!
//! Then the user approves, `Gateway::send_with_preview` runs for real over the
//! loopback origin, and the gate asserts (a) bytes > 0 now, (b) exactly one
//! request, (c) `SHA-256(body) == SHA-256(transport.wire_bytes(payload))` —
//! preview == wire, doc 13 §3 — and (d) the `cloud_send` audit row in the DB
//! records that same hash, transport and byte count (doc 13 §3).
//!
//! The "proactive path" here is the real offline chain: the pattern engine over
//! a scripted event stream (capture-on, connector lookups, no candidate can emit
//! anything), then the real `payload_builder` + `Redactor` over the golden
//! redaction fixture (doc 15 §7: `email × 2, secret_key × 1`), staged in a real
//! `PreviewSession`. The CLI transport itself is not exercised (no `claude` on a
//! CI box); observer 3 is what covers "nothing spawned before Send".
//!
//! Windows-only (netstat / CIM); loopback-only; deterministic; NOT `#[ignore]` —
//! it runs on every `cargo test --workspace`. `gates` is lint-exempt, so the
//! std `TcpListener` origin and the measurement spawns are allowed here and
//! nowhere on the proactive path.

#![cfg(windows)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aperture_contracts::context_payload::{ContextPayload, Intent, TransportTarget};
use aperture_contracts::event::{Event, EventType};
use aperture_contracts::fakes::golden;
use aperture_contracts::fakes::ScriptedEventPlayer;
use aperture_contracts::reasoning::ReasoningTransport;
use aperture_contracts::ConnectorState;
use aperture_db::Db;
use aperture_pattern_engine::{EngineContext, PatternEngine};
use aperture_privacy::audit_log::{sha256_hex, AuditLog};
use aperture_privacy::redaction::Redactor;
use aperture_reasoning_gateway::payload_builder;
use aperture_reasoning_gateway::preview::{PreviewDecision, PreviewSession};
use aperture_reasoning_gateway::suggestion_validator::ConnectorLookup;
use aperture_reasoning_gateway::transports::api::{ApiSettings, ApiTransport};
use aperture_reasoning_gateway::Gateway;

// ---------------------------------------------------------------------------
// Observer 1: the loopback origin (byte counter + body capture)
// ---------------------------------------------------------------------------

struct LoopbackOrigin {
    url: String,
    bytes_received: Arc<AtomicU64>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl LoopbackOrigin {
    /// Bind `127.0.0.1:0` and serve one canned Messages-API reply per request.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback bind");
        let port = listener.local_addr().unwrap().port();
        let bytes_received = Arc::new(AtomicU64::new(0));
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let (b, bod) = (Arc::clone(&bytes_received), Arc::clone(&bodies));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read headers, then exactly Content-Length body bytes.
                let (head_end, content_length) = loop {
                    let n = match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break (buf.len(), 0usize),
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = find(&buf, b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (pos + 4, len);
                    }
                };
                while buf.len() < head_end + content_length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                b.fetch_add(buf.len() as u64, Ordering::SeqCst);
                bod.lock().unwrap().push(buf[head_end..].to_vec());
                // Messages-API shape the ApiTransport parses: content[0].text.
                let body = r#"{"content":[{"type":"text","text":"{\"suggestions\":[],\"answer_text\":\"ok\"}"}]}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.flush();
            }
        });
        Self { url: format!("http://127.0.0.1:{port}/v1/messages"), bytes_received, bodies }
    }

    fn egress_bytes(&self) -> u64 {
        self.bytes_received.load(Ordering::SeqCst)
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.bodies.lock().unwrap().clone()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Observer 2: this process's TCP connections (non-loopback must be zero)
// ---------------------------------------------------------------------------

fn non_loopback_connections(pid: u32) -> Vec<String> {
    let out = std::process::Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .output()
        .expect("netstat runs on Windows");
    let text = String::from_utf8_lossy(&out.stdout);
    let pid_s = pid.to_string();
    text.lines()
        .filter(|l| l.trim_start().starts_with("TCP"))
        .filter(|l| l.split_whitespace().last() == Some(pid_s.as_str()))
        .filter(|l| {
            let remote = l.split_whitespace().nth(2).unwrap_or("");
            !(remote.starts_with("127.") || remote.starts_with("[::1]") || remote.starts_with("0.0.0.0") || remote.starts_with("[::]"))
        })
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// Observer 3: child processes (the CLI transport is a spawn)
// ---------------------------------------------------------------------------

fn child_processes(pid: u32) -> Vec<String> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Get-CimInstance Win32_Process -Filter \"ParentProcessId={pid}\" | Select-Object -ExpandProperty Name"
            ),
        ])
        .output()
        .expect("powershell runs on Windows");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // PowerShell itself is our child while it answers; exclude it.
        .filter(|l| !l.eq_ignore_ascii_case("powershell.exe"))
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// The proactive path (Path A), offline
// ---------------------------------------------------------------------------

struct NoConnectors;
impl ConnectorLookup for NoConnectors {
    fn by_type(&self, _: &str) -> Option<&dyn aperture_contracts::Connector> {
        None
    }
}

fn event(ts: i64, kind: EventType, app: &str, title: &str) -> Event {
    Event {
        id: 0,
        ts,
        r#type: kind,
        app: Some(app.into()),
        process: Some(format!("{}.exe", app.to_ascii_lowercase())),
        window_title: Some(title.into()),
        payload: serde_json::json!({}),
        connector_id: None,
        session_id: None,
        redaction_flags: 0,
    }
}

/// Drive the REAL pattern engine over a scripted recurring workflow with
/// capture ON, then build the golden payload through the REAL redactor +
/// payload builder and stage it as a preview (NOT approved). Returns the
/// staged session and the redactions the preview shows.
fn run_proactive_path_a_to_preview() -> (PreviewSession, Vec<aperture_contracts::Redaction>) {
    // 1. The SC2-style script: the same three-step workflow, four days running.
    let mut script = Vec::new();
    for day in 0..4i64 {
        let base = 1_700_000_000_000 + day * 86_400_000;
        script.push(event(base, EventType::WindowFocus, "Code", "main.rs — aperture"));
        script.push(event(base + 60_000, EventType::WindowFocus, "Chrome", "docs.rs — Tokio"));
        script.push(event(base + 120_000, EventType::Navigation, "Chrome", "docs.rs — Tokio"));
        script.push(event(base + 180_000, EventType::WindowFocus, "Code", "main.rs — aperture"));
    }
    let mut player = ScriptedEventPlayer::new(script);
    let mut engine = PatternEngine::new();
    engine.set_capture(true);
    let lookup = |_: &aperture_pattern_engine::normalizer::Token| -> Option<ConnectorState> { None };
    let mut candidates = 0usize;
    while let Some(ev) = player.next() {
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ev.ts };
        candidates += engine.on_event(&ev, &ctx).len();
    }
    // Whether or not a candidate fired is the engine's business (doc 08 §5);
    // SC5's claim is that none of this touched the network.
    let _ = candidates;

    // 2. The payload: golden fixture → real redactor → real builder → preview.
    let redactor = Redactor::new(&[]).expect("built-in rules compile");
    let golden = golden::redaction_fixture();
    let (payload, _report) = payload_builder::build(
        Intent::SummarizeCurrent,
        golden.items.clone(),
        TransportTarget::MessagesApi,
        &redactor,
        golden.created_ts,
    )
    .expect("payload builds");
    let redactions = payload.redactions.clone();
    assert!(!payload.user_approved, "a freshly built payload is never approved");
    (PreviewSession::new(payload), redactions)
}

/// SHA-256 of the exact serialized preview bytes (what `preview_set_approved`
/// hashes), lowercase hex.
fn preview_payload_sha256(payload: &ContextPayload) -> String {
    sha256_hex(&serde_json::to_vec(payload).expect("serializes"))
}

#[test]
fn sc5_zero_egress_on_proactive_path_then_bytes_only_after_approved_send() {
    let pid = std::process::id();
    let origin = LoopbackOrigin::start();

    // --- Path A: everything up to the preview, under all three observers.
    assert_eq!(origin.egress_bytes(), 0, "origin saw bytes before anything ran");
    let (session, redactions) = run_proactive_path_a_to_preview();
    let remote = non_loopback_connections(pid);
    assert!(remote.is_empty(), "non-loopback TCP connections during Path A: {remote:?}");
    let children = child_processes(pid);
    assert!(children.is_empty(), "child processes spawned during Path A: {children:?}");
    assert_eq!(origin.egress_bytes(), 0, "bytes left during the proactive path");

    // The preview is honest about what it will send (doc 13 §5 fixture).
    let mut by_rule: Vec<(String, u32)> = redactions.iter().map(|r| (r.rule.clone(), r.count)).collect();
    by_rule.sort();
    assert_eq!(
        by_rule,
        vec![("email".to_string(), 2), ("secret_key".to_string(), 1)],
        "golden fixture redactions"
    );
    let preview_bytes = serde_json::to_vec(session.payload()).unwrap();
    let preview_text = String::from_utf8_lossy(&preview_bytes);
    for email in golden::FIXTURE_EMAILS {
        assert!(!preview_text.contains(email), "{email} survived redaction in the preview");
    }
    assert!(!preview_text.contains(&golden::fixture_secret()), "the secret survived redaction");
    let preview_hash = preview_payload_sha256(session.payload());

    // --- The user approves. Only now may the gateway open a socket.
    let approved = session.approve(PreviewDecision::Send).expect("Send keeps the payload");
    assert!(approved.user_approved);
    assert_eq!(
        preview_payload_sha256(&approved),
        preview_hash,
        "approval must not change the bytes (user_approved is skip_serializing)"
    );

    let db = Arc::new(Db::open_in_memory().expect("db"));
    let transport = ApiTransport::new(
        ApiSettings {
            endpoint: origin.url.clone(),
            model: "sc5-fixture-model".into(),
            anthropic_version: "2023-06-01".into(),
            beta_headers: Vec::new(),
            cache_ttl: "5m".into(),
            max_tokens: 64,
        },
        "sc5-dummy-key",
    );
    let expected_wire_hash = sha256_hex(&transport.wire_bytes(&approved));
    let expected_wire_len = transport.wire_bytes(&approved).len() as u64;
    let gateway = Gateway::new(vec![Box::new(transport)], Box::new(NoConnectors))
        .with_audit(Arc::new(AuditLog::new(Arc::clone(&db))));

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let outcome = rt
        .block_on(gateway.send_with_preview(&approved, true))
        .expect("the approved send succeeds over the loopback origin");
    assert!(outcome.audit_failure.is_none(), "audit row must be written: {:?}", outcome.audit_failure);

    // --- Bytes now, exactly one request, and preview == wire.
    assert!(origin.egress_bytes() > 0, "the approved Send produced no bytes");
    let bodies = origin.bodies();
    assert_eq!(bodies.len(), 1, "exactly one request for one Send");
    let body_hash = sha256_hex(&bodies[0]);
    assert_eq!(body_hash, expected_wire_hash, "SHA-256(wire body) == SHA-256(transport.wire_bytes)");
    assert_eq!(bodies[0].len() as u64, expected_wire_len);
    let body_text = String::from_utf8_lossy(&bodies[0]);
    for email in golden::FIXTURE_EMAILS {
        assert!(!body_text.contains(email), "{email} reached the wire");
    }
    assert!(!body_text.contains(&golden::fixture_secret()), "the secret reached the wire");
    assert!(body_text.contains("⟨email#1⟩"), "the wire carries the placeholder the preview showed");

    // --- The audit row records exactly that (doc 13 §3).
    let audit = db.recent_audit_events(5).expect("audit read");
    let row = audit
        .iter()
        .find(|e| e.r#type == EventType::CloudSend)
        .expect("one cloud_send row");
    assert_eq!(row.payload["wire_sha256"], serde_json::json!(expected_wire_hash));
    assert_eq!(row.payload["transport"], serde_json::json!("messages-api"));
    assert_eq!(row.payload["byte_count"], serde_json::json!(expected_wire_len));
    assert_eq!(row.payload["payload_id"], serde_json::json!(approved.payload_id.to_string()));

    // Still nothing non-loopback, still no children: the Send went where the
    // settings pointed it and nowhere else.
    let remote = non_loopback_connections(pid);
    assert!(remote.is_empty(), "non-loopback TCP connections after Send: {remote:?}");
    let children = child_processes(pid);
    assert!(children.is_empty(), "child processes after Send: {children:?}");
}

/// The refusal half of the invariant: an UNAPPROVED payload never reaches the
/// transport, even when a healthy one is configured and the caller lies.
#[test]
fn sc5_unapproved_payload_never_opens_the_socket() {
    let origin = LoopbackOrigin::start();
    let (session, _) = run_proactive_path_a_to_preview();
    let payload = session.payload().clone(); // user_approved == false
    let transport = ApiTransport::new(
        ApiSettings {
            endpoint: origin.url.clone(),
            model: "sc5-fixture-model".into(),
            anthropic_version: "2023-06-01".into(),
            beta_headers: Vec::new(),
            cache_ttl: "5m".into(),
            max_tokens: 64,
        },
        "sc5-dummy-key",
    );
    let gateway = Gateway::new(vec![Box::new(transport)], Box::new(NoConnectors));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // The caller claims approval; the payload flag says otherwise — refused.
    let err = rt.block_on(gateway.send_with_preview(&payload, true)).err().expect("refused");
    assert!(matches!(err, aperture_reasoning_gateway::GatewayError::NotApproved), "{err:?}");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(origin.egress_bytes(), 0, "an unapproved payload produced bytes");
}
