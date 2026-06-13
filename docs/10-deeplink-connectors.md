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
Registry: connectors self-register at startup; the pattern engine and Bubble UI know **only this trait**, so v2 connectors (Slack thread, terminal cwd, Figma frame…) plug in without core changes. `reconstruct_payload` carries `payload_version` for forward migration.

## 2. Browser tab / URL
- **Capture:** on `navigation` events — URL via UIA `ValuePattern` on the address-bar Edit element of the foreground browser. **Known-flaky (RK4):** the element name is localized ("Address and search bar" is en-US Chrome) and shifts across versions; resolve by UIA `ControlType.Edit` + keyboard-focusable heuristics rather than name alone [VERIFY per browser/version]. Fallbacks: last-known URL for that window; window title (lossy) as a search hint.
- **Payload v1:** `{url, title, browser}`; TTL **24 h** [ASSUMPTION].
- **Reconstruct/open:** the stored URL → `ShellExecuteW` (default browser). Outcome is a *new tab* — honest framing in copy ("Reopen page"), not tab-restoration.
- *v2 path if UIA proves too flaky:* an optional companion browser extension (also fixes YouTube position — below).

## 3. Video timestamp (YouTube)
- **Detect:** `navigation` URLs with `youtube.com/watch` or `youtu.be/` hosts; parse `v=<id>` and any `t=` param.
- **Position capture — honest hierarchy (RK3, the least-certain connector):**
  1. `t=` present in the observed URL (e.g., after the user used "copy at current time") — exact;
  2. periodic `media_state` heuristics where obtainable (player UIA exposure is unreliable [VERIFY]);
  3. otherwise position unknown ⇒ store `position_s = null`.
- **Payload v1:** `{video_id, url, title, position_s|null, observed_ts}`; TTL **7 d** [ASSUMPTION].
- **Reconstruct:** `https://www.youtube.com/watch?v=<id>&t=<s>s` (use `&` when params exist, `?` otherwise; `youtu.be/<id>?t=<s>` equivalent). `null` position ⇒ plain watch URL and the bubble says "from the start" (US1 acceptance d).
- This connector is the M4 de-risk spike; if no reliable position source exists without an extension, v1 ships the degrade path knowingly.

## 4. Documents
- **Capture:** on `document_state` / focus of known editors — path resolution ladder: (1) full path present in the window title; (2) title filename matched against Windows Recent Items / per-app MRU [VERIFY access]; (3) unresolved ⇒ no capture (never guess a path).
- **Payload v1:** `{path, app_hint, title}`; TTL **7 d**, and `reconstruct` re-checks the file exists.
- **Open:** `ShellExecuteW(path)` (default handler); `app_hint` used only if the default differs and the user opted into "open with same app" [ASSUMPTION]. Missing file ⇒ offer the containing folder.

## 5. IDE files (VS Code first)
- **Capture:** `ide_state` from title parsing — `"● {file} - {workspace} - Visual Studio Code"` (dirty-dot aware) [VERIFY per VS Code version]; path resolved via workspace MRU when the title holds only a filename. Line/col are best-effort (a prior precise `ide_state` if any); else null.
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
