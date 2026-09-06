//! Milestone validation gates (doc 16).
//!
//! Doc 16 gives every milestone a **measured/proven** gate that must pass before
//! the next stage starts; gate results overwrite the corresponding `[VERIFY]`
//! figures in docs 01/04. This crate is the home for those gates as ordinary
//! `cargo test` integration tests — the real surface lives under `tests/`, not
//! here. The library itself is intentionally empty.
//!
//! The gates implemented here (table refreshed 2026-09-05; the authoritative
//! per-test list is `docs/engineering/components/gates.md`):
//!
//! | File                      | Gate | Milestone | Invariant guarded |
//! |---------------------------|------|-----------|-------------------|
//! | `tests/m0_schema_roundtrip.rs` | schema round-trips every `EventType`; sqlite-vec KNN; fakes compile | M0 | data-model fidelity (doc 03) |
//! | `tests/m4_us1_resume.rs`       | US1 resume (offline half): capture → `connector_state` → freshest-lookup → render | M4 | one-click state resumption (doc 10, US1) |
//! | `tests/m5_budget_ceiling.rs`   | no admission ever projects over 7.0 GB; STT is the swap victim | M5 | the 8 GB VRAM ceiling (doc 04 R1, ADR-030) |
//! | `tests/m5_wake_band.rs`        | VLM wakes never cross the ~10/h hard ceiling; the window slides | M5 | voice is never starved by enrichment (ADR-032) |
//! | `tests/m5_load_times.rs`       | (on-target, `#[ignore]`, bodies `todo!()`) SC3 cold-load SLAs + measured co-resident VRAM ≤ 7.0 GB | M5 (RTX target) | SC3 load times + ADR-030 on real numbers |
//! | `tests/m6_l2_swap.rs`          | L1 conditional co-residency; the L2 STT swap admits only after eviction; SC4 slot (`#[ignore]`) | M6 | the swap half of the VRAM ceiling (ADR-030) |
//! | `tests/m9_privacy.rs`          | truthful encryption status + unreadable DB (SQLCipher, default feature); Purge All real on disk; excluded apps never framed; audit answers "what left"; consent fails closed | M9 | doc 13 exit criteria |
//! | `tests/sc5_network_monitor.rs` | zero egress on the proactive path; bytes only after an approved Send; preview hash == wire hash (real, **not** ignored since 2026-08-22) | M1→ (strict at M7) | the two-emitter transparency gate (doc 13 §2) |
//! | `tests/sc6_vram_release.rs`    | (on-target, `#[ignore]`) toggle OFF → VRAM ~0 in < 3 s, sidecar tree dead; resolves sidecars via `SidecarConfig::resolve` like the app | M1 (RTX target) | the capture toggle (doc 04 §5, doc 05 §5) |
//! | `tests/v2m0_uia_executor.rs`   | (on-target, `#[ignore]`, moves the mouse) the real `UiaExecutor` drives a live Notepad end to end | V2-M0 | doc 22 §10 |
//!
//! SC5 and SC6 are **permanent** regression gates from M1/M7 on (doc 16, staged
//! recommendation 3): the two trust foundations — zero silent egress and a
//! toggle that truly releases — are protected forever. SC5 runs on every
//! `cargo test --workspace`; SC6, SC3, SC4 and V2-M0 are `#[ignore]`d and
//! owner-run on the RTX target (`CLAUDE.md` §6). There is no CI.

// TODO(M0:): nothing public ships from this crate — keep it empty. If future gates
// need shared scaffolding (e.g. a scratch-DB builder, a proactive-path driver over
// the contracts fakes), factor it into `pub(crate)` helpers here and re-use across
// the `tests/` harnesses rather than copy-pasting per gate.
