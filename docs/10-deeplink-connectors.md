# Doc 10 — Deep-Link / State-Resumption Connectors

## 1. The connector contract (the expansion seam — G3)
```rust
trait Connector {
  fn id(&self) -> &'static str;                       // 'browser'|'youtube'|'document'|'ide'
  fn can_capture(&self, ev: &Event) -> bool;          // cheap predicate on the bus
  fn capture(&self, ev: &Event) -> Option<ConnectorState>;   // → reconstruct_payload (versioned JSON)
  fn staleness_ttl(&self) -> Duration;                // when captured state stops being trustworthy
  fn reconstruct(&self, st: &ConnectorState) -> Result<ResumeArtifact>; // URL | ProtocolUri | FileOpen{path, app_hint}
  fn open(&self, a: &ResumeArtifact) -> Result<OpenOutcome>;  // ShellExecuteW / protocol dispatch
  fn validate(&self, cloud_payload: &Json) -> Option<ConnectorState>; // gate for Claude-suggested actions (Doc 09 §4)
}
```
Registry: connectors self-register at startup; the pattern engine and Bubble UI know **only this trait**, so v2 connectors (Slack thread, terminal cwd, Figma frame…) plug in without core changes. The primary v2 expansion target is **communication-app threads (Slack / Teams / Discord)** (Q90), so the trait must accommodate **thread/channel deep-links** (`slack://…`, `msteams://…`, etc.) alongside file paths and URLs. `reconstruct_payload` carries `payload_version` for forward migration.

## 2. Browser tab / URL
- **Capture:** on `navigation` events — URL via the **browser extension (tabs API)**, the primary and reliable source (a committed v1 component, ADR-027). **No-extension fallback:** UIA `ValuePattern` on the address-bar Edit element of the foreground browser — **known-flaky (RK4, now demoted to fallback):** the element name is localized ("Address and search bar" is en-US Chrome) and shifts across versions; resolve by UIA `ControlType.Edit` + keyboard-focusable heuristics rather than name alone [VERIFY per browser/version]. Further fallbacks: last-known URL for that window; window title (lossy) as a search hint.
- **Payload v1:** `{url, title, browser}`; TTL **24 h** [ASSUMPTION].
- **Reconstruct/open:** the stored URL → `ShellExecuteW` (default browser). Outcome is a *new tab* — honest framing in copy ("Reopen page"), not tab-restoration. Validation is **on-click** (button shows optimistically; the connector validates before execution and fails gracefully — ADR-035).

## 3. Video timestamp (YouTube)
- **Detect:** `navigation` URLs with `youtube.com/watch` or `youtu.be/` hosts; parse `v=<id>` and any `t=` param.
- **Position capture — hierarchy (RK3 resolved via the v1 extension, ADR-027):**
  1. the **browser extension content script** reads `video.currentTime` directly — the primary, reliable position source;
  2. `t=` present in the observed URL (e.g., after the user used "copy at current time") — exact fallback;
  3. otherwise position unknown ⇒ store `position_s = null` (the bubble then says "from the start").
- **Payload v1:** `{video_id, url, title, position_s|null, observed_ts}`; TTL **7 d** [ASSUMPTION].
- **Reconstruct:** `https://www.youtube.com/watch?v=<id>&t=<s>s` (use `&` when params exist, `?` otherwise; `youtu.be/<id>?t=<s>` equivalent). `null` position ⇒ plain watch URL and the bubble says "from the start" (US1 acceptance d).
- This is the **first connector built at M4** (Q75) — it exercises the whole extension + native-messaging path earliest; the `t=` and `null`→"from the start" rungs remain as fallbacks.

## 4. Documents
- **Capture:** on `document_state` / focus of known editors — path resolution ladder: (1) full path present in the window title; (2) title filename matched against Windows Recent Items; (3) **per-app MRU registry reads** (e.g. each app's recent-files list under its registry hive) — more robust than title-parse but **`[VERIFY]` / version-fragile per app**; (4) unresolved ⇒ no capture (the floor — never guess a path).
- **Payload v1:** `{path, app_hint, title}`; TTL **7 d**, and `reconstruct` re-checks the file exists. The opt-in `app_hint` is unchanged.
- **Open:** `ShellExecuteW(path)` (default handler); `app_hint` used only if the default differs and the user opted into "open with same app" [ASSUMPTION]. Missing file ⇒ offer the containing folder.

## 5. IDE files (VS Code first)
- **Capture:** `ide_state` — the resolution **method (title-parse vs reading `state.vscdb` vs `code -g`) is decided at the M4 spike, per VS Code version** (Q56). Baseline is title parsing — `"● {file} - {workspace} - Visual Studio Code"` (dirty-dot aware) [VERIFY per VS Code version]; path resolved via workspace MRU when the title holds only a filename. Line/col are best-effort (a prior precise `ide_state` if any); else null.
- **Payload v1:** `{path, line|null, col|null, workspace}`; TTL **7 d**.
- **Reconstruct:** `vscode://file/{abs_path}:{line}:{col}` (documented URI form) → protocol handler; fallback `code -g {path}:{line}` CLI [VERIFY availability]; final fallback: plain file open.
- Other editors are v2 connectors behind the same trait (each needs its own scheme: `jetbrains://`, etc.).

## 6. Shared failure handling
| Failure | Behavior |
|---|---|
| `reconstruct` target gone (file deleted, video private) | `open` returns `Failed{reason}` → bubble swaps to fallback copy + nearest-degrade action; `suggestion_clicked{outcome:failed_fallback}` recorded for SC7 |
| Stale state (past TTL) | Pattern engine's freshness factor zeroes the candidate (Doc 08 §5) — stale bubbles are prevented, not apologized for |
| Protocol handler unregistered | Fall back one rung on that connector's ladder |
| Cloud-suggested payload (Doc 09) | `validate()` must produce a well-formed state from the JSON or the action button is withheld |

## 7. Resource cost
CPU-trivial; capture work rides the existing event pipeline; no GPU, no network.

---
> **R2 amendments applied** (see docs/19–21): ADR-027 (browser extension is v1 — browser URL + YouTube position), ADR-035 (validate-on-click); Q90 (v2 comms-thread expansion), Q75 (YouTube built first at M4), Q62 (per-app MRU registry reads), Q56 (VS Code method at M4 spike), Q57 (browser TTL 24 h unchanged).
