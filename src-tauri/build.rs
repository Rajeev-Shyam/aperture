//! Tauri v2 build script (doc 16 M0). Generates the compile-time context from
//! `tauri.conf.json` (overlay window config, capabilities, externalBin sidecars)
//! and embeds icons. Must run before `main.rs` is compiled.

fn main() {
    // TODO(M0): if a capability/permission gate is added for the two-emitter rule
    // (doc 13 §2), assert here that no network plugin permission leaks into the
    // overlay capability — the shell must never gain socket access.
    tauri_build::build();
}
