# Doc 09 — Reasoning & Claude Integration

## 1. Interface
| | |
|---|---|
| **Inputs** | A Context Payload (Doc 03 §4) that has **passed the transparency gate** (Doc 13) + an explicit user trigger (enrichment click / voice escalation) |
| **Outputs** | `StructuredSuggestions` (schema §4) rendered by the Bubble UI exactly like local output; or a plain-text answer |
| **Resource cost** | Negligible local; network + tokens only on explicit use. **Never invoked by the proactive loop** (locked answer A) |

## 2. The `ReasoningGateway` (one interface, swappable transports)
```rust
trait ReasoningTransport {
  fn id(&self) -> TransportId;                       // claude-desktop-mcp | claude-cli | messages-api
  fn health(&self) -> Health;                         // installed/running/authenticated?
  fn send(&self, payload: &ContextPayload) -> Result<StructuredSuggestions, TransportError>;
}
```
The gateway holds an ordered transport list from settings, picks the first healthy one, and is the **only** crate permitted to open network sockets or spawn the CLI (Doc 13 two-emitter rule; enforced by a CI lint on socket APIs outside this crate [ASSUMPTION]).

## 3. The three transports — and the push/pull asymmetry that shapes the UX
| | Direction | Mechanics | Consent-gate placement | Caveats |
|---|---|---|---|---|
| **Claude Code CLI** (primary candidate) | **Push** — Aperture initiates | Spawn `claude -p <prompt> --output-format json`, headless; parse `{result, total_cost_usd, session_id}` | Preview shown **before** spawning; the approved bytes form the prompt | Documented headless caveats: empty output on large stdin (~7k chars on some versions) and a 10 MB stdin cap ⇒ keep payloads compact, pass long context via a temp file path if supported. **[VERIFY flags, version behavior]** |
| **Claude Desktop via MCP** | **Pull** — Claude initiates | Aperture runs a **local MCP server** (stdio, JSON-RPC 2.0) registered in `%APPDATA%\Claude\claude_desktop_config.json`; exposes tools `aperture_get_context(payload_id)`, `aperture_list_recent`, `aperture_submit_suggestions(json)` | Inside the **tool handler**: when Claude Desktop calls `aperture_get_context`, the handler **blocks**, shows the preview, and returns the payload only on user Send (or returns a refusal). MCP hosts additionally require their own tool-use consent | Aperture **cannot push a prompt into Claude Desktop**; the UX is a handoff ("copied a starter prompt — paste in Claude"), suggestions return via `aperture_submit_suggestions`. [VERIFY config/registration details] |
| **Messages API** | Push | Plain HTTPS to the Messages endpoint; needs the user's API key | Preview before the HTTPS call | **Model name and any beta headers are settings, never code — [VERIFY at build time]** (locked NG8) |
**Fallback order (default):** CLI → Desktop-MCP → API, user-reorderable. Health failures fall through with a visible notice; offline ⇒ the local answer stands and nothing queues silently.

## 4. Structured-output contract (source-agnostic suggestions)
The cloud is asked (via tool schema on MCP/API, or a strict JSON instruction on CLI) to return:
```json
{"suggestions":[{"title":"string","connector_type":"browser|youtube|document|ide|none",
  "reconstruct_payload":{},"rationale":"string"}],
 "answer_text":"optional prose answer"}
```
Validation: schema-check on receipt; `reconstruct_payload` is **re-validated by the target connector** before any bubble offers it (the cloud can suggest, only connectors can act). Invalid suggestions degrade to `answer_text` rendering. This is the same shape local candidates flatten into, so the Bubble UI is source-agnostic (Doc 15 §5).

## 5. Token economics — prompt caching & image discipline
- **Prompt layout:** stable prefix first — system framing + the suggestions JSON schema + standing instructions — with a cache breakpoint; the volatile payload last. Cache reads price at **~10 %** of base input; the 5-minute cache write costs **+25 %** over base (1-hour TTL write ≈ 2×). The stable prefix recurs across calls, so this is real money on the API transport and good latency on all. **[VERIFY current pricing/TTLs at build time.]**
- **Images are the expensive, cache-hostile item:** vision tokens ≈ `⌈w/28⌉ × ⌈h/28⌉` (~`(w·h)/750`), capped around ~1,568 tokens with auto-resize beyond ~1.15 MP — and **any image change invalidates the cache**. Policy: **OCR text is the default context currency; a screenshot is opt-in** via enrichment, pre-downscaled to ≤ 1568 px long edge / ~1.15 MP before preview so the user sees exactly what ships.
- Payload size guard: warn in the preview at > 50 KB serialized [ASSUMPTION]; hard-stop before the CLI stdin caveat threshold on that transport.

## 6. Failure modes
| Failure | Behavior |
|---|---|
| Transport unhealthy (CLI missing, Desktop not running, no API key) | Fall through the ordered list; if none healthy: clear notice + the local answer remains |
| Malformed JSON from the model | One repair round-trip on API/MCP; on CLI, render `answer_text`/raw as prose |
| Oversized payload | Truncation policy (drop oldest `event_trail` items first), and the preview re-renders the truncated object before Send — the wire bytes are always the previewed bytes |
| Mid-call cancel | Abort the request/kill the CLI child; nothing partial is stored |
| Cloud suggests an unresumable action | Connector validation fails ⇒ shown as text-only advice, no action button |
