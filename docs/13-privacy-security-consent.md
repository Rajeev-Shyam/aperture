# Doc 13 — Privacy, Security & Consent Design

## 1. Principles & threat model
**Protects against:** silent data exfiltration (by us — the architecture makes it impossible, not just policed); over-collection (sensitive apps, raw frames); casual local snooping of history; payload surprise (you always see what ships).
**Honest posture on minimization (ADR-029):** with **empty default exclusions** and **broad browser-extension reach** (both by the user's choice, Q15/Q61), "data minimization *by default*" would be an overclaim. The accurate framing is **minimal *defaults* + user-driven minimization + transparent disclosure**: the architecture still makes *silent* exfiltration impossible and every send is previewed/audited, but v1 does **not** claim aggressive default *collection*-minimization. Compensating controls: detect-and-suggest onboarding, the Activity & Privacy view, and one-click domain/app exclusion.
**Out of scope (stated honestly):** an attacker with local admin / same-user malware can read what the user can read; DRM-grade screen protection; forensic-grade deletion of OS-level traces.

## 2. The cloud boundary — the emitter rule, made precise (architectural, testable)
Exactly **one crate** (`reasoning_gateway`, Doc 09) may open **application** network sockets or spawn the Claude CLI. Stated precisely (ADR-036):
- **Raw user data** (history, OCR text, payloads, titles, URLs) leaves **only via the gateway crate**, only on a payload flagged `user_approved=true` (an explicit Send **or** an active scoped allow — §3, ADR-026). Unchanged.
- **Opt-in, off-by-default, aggregate-only diagnostics** (wake rate, queue waits, VRAM peaks, click-through *rates* — never content) is **routed through the gateway crate itself**, so "only the gateway opens app sockets" stays literally true; each send is audited like `cloud_send` (Q89).
- **The Tauri app-updater** is a **separate framework path** carrying only version/binary requests and **no user-derived data**; it is documented and **excluded from the SC5 *user-data*-egress test** (but still visible to the network monitor).
- **Loopback IPC** — the extension's native-messaging **fallback** (ADR-028) — is on-device, so it is exempt-but-scoped: it must bind **127.0.0.1 only**, be **authenticated** (per-install token), and the **SC5 monitor whitelists loopback** so it cannot false-trip zero-egress.

Everything else — capture, OCR, embeddings, patterns, the DB — is egress-free by construction.
**Enforcement:** (a) CI lint denying socket/process-spawn APIs outside the gateway crate [ASSUMPTION: clippy/custom lint]; (b) the SC5 network-monitor test in CI and at every milestone gate: *zero **user-data** bytes on the proactive path; user data leaves only after Send/scoped-allow; updater traffic distinguished; loopback whitelisted.*

## 3. Context transparency, end-to-end (G7)
- One serialized object is built, previewed, edited, and transmitted — **preview == wire** is a data-flow property (single object), not a UI promise (Docs 03 §4, 11 §4).
- The preview always shows: every item (expandable, removable), every redaction (rule + count), the transport target, the size/token estimate.
- **Send** is the only manual egress trigger; **Cancel** leaves zero residue; the payload's SHA-256 + transport + byte count are written to the local `cloud_send` audit log.
- **Scoped allow (ADR-026):** a per-app+intent allow may automate the *Send click*, but the exact payload is **still rendered**, a **cancel window** (default 3 s, configurable) **still precedes egress**, and the SHA-256 is **still audit-logged**. Only the manual click is skipped — visibility is unchanged.
- The MCP (pull) transport enforces the same gate **inside the tool handler** — Claude Desktop's tool call blocks on the user's preview decision (Doc 09 §3).
- **Gated history search (ADR-037):** the `aperture_search_history` MCP tool lets Claude *propose* a query; the handler runs Doc 03 §5 retrieval, applies **redaction + exclusions**, and **shows the user the matched results before anything returns**, auditing each return. Claude can pull relevant history, but nothing leaves unseen.

## 4. Data minimization at the source
- **Raw frames are never persisted** — frame → OCR → drop (Doc 05 §2); only OCR text + a perceptual hash are stored.
- **Exclusion lists** stop collection at the earliest gate (Doc 05 §4): match by process / window-class / title-regex / **`url_pattern`** (ADR-040); excluded contexts yield metadata-only events flagged `EXCLUDED` and can never appear in any payload. **Defaults ship empty** (ADR-029/Q15) — the frictionless "max user control" choice; safety is restored by **detect-and-suggest** onboarding (§8, scans installed password managers / banking apps locally and *suggests* exclusions the user confirms — never auto-excluded) and a one-click **"exclude this domain"** action on any browser bubble.
- **The browser extension reads URLs + video position only — never page DOM/content** (ADR-029). Extension-sourced URLs traverse the **same exclusion + redaction pipeline** as UIA-sourced ones (FIX 2.2) and respect `url_pattern` exclusions and incognito.
- Private/incognito browser windows: detected via title suffix heuristics and treated as excluded [VERIFY reliability per browser].

## 5. Redaction pipeline (runs at payload assembly, before preview)
Ordered deterministic rules over every text item:
| Order | Rule | Mechanism |
|---|---|---|
| 1 | Secrets/keys | regex for common token shapes (AWS/`sk-`/PEM headers/JWT) |
| 2 | Payment cards | 13–19 digit runs passing Luhn |
| 3 | IBAN / account-like | country-prefixed IBAN regex |
| 4 | Email addresses | RFC-lite regex |
| 5 | Phone numbers | E.164-ish + local formats |
| 6 | User-defined terms | literal/regex list from settings |
Replacements are typed placeholders (`⟨email#1⟩`); every hit increments `redactions[]` shown in the preview. Misses are mitigated by the per-item remove/edit affordance — the human is the last redactor by design.

## 6. At-rest protection
- DB encrypted with SQLCipher-style page encryption [VERIFY exact crate]; the key is generated per-install, wrapped by **DPAPI (current user)**, stored in Windows Credential Manager [VERIFY API surface].
- **Optional recovery passphrase (ADR-038):** off by default (keeps the frictionless DPAPI flow); when set, it derives a **second key-encryption-key via Argon2id**, providing recovery if the Windows account is lost. It is a second (opt-in, Argon2-hardened) attack surface, documented.
- Key loss ⇒ DB unreadable **by design**; documented plainly ("your history cannot be recovered without your Windows account **or your recovery passphrase, if you set one**").
- Settings and exclusion lists live inside the same encrypted DB.

## 7. Retention, purge, audit
- TTL defaults per Doc 03 §6 (events 90 d, OCR text 30 d, voice 30 d, suggestions/patterns 180 d — all user-adjustable); nightly pruner.
- **Purge All:** truncate + VACUUM, one click, with confirmation.
- **Audit log (local only):** `capture_toggle`, `cloud_send`, **and opt-in diagnostics sends** — the user can always answer "when was it watching?" and "what ever left this machine?". Surfaced in the **Activity & Privacy view** (ADR-040). Audit rows survive purge for 30 d, then expire [ASSUMPTION].

## 8. Consent UX summary
**First-run sequence (ADR-040):** **consent → detect-and-suggest sensitive apps → browser-extension install → enable capture** (capture stays OFF until consented). The safety setup is surfaced at the right moment without being forced. The indicator is always truthful (Doc 05 §5); voice is opt-in at first PTT use (mic permission flow). **Cold-start:** a subtle one-time *"learning your patterns"* note, then silence until the pattern floors are met (Q92).
Every cloud send is approved — either individually **or** under a **scoped allow** (ADR-026, supersedes the old "no always-allow in v1"): still payload-displayed + cancel-window + audited. A **global suggestion snooze** (15 min / 1 h / until re-enabled) silences bubbles while capture + learning continue — **distinct from the capture toggle**, which stops everything (ADR-040/Q95).

## 9. Failure modes
| Failure | Behavior |
|---|---|
| Redactor false negative | Preview edit/remove is the backstop; add-term affordance turns a miss into a rule |
| Exclusion list gap | One-click "exclude this app" from any bubble's overflow menu |
| Audit log tampering (local admin) | Out of threat model; noted in docs |
| Encryption lib CVE | Key wrapping isolates blast radius; lib pinned + tracked in Doc 17 |
| Broad extension host access (RK14) | Broad permission, **narrow use** (URLs + position only); exclusions/incognito gating; install-time disclosure; `url_pattern` + "exclude this domain"; residual exposure **accepted** (Q61) |

---
## Implementation status (2026-07-08) — redaction + audit pulled forward to M7

- **Redaction pipeline implemented (§5)** — ahead of the M9 privacy milestone, because the gateway structurally needs redaction-**before**-preview (doc 09 §5). `redaction::Redactor` runs the ordered rules (secret / payment-card-with-Luhn / IBAN / email / phone / user-terms) over every text-bearing payload item — including recursive JSON **strings and numeric values** — replacing hits with `⟨noun#n⟩` and recording the per-rule counts the preview shows. Phone matches require a formatting separator, so a bare long digit run is left for the Luhn card rule + the human (never falsely scrubbed).
- **Audit hash (§3).** `audit_log::sha256_hex` is implemented; the gateway records the `cloud_send` hash over the transport's **actual wire bytes**, **after** a successful send (never a phantom row on a failed send), tagged with the transport that actually egressed.
- **Two-emitter rule (§2).** The CLI-spawn / HTTPS egress primitives now **self-guard** on `user_approved` in addition to the gateway chokepoint. Enforcement today = dependency direction + the SC5 gate; the scoped CI lint is still a TODO (the contracts comment was softened from "CI-lint enforced" to match).
Full session detail: `docs/handoff/session-bridge-2026-07-08-m6-m8.md`.

---
## Implementation status (2026-08-09) — M9 landed

**M9 is implemented and gated** (`crates/gates/tests/m9_privacy.rs`, `cargo xtask gate m9`).

- **At-rest key management (§6) — real, and verified on Windows.** `key_manager` now uses
  `BCryptGenRandom` (system-preferred RNG) → `CryptProtectData` (DPAPI, current-user scope) →
  `CredWriteW` (Credential Manager, `CRED_PERSIST_LOCAL_MACHINE`). `DbKey` is `Zeroizing` and its
  `Debug` prints `redacted`. Tests exercise the **real OS APIs**: CSPRNG non-constancy, DPAPI
  round-trip, tamper→fail-closed, Credential-Manager round-trip/miss/delete, and key stability
  across calls (a changed key ⇒ the old DB is unreadable, by design).
- **At-rest *encryption* (§6) — wired, OFF by default, and honest about it.** `Db::open_encrypted`
  applies `PRAGMA key = "x'<hex>'"` as the first statement and then forces a schema read so a wrong
  key surfaces `DbError::Decryption`. It sits behind the **`sqlcipher` cargo feature**
  (`rusqlite/bundled-sqlcipher-vendored-openssl`), which is off by default because that build
  compiles OpenSSL from source and needs a **native Windows Perl (Strawberry) + NASM on PATH** —
  the Git-for-Windows/Cygwin perl fails on a missing `Locale::Maketext::Simple`. With the feature
  off, `Db::is_encrypted()` returns `false`, the shell logs a prominent warning, the first-run and
  privacy UIs *say* the history is not encrypted, and the gate prints an explicit INCOMPLETE note.
  **Nothing anywhere claims encryption that was not applied.**
  → **Carry-forward:** install Strawberry Perl + NASM, then
  `cargo test -p aperture-gates --features sqlcipher --test m9_privacy` to close criterion 1.
- **Audit log (§3, §7) — persisted.** `AuditLog` writes `capture_toggle` / `cloud_send` rows as
  ordinary `Event`s into the encrypted DB (so they inherit at-rest protection and the retention
  pruner's audit window for free — no second, weaker store). The gateway holds an `AuditSink` trait
  (`NullAuditSink` by default, DB-backed via `Gateway::with_audit`) so it stays unit-testable
  without a database. **An audit-write failure never fails a Send** — the bytes already left, and
  reporting a completed send as failed is a worse lie than a missing row; it logs at `error`.
- **Consent (§8) — persisted + audited, fails closed.** `ConsentManager` stores `ConsentState` in
  the encrypted `settings` table under key `consent`; an **unparseable record defaults to capture
  OFF** (an unreadable consent record is not evidence of consent). Every capture transition
  persists *then* writes a `capture_toggle` row. `toggle_capture` aborts an ON transition if the
  audit write fails, but never traps the user in ON on an OFF-path failure.
- **Purge All (§7) — implemented, with one deliberate amendment.** Doc 03 §6 says "truncates every
  table". `Db::purge_all` preserves three: `exclusion_list` (purging it would silently *resume
  capturing* apps the user excluded — a privacy control must never weaken itself), `settings`
  (holds consent; a *data* purge must not reset it and re-trigger first-run), and
  `schema_migrations`. Everything else goes, audit rows inside `audit_days` survive, then `VACUUM`.
- **Exclusions (§4) — de-duplicated, not rebuilt.** `privacy::exclusion_manager` was a second,
  weaker copy of logic `aperture_capture::exclusion` already implemented in full (process /
  window_class / title_regex / `url_pattern` + the private-window heuristic, running *inside* the
  capture gate before any frame is pulled). It was **deleted** rather than kept in sync. The
  persisted half lives in `db` (`read_exclusion_list` / `add_exclusion_rule` /
  `set_exclusion_enabled`); the shell compiles the enabled rows into the `ExclusionList` at startup.
- **Detect-and-suggest (§4, §8) — new `privacy::detect_suggest`.** A local, one-level filesystem
  scan of the standard install roots produces *candidates* from a small curated catalogue of
  password managers / finance apps. It never auto-excludes (ADR-029/Q15 holds: shipped defaults
  stay empty), never egresses, and a failed scan yields fewer suggestions rather than a blocked
  first run.
- **First-run + Activity & Privacy UI (§7, §8; ADR-040).** `FirstRunConsent.tsx` runs the ordered
  sequence (consent → detect-and-suggest → extension → enable capture); declining completes
  first-run with capture OFF and never re-nags. `PrivacyPanel.tsx` is the Activity & Privacy view:
  the audit feed (each `cloud_send` showing transport, byte count, and the SHA-256 prefix),
  exclusion add/disable/delete, and Purge All behind a typed `DELETE` confirmation that states
  plainly what survives. Both render as **opaque** chrome, never glass — they are the largest
  surfaces in the app and would otherwise breach the ≤2 glass budget (doc 14 §5).
- **Two-emitter CI lint (§2) — now actually scoped.** `xtask lint-emitters` was **failing** before
  M9: the `vlm-host`/`stt-host` loopback sidecars tripped it (12 violations), because the
  loopback carve-out tested `needle.contains("net")`, which never matches `reqwest` or
  `TcpListener`. The carve-out now audits the sidecars' whole socket/spawn surface **and adds the
  check that makes the exemption sound**: any non-loopback host (a public URL, `0.0.0.0`, an
  unresolvable literal) inside a loopback-scoped crate is a violation. That boundary has its own
  unit tests in `xtask`. The SC5 byte-monitor remains the authoritative dynamic backstop.

- **Purge All is real on disk, not just logical.** The first implementation ran `DELETE; VACUUM` on
  a WAL-mode connection held open for the process lifetime — which rewrites into `history.db-wal`
  and leaves every "purged" row verbatim-recoverable from the files (plaintext in the default
  build). It now runs `PRAGMA wal_checkpoint(TRUNCATE)` after the VACUUM, and the connection sets
  `PRAGMA secure_delete=ON` so freed pages don't carry content between the nightly pruner's deletes
  and the next VACUUM. Asserted by scanning the raw bytes of `history.db`/`-wal`/`-shm` for a
  sentinel (`m9_purged_content_is_not_recoverable_from_the_files_on_disk`).
- **Modal surfaces make the overlay interactive.** The overlay window is click-through
  (`WS_EX_TRANSPARENT`) and unfocusable by design — correct for passive bubbles, fatal for a dialog.
  `overlay::set_interactive` + the `set_overlay_interactive` command, paired by the UI's
  `useModalSurface` hook, clear the bit while a modal is mounted. Without it the first-run consent
  buttons (and the pre-existing Context Preview panel's Send/Cancel) were literally unclickable.
- **Exclusion patterns are validated at entry.** `ExclusionList::compile` fails *open* on a bad
  regex — right at load time, wrong at entry time, because it yields a rule the UI shows as an
  active protection while it matches nothing. `exclusion::validate_pattern` (same `RegexBuilder`
  config as the compile path) gates `add_exclusion`.
- **`capture_toggle` rows share one schema across both writers.** `aperture_capture::toggle` writes
  the *mechanism* row (capture actually started/stopped, `source: "capture"`) and
  `aperture_privacy::audit_log` the *decision* row (`source: "consent"`). Both carry `enabled` +
  `reason`. Keeping both is deliberate: a decision row with no matching mechanism row means capture
  was requested and never actually ran, which the trail must show rather than hide.

**Known gap (deliberate, documented):** a rule added from the Activity & Privacy view is durable
immediately but the *running* capture matcher is built at startup, so it applies from the next
launch. Hot-reload needs an interior-mutable `ExclusionList` in `aperture-capture`; deferred rather
than bolted on.

**Pre-existing gap surfaced by the M9 review (not fixed here):** `overlay::set_hit_test_rects` has
**no callers** — the bubble input path was never wired (an M3-UI TODO). M9 works around it for modal
surfaces via `set_interactive`; bubble click-through still needs its own wiring in the v1 close-out.

Full session detail: `docs/handoff/session-bridge-2026-08-09-m9.md`.

---
> **R2 amendments applied** (see docs/19–21): ADR-029 (honest minimization reframe; extension URL-only use; empty default exclusions), ADR-036 (precise emitter rule; diagnostics; updater carve-out), ADR-028 (loopback-fallback scoping + SC5 whitelist), ADR-026 (scoped-allow transparency), ADR-037 (gated `aperture_search_history`), ADR-040 (`url_pattern`, first-run sequence, Activity & Privacy view, global snooze, cold-start note), ADR-038 (optional Argon2id recovery passphrase). Redaction rules (Q21) and 30 d audit survival (Q18) unchanged.

## Implementation status (2026-08-13) — v1 wiring + review hardening

- **Approval is content-bound (§3).** `preview_set_approved` re-redacts the synced edits and hashes the canonical bytes; `preview_send` ships the core-owned object only after re-verifying that hash — a client cannot substitute content after the gate, and panel-added text cannot bypass the redactor (2026-08-13 multi-agent review, HIGH).
- **Voice follows the mechanism, not the decision (§8).** Voice enable and PTT gate on the LIVE ToggleOwner state; a failed `capture.start()` can no longer leave the mic arm-able off a stale `consent.capture_enabled`.
- **Exclusion hot-reload (§4).** `ExclusionList` is interior-mutable; `add_exclusion`/`set_exclusion` recompile+swap the live matcher (keep-previous-on-error). Settings first-run seeding is per-key `INSERT OR IGNORE`, keyed on the `reasoning` row (never table-emptiness — `consent` shares the table).
- The gateway now actually carries the DB-backed `AuditLog` (`Gateway::with_audit`) — `cloud_send` rows persist.
Full detail: `docs/handoff/session-bridge-2026-08-13-v1-wiring.md`.
