# Doc 13 — Privacy, Security & Consent Design

## 1. Principles & threat model
**Protects against:** silent data exfiltration (by us — the architecture makes it impossible, not just policed); over-collection (sensitive apps, raw frames); casual local snooping of history; payload surprise (you always see what ships).
**Out of scope (stated honestly):** an attacker with local admin / same-user malware can read what the user can read; DRM-grade screen protection; forensic-grade deletion of OS-level traces.

## 2. The cloud boundary — the two-emitter rule (architectural, testable)
Exactly **one crate** (`reasoning_gateway`, Doc 09) may open network sockets or spawn the Claude CLI, and it acts only on a payload object flagged `user_approved=true` by the preview panel. Everything else — capture, OCR, embeddings, patterns, the DB — is egress-free by construction.
**Enforcement:** (a) CI lint denying socket/process-spawn APIs outside the gateway crate [ASSUMPTION: clippy/custom lint]; (b) the SC5 network-monitor test in CI and at every milestone gate: *zero bytes on the proactive path; bytes only after Send.*

## 3. Context transparency, end-to-end (G7)
- One serialized object is built, previewed, edited, and transmitted — **preview == wire** is a data-flow property (single object), not a UI promise (Docs 03 §4, 11 §4).
- The preview always shows: every item (expandable, removable), every redaction (rule + count), the transport target, the size/token estimate.
- **Send** is the only egress trigger; **Cancel** leaves zero residue; the payload's SHA-256 + transport + byte count are written to the local `cloud_send` audit log.
- The MCP (pull) transport enforces the same gate **inside the tool handler** — Claude Desktop's tool call blocks on the user's preview decision (Doc 09 §3).

## 4. Data minimization at the source
- **Raw frames are never persisted** — frame → OCR → drop (Doc 05 §2); only OCR text + a perceptual hash are stored.
- **Exclusion lists** stop collection at the earliest gate (Doc 05 §4): match by process / window-class / title-regex; excluded contexts yield metadata-only events flagged `EXCLUDED` and can never appear in any payload. Shipped defaults: password managers, common banking domains' window titles [ASSUMPTION — curated list, user-editable].
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
- DB encrypted with SQLCipher-style page encryption [VERIFY exact library]; the key is generated per-install, wrapped by **DPAPI (current user)**, stored in Windows Credential Manager [VERIFY API surface].
- Key loss ⇒ DB unreadable **by design**; documented plainly ("your history cannot be recovered without your Windows account").
- Settings and exclusion lists live inside the same encrypted DB.

## 7. Retention, purge, audit
- TTL defaults per Doc 03 §6 (events 90 d, OCR text 30 d, voice 30 d, suggestions/patterns 180 d — all user-adjustable); nightly pruner.
- **Purge All:** truncate + VACUUM, one click, with confirmation.
- **Audit log (local only):** `capture_toggle` and `cloud_send` events — the user can always answer "when was it watching?" and "what ever left this machine?". Audit rows survive purge for 30 d, then expire [ASSUMPTION].

## 8. Consent UX summary
First-run: explicit opt-in to capture (default OFF until consented) [ASSUMPTION]; the indicator is always truthful (Doc 05 §5); every cloud send is individually approved (no "always allow" in v1 [ASSUMPTION — re-evaluate after dogfood]); voice is opt-in at first PTT use (mic permission flow).

## 9. Failure modes
| Failure | Behavior |
|---|---|
| Redactor false negative | Preview edit/remove is the backstop; add-term affordance turns a miss into a rule |
| Exclusion list gap | One-click "exclude this app" from any bubble's overflow menu |
| Audit log tampering (local admin) | Out of threat model; noted in docs |
| Encryption lib CVE | Key wrapping isolates blast radius; lib pinned + tracked in Doc 17 |
