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
> **R2 amendments applied** (see docs/19–21): ADR-029 (honest minimization reframe; extension URL-only use; empty default exclusions), ADR-036 (precise emitter rule; diagnostics; updater carve-out), ADR-028 (loopback-fallback scoping + SC5 whitelist), ADR-026 (scoped-allow transparency), ADR-037 (gated `aperture_search_history`), ADR-040 (`url_pattern`, first-run sequence, Activity & Privacy view, global snooze, cold-start note), ADR-038 (optional Argon2id recovery passphrase). Redaction rules (Q21) and 30 d audit survival (Q18) unchanged.
