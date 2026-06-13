//! SC5 gate — zero silent egress; bytes only after an approved Send (doc 13 §2,
//! doc 16 M3/M7). One of the two permanent regression gates (doc 16, staged
//! recommendation 3).
//!
//! The promise (doc 13 §2, the two-emitter rule): exactly one crate
//! (`reasoning_gateway`) may open a network socket, and only on a payload flagged
//! `user_approved = true` by the preview panel. Everything upstream — capture,
//! OCR, embeddings, patterns, the DB — is egress-free *by construction*. SC5 is
//! the runtime proof of that architectural claim.
//!
//! Three things this gate asserts (doc 16 M7 "SC5 strict"):
//!   1. A full proactive Path-A run (capture → pattern → suggestion bubble),
//!      driven entirely by the contracts fakes, produces **zero** network bytes.
//!   2. Bytes appear on the wire **only** after `preview_send` is invoked on a
//!      payload the user approved (`ContextPayload::user_approved == true`).
//!   3. The bytes the preview rendered hash-equal the bytes that hit the wire —
//!      `SHA-256(preview) == SHA-256(wire)` (doc 13 §3, "preview == wire" as a
//!      data-flow property), which is also what the `cloud_send` audit row records.
//!
//! This is `#[ignore]` until the monitor backend is chosen and wired (the work
//! lands at M7; the harness exists from M1 so the gate can run as soon as a
//! proactive path does). The assertions below are the contract the backend must
//! satisfy; the measurement mechanism is the only `todo!()`.
//!
//! Path A = the proactive path (Aperture initiates a suggestion). Contrast Path B
//! = the reactive resume-click path; neither is allowed to emit before Send.

// TODO(M7:): pick and wire the network-monitor backend. Two candidates (doc 01
//   SC5 row, doc 04 §): an in-process socket/loopback counter via ETW
//   (`Microsoft-Windows-Kernel-Network` provider, no admin, CI-friendly) OR an
//   out-of-process mitmproxy/pcap tap (truest, needs a proxy/cert in CI). ETW is
//   the likely CI default; mitmproxy is the belt-and-suspenders manual gate. Until
//   then `NetworkMonitor` is a stub and every assertion routes through `todo!()`.

use aperture_contracts::context_payload::ContextPayload;

/// Counts bytes that leave the machine during a window of execution. The real
/// implementation taps ETW or a proxy (see the module TODO); here it is a stub so
/// the gate's *shape* is reviewable and the contract is pinned.
struct NetworkMonitor;

impl NetworkMonitor {
    /// Begin counting from zero. Must capture **all** egress for this process tree
    /// (the gateway may spawn the Claude CLI — a child process — so a per-process
    /// counter is insufficient; the monitor is process-tree- or host-scoped).
    fn start() -> Self {
        // TODO(M7:): open the ETW session / attach the proxy tap.
        todo!("M7: start the SC5 network monitor (ETW session or proxy tap)")
    }

    /// Total bytes observed leaving the machine since `start()`.
    fn egress_bytes(&self) -> u64 {
        todo!("M7: read the egress byte counter from the monitor backend")
    }

    /// SHA-256 of the exact bytes observed on the wire (for the preview==wire
    /// hash compare). `None` if nothing has been sent yet.
    fn wire_payload_sha256(&self) -> Option<[u8; 32]> {
        todo!("M7: capture + hash the transmitted body from the monitor backend")
    }
}

/// Drive a full proactive Path-A run over the contracts fakes and return the
/// payload the preview panel produced (NOT yet approved — `user_approved=false`).
///
/// Uses `ScriptedEventPlayer` → (fake pattern/suggestion path) → payload builder
/// → preview. No `FakeTransport::send` is called here: a proactive run *stops at
/// the preview*; nothing should touch the network until the user clicks Send.
fn run_proactive_path_a_to_preview() -> ContextPayload {
    // TODO(M7:): assemble the offline proactive path from the fakes:
    //   1. ScriptedEventPlayer replays a recurring workflow (the SC2 script).
    //   2. the pattern engine + suggestion generator raise a candidate.
    //   3. the payload builder assembles ONE ContextPayload (= the golden
    //      redaction_fixture, doc 15 §7) and the preview renders it.
    //   4. return it with user_approved == false.
    todo!("M7: wire the offline proactive path (fakes) up to the preview panel")
}

/// SHA-256 of the exact serialized bytes the preview rendered — the same bytes
/// `preview_send` will transmit (doc 13 §3). This is computed locally, before any
/// egress, so the post-send wire hash can be compared against it.
fn preview_payload_sha256(_payload: &ContextPayload) -> [u8; 32] {
    // TODO(M7:): serialize `payload` exactly as the gateway will (the `v1` wire
    // form, `user_approved` skipped per context_payload.rs §) and SHA-256 it.
    // The hash is also what the `cloud_send` audit row stores (doc 13 §7).
    todo!("M7: serialize the payload to its v1 wire bytes and SHA-256 them")
}

/// Invoke the one sanctioned egress trigger: the gateway sending an **approved**
/// payload. Returns the structured suggestions (ignored by this gate). Calling
/// this is the ONLY thing in the whole test that is permitted to move bytes.
fn preview_send(_approved: &ContextPayload) {
    // TODO(M7:): call into aperture_reasoning_gateway with `user_approved == true`.
    // Precondition the gateway itself enforces: it refuses any payload whose
    // `user_approved` is false (doc 15 §2 law (c)).
    todo!("M7: route the approved payload through aperture_reasoning_gateway::send")
}

#[test]
#[ignore = "SC5 strict: requires the network-monitor backend (ETW/mitmproxy) — wired at M7 (doc 16)"]
fn sc5_zero_egress_on_proactive_path_then_bytes_only_after_approved_send() {
    let monitor = NetworkMonitor::start();

    // (1) Full proactive Path-A run → preview. Nothing approved, nothing sent.
    let payload = run_proactive_path_a_to_preview();
    assert!(
        !payload.user_approved,
        "a proactive run must reach the preview with user_approved == false"
    );
    assert_eq!(
        monitor.egress_bytes(),
        0,
        "SC5 VIOLATION: the proactive path emitted bytes before any Send"
    );

    // Hash the preview bytes *now*, before egress is possible.
    let preview_hash = preview_payload_sha256(&payload);

    // (2) The user approves and sends. Only the preview panel may set this flag
    // (context_payload.rs §, doc 15 §2 law (b)); we model that here.
    let mut approved = payload;
    approved.user_approved = true;
    preview_send(&approved);

    // Bytes appear ONLY now.
    assert!(
        monitor.egress_bytes() > 0,
        "an approved Send must actually transmit (no silent drop)"
    );

    // (3) preview == wire: the bytes the user saw are the bytes that left.
    let wire_hash = monitor
        .wire_payload_sha256()
        .expect("a Send must have produced wire bytes to hash");
    assert_eq!(
        preview_hash, wire_hash,
        "SC5 VIOLATION: wire bytes differ from the previewed bytes (preview != wire)"
    );
}
