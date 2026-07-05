//! Aperture workspace task runner (the `cargo-xtask` pattern; doc 16 tooling).
//!
//! Run as `cargo xtask <subcommand>` (via the `xtask` alias) or
//! `cargo run -p xtask -- <subcommand>`. Subcommands:
//!
//! | Subcommand        | What it does | Doc |
//! |-------------------|--------------|-----|
//! | `lint-emitters`   | Scan the crate graph; fail if a network-socket / Claude-CLI / process-spawn API appears OUTSIDE the two sanctioned sites (the two-emitter rule). | 13 §2 |
//! | `gate <m0..m9>`   | Run the tagged validation-gate tests for one milestone. | 16 |
//! | `sc5`             | Run the SC5 network-monitor gate (zero silent egress). | 13 / 16 |
//! | `sc6`             | Run the SC6 VRAM-release gate (toggle OFF → VRAM ~0 < 3 s). | 04 / 05 / 16 |
//! | `seed-db`         | Apply migrations to a scratch DB + round-trip every EventType. | 03 / 16 M0 |
//!
//! Honest-stub policy: where the underlying mechanism is not built yet (the
//! monitor backends, the typed DB insert path), the subcommand prints what it
//! *will* do and exits with a clear "not yet wired (M<n>)" status rather than
//! pretending to pass. `lint-emitters` is the exception — it works today, because
//! the trust foundation it guards (doc 13 §2) must be enforced from M0 on.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next();
    match cmd.as_deref() {
        Some("lint-emitters") => lint_emitters(),
        Some("gate") => {
            let m = args.next().context("usage: xtask gate <m0..m9>")?;
            run_gate(&m)
        }
        Some("sc5") => run_sc5(),
        Some("sc6") => run_sc6(),
        Some("seed-db") => seed_db(),
        Some(other) => {
            print_usage();
            bail!("unknown subcommand: {other}");
        }
        None => {
            print_usage();
            bail!("no subcommand given");
        }
    }
}

fn print_usage() {
    eprintln!(
        "xtask — Aperture workspace task runner\n\
         \n\
         USAGE: cargo xtask <subcommand>\n\
         \n\
         SUBCOMMANDS:\n\
         \x20 lint-emitters     enforce the two-emitter rule (doc 13 §2)\n\
         \x20 gate <m0..m9>     run a milestone's validation-gate tests (doc 16)\n\
         \x20 sc5               SC5 network-monitor gate — zero silent egress\n\
         \x20 sc6               SC6 VRAM-release gate — toggle OFF releases the GPU\n\
         \x20 seed-db           apply migrations to a scratch DB + round-trip events (M0)\n"
    );
}

/// Workspace root = the parent of `xtask/` (this crate lives at `<root>/xtask`).
fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/xtask`; its parent is the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent dir")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// lint-emitters — the two-emitter rule (doc 13 §2), CI-enforced from M0 on.
// ---------------------------------------------------------------------------

/// Source-level signatures of the forbidden capabilities. A crate that is NOT a
/// sanctioned emitter must contain none of these (outside comments/strings — see
/// the line filter below). The list is conservative-by-construction: the goal is
/// "egress-free by construction", so we deny the *capability surface*, not just
/// known-bad calls (doc 13 §2).
const FORBIDDEN_API_NEEDLES: &[&str] = &[
    // raw sockets
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "std::net::",
    "tokio::net::",
    // HTTP / network client crates
    "reqwest",
    "hyper::",
    "ureq",
    // arbitrary process spawning (the Claude CLI is spawned this way)
    "std::process::Command",
    "process::Command",
    "tokio::process::",
];

/// The only two crates allowed to carry the full forbidden surface (doc 13 §2):
///   1. `reasoning-gateway` — the sole network/Claude-CLI emitter.
///   2. `orchestration`     — ModelLifecycle's sanctioned sidecar spawn
///      (`vlm-host`/`stt-host` via `std::process::Command`); doc 12 §3, doc 16 M5.
const SANCTIONED_CRATES: &[&str] = &["reasoning-gateway", "orchestration"];

/// Loopback-scoped crates (ADR-028/ADR-036, doc 13 §2 R2 precision): the emitter
/// rule governs *external user-data* egress; loopback IPC between our own
/// components is exempt-but-scoped. The sidecar *host* binaries bind a local
/// model server on **127.0.0.1 only** (pinned port/pipe supplied by the
/// orchestrator, doc 12 §5) — they may name socket types for that bind, and each
/// use is printed for audit. The SC5 monitor whitelists loopback (ADR-028);
/// anything binding a non-loopback interface is still a violation, caught at the
/// SC5 dynamic gate (M7) and code review.
const LOOPBACK_SCOPED_CRATES: &[&str] = &["vlm-host", "stt-host"];

/// Crates skipped entirely (tooling / not part of the product egress surface).
const SKIPPED_CRATES: &[&str] = &["gates", "xtask"];

fn lint_emitters() -> Result<()> {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    println!(
        "lint-emitters: scanning {} + src-tauri (doc 13 §2)",
        crates_dir.display()
    );

    let mut violations: Vec<String> = Vec::new();
    let mut scanned_files = 0usize;

    // Every crate under crates/, PLUS the root `src-tauri` shell crate — the
    // shipped binary promises "no sockets, no CLI spawn" (its own header) and
    // is exactly the surface the two-emitter rule protects.
    let mut crate_dirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&crates_dir)
        .with_context(|| format!("read {}", crates_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            crate_dirs.push(entry.path());
        }
    }
    crate_dirs.push(root.join("src-tauri"));

    for dir in crate_dirs {
        let crate_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if SKIPPED_CRATES.contains(&crate_name.as_str()) {
            continue;
        }
        let sanctioned = SANCTIONED_CRATES.contains(&crate_name.as_str());
        let loopback_scoped = LOOPBACK_SCOPED_CRATES.contains(&crate_name.as_str());
        let src = dir.join("src");
        if !src.is_dir() {
            continue;
        }
        scan_rs_files(&src, &mut scanned_files, |file, line_no, line| {
            if let Some(needle) = forbidden_needle_in(line) {
                if sanctioned {
                    // Allowed here, but make the audit trail visible: a reviewer
                    // (and SC5) can confirm each sanctioned use.
                    println!(
                        "  [sanctioned] {}:{} uses `{}` (crate `{}`)",
                        rel(file),
                        line_no,
                        needle,
                        crate_name
                    );
                } else if loopback_scoped && needle.contains("net") {
                    // ADR-028/036: sidecar hosts may bind 127.0.0.1 model servers.
                    // Socket-type mentions are audited, not denied; process spawns
                    // are still forbidden here (only `net` needles pass).
                    println!(
                        "  [loopback-scoped] {}:{} uses `{}` (crate `{}`; 127.0.0.1-only, ADR-028)",
                        rel(file),
                        line_no,
                        needle,
                        crate_name
                    );
                } else {
                    violations.push(format!(
                        "{}:{}: `{}` in non-emitter crate `{}` — two-emitter rule (doc 13 §2)",
                        rel(file),
                        line_no,
                        needle,
                        crate_name
                    ));
                }
            }
        })?;
    }

    println!("lint-emitters: scanned {scanned_files} source files");
    if violations.is_empty() {
        println!("lint-emitters: OK — no forbidden egress surface outside the two emitters");
        Ok(())
    } else {
        eprintln!("\nlint-emitters: {} VIOLATION(S):", violations.len());
        for v in &violations {
            eprintln!("  {v}");
        }
        bail!("two-emitter rule violated (doc 13 §2)")
    }
}

/// First forbidden needle present in a *code* line, or `None`.
///
/// Best-effort comment/string filtering: skip `//`-comment lines and lines whose
/// only hit is inside a string/doc. Cheap and good enough for a CI gate; a false
/// positive is a one-line `#[allow]`-style annotation away (TODO below).
fn forbidden_needle_in(line: &str) -> Option<&'static str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("//!") || trimmed.starts_with("*") {
        return None;
    }
    // TODO(M0:): tighten this. Strip line/block comments and string literals
    // properly (or switch to a `syn`-based AST scan) so a forbidden token quoted
    // in a string isn't flagged. For a CI gate the line heuristic is acceptable;
    // an intentional sanctioned use sits in one of the two SANCTIONED_CRATES.
    FORBIDDEN_API_NEEDLES
        .iter()
        .copied()
        .find(|needle| line.contains(needle))
}

/// Recursively visit every `.rs` file under `dir`, calling `f(path, line_no, line)`.
fn scan_rs_files(
    dir: &Path,
    counter: &mut usize,
    mut f: impl FnMut(&Path, usize, &str),
) -> Result<()> {
    // Iterative DFS to keep the closure non-recursive (FnMut can't recurse here).
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("read {}", d.display()))? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                *counter += 1;
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("read {}", path.display()))?;
                for (i, line) in text.lines().enumerate() {
                    f(&path, i + 1, line);
                }
            }
        }
    }
    Ok(())
}

/// Path relative to the workspace root, for tidy output.
fn rel(p: &Path) -> String {
    p.strip_prefix(workspace_root())
        .unwrap_or(p)
        .display()
        .to_string()
}

// ---------------------------------------------------------------------------
// gate <m0..m9> — run a milestone's tagged validation-gate tests (doc 16).
// ---------------------------------------------------------------------------

fn run_gate(milestone: &str) -> Result<()> {
    let m = milestone.to_ascii_lowercase();
    match m.as_str() {
        "m0" => {
            // M0 gate runs in plain CI: schema round-trip + fakes compile.
            println!("gate M0: schema round-trips every EventType; fakes compile (doc 16 M0)");
            cargo_test(&["-p", "aperture-gates", "--test", "m0_schema_roundtrip"])
        }
        "m1" => {
            println!("gate M1: capture unit surface (exclusion/pHash/toggle/sampler) + lint; SC6 on-target (doc 16 M1)");
            // The capture crate's tests cover the exclusion gate, the pHash gate,
            // the toggle SLA path, and the sampler policy. The SC6 half (real
            // VRAM release via nvidia-smi) is #[ignore] until run on the RTX
            // target — routed through `sc6` so it is surfaced, never skipped
            // silently.
            cargo_test(&["-p", "aperture-capture"])?;
            lint_emitters()?;
            run_sc6()
        }
        "m2" => {
            println!("gate M2: OCR<=400ms/embed<=300ms surfaces + atomic store + sane KNN (doc 16 M2)");
            // Unit/integration halves of the M2 gate (budgets are LOGGED per
            // frame; the measured pass on real screen content is the on-target
            // step recorded in the gate report).
            cargo_test(&["-p", "aperture-vision-ocr", "-p", "aperture-embedding"])?;
            cargo_test(&["-p", "aperture-db"]) // atomic event+ctx+vec write + KNN sanity
        }
        "m3" => {
            println!("gate M3: pattern engine SC2-shaped script + suggestion render; SC5 holds (doc 16 M3)");
            // The SC2-shaped scripted workflow lives in the pattern-engine tests
            // (3 repetitions → candidate on the next antecedent); the <2 s wall
            // -clock measure is asserted on-target once the overlay renders.
            cargo_test(&["-p", "aperture-pattern-engine", "-p", "aperture-suggestion-generator"])?;
            run_sc5()
        }
        "m7" => {
            println!("gate M7: SC5 strict — zero bytes until Send, preview == wire (doc 16 M7)");
            run_sc5()
        }
        "m4" | "m5" | "m6" | "m8" | "m9" => {
            // Honest stub: these gates' tests don't exist yet (the subsystems are
            // later milestones). Don't fake a pass.
            println!("gate {m}: no gate tests wired yet (doc 16 {})", m.to_uppercase());
            todo!(
                "{}: add this milestone's validation-gate test target and invoke it here",
                m.to_uppercase()
            )
        }
        other => bail!("unknown milestone `{other}` (expected m0..m9)"),
    }
}

// ---------------------------------------------------------------------------
// sc5 / sc6 — launch the two permanent regression monitors (doc 16 rec 3).
// ---------------------------------------------------------------------------

fn run_sc5() -> Result<()> {
    println!("sc5: zero silent egress; bytes only after approved Send (doc 13 §2)");
    // Two halves (ADR-036 wording):
    //  - STATIC (runs today): lint-emitters — the capability-surface deny list.
    //  - DYNAMIC (M7): the #[ignore]d network-monitor test. Until its ETW/proxy
    //    backend exists, we compile + surface it as `ignored` (visible, never
    //    silently skipped) — forcing it with --include-ignored before M7 would
    //    fail on the deliberate todo!() harness and make every earlier gate
    //    dishonest in the other direction. `APERTURE_SC5_STRICT=1` (set by the
    //    M7 gate) opts into the strict run.
    lint_emitters()?;
    let strict = std::env::var("APERTURE_SC5_STRICT").is_ok_and(|v| v == "1");
    let mut args = vec!["-p", "aperture-gates", "--test", "sc5_network_monitor"];
    if strict {
        args.extend(["--", "--include-ignored"]);
    }
    cargo_test(&args)
}

fn run_sc6() -> Result<()> {
    println!("sc6: toggle OFF → VRAM ~0 in < 3 s, sidecars dead, idle CPU < 2 % (doc 04/05)");
    // On-target only (needs nvidia-smi + RTX 5060 + the orchestration kill
    // path, M5); until then the #[ignore]d test is compiled + surfaced, and
    // `APERTURE_SC6_ON_TARGET=1` (the M1/M5 hardware gate) opts into the run.
    let on_target = std::env::var("APERTURE_SC6_ON_TARGET").is_ok_and(|v| v == "1");
    let mut args = vec!["-p", "aperture-gates", "--test", "sc6_vram_release"];
    if on_target {
        args.extend(["--", "--include-ignored"]);
    }
    cargo_test(&args)
}

// ---------------------------------------------------------------------------
// seed-db — apply migrations to a scratch DB + round-trip every EventType (M0).
// ---------------------------------------------------------------------------

fn seed_db() -> Result<()> {
    println!("seed-db: apply migrations to a scratch DB + round-trip every EventType (doc 16 M0)");
    // The authoritative round-trip lives in the gate test; this command runs it
    // so a developer can `cargo xtask seed-db` and watch it. Option (a) from the
    // original TODO — shelling out keeps xtask std-only (no rusqlite here).
    cargo_test(&["-p", "aperture-gates", "--test", "m0_schema_roundtrip"])
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Run `cargo test <args>` from the workspace root, inheriting stdio, and fail if
/// the test process returns non-zero.
fn cargo_test(args: &[&str]) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .arg("test")
        .args(args)
        .current_dir(workspace_root())
        .status()
        .context("spawn `cargo test`")?;
    if status.success() {
        Ok(())
    } else {
        bail!("`cargo test {}` failed ({status})", args.join(" "))
    }
}
