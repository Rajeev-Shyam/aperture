//! Milestone validation gates (doc 16).
//!
//! Doc 16 gives every milestone a **measured/proven** gate that must pass before
//! the next stage starts; gate results overwrite the corresponding `[VERIFY]`
//! figures in docs 01/04. This crate is the home for those gates as ordinary
//! `cargo test` integration tests — the real surface lives under `tests/`, not
//! here. The library itself is intentionally empty.
//!
//! The gates implemented here:
//!
//! | File                      | Gate | Milestone | Invariant guarded |
//! |---------------------------|------|-----------|-------------------|
//! | `tests/m0_schema_roundtrip.rs` | schema round-trips every `EventType`; fakes compile | M0 | data-model fidelity (doc 03) |
//! | `tests/sc5_network_monitor.rs` | zero egress on the proactive path; bytes only after Send | M1→ (strict at M7) | the two-emitter transparency gate (doc 13 §2) |
//! | `tests/sc6_vram_release.rs`    | toggle OFF → VRAM ~0 in < 3 s, sidecars dead, idle CPU < 2 % | M1 (RTX target) | the capture toggle (doc 04 §, doc 05 §5) |
//!
//! SC5 and SC6 are **permanent** CI regression gates from M1/M7 on (doc 16,
//! staged recommendation 3): the two trust foundations — zero silent egress and
//! a toggle that truly releases — are protected forever. The two on-target gates
//! (SC5 strict, SC6) are `#[ignore]`-gated until their measurement backend / the
//! RTX 5060 target is available; the M0 gate runs everywhere, including CI.

// TODO(M0:): nothing public ships from this crate — keep it empty. If future gates
// need shared scaffolding (e.g. a scratch-DB builder, a proactive-path driver over
// the contracts fakes), factor it into `pub(crate)` helpers here and re-use across
// the `tests/` harnesses rather than copy-pasting per gate.
