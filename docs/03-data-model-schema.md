# Doc 03 — Data Model & Schema

## 1. Storage engine & file layout
- **SQLite**, WAL mode, single file `%LOCALAPPDATA%\Aperture\history.db` (+`-wal`/`-shm`). One file ⇒ the "never leaves the device" boundary is auditable, backupable, and purgeable.
- **sqlite-vec** extension for KNN over embeddings (SIMD-accelerated `vec0` virtual tables).
- **At-rest encryption:** SQLCipher-style page encryption; key held in Windows DPAPI / Credential Manager. [VERIFY exact library + key-wrapping approach — Doc 13.]
- Writers: the Tier-0 pipeline only (single-writer). Readers: pattern engine, retrieval, payload builder, UI.

## 2. Event taxonomy
| type | Emitted by | Type-specific payload fields | Connector? |
|---|---|---|---|
| `window_focus` | WinEvent hook | `app, process, window_title, hwnd_hash` | maybe |
| `window_open` / `window_close` | WinEvent/UIA | same as above | maybe |
| `navigation` | UIA address-bar read | `url, browser` | browser/youtube |
| `media_state` | connector heuristics | `url, video_id, position_s?, state(play/pause)` | youtube |
| `document_state` | connector heuristics | `path, app` | document |
| `ide_state` | connector heuristics | `path, line?, col?, workspace?` | ide |
| `voice_utterance` | STT (07) | `transcript, duration_ms, stt_model, confidence, intent(query/telemetry)` | — |
| `suggestion_shown/clicked/dismissed` | Bubble UI (11) | `suggestion_id, outcome` | — |
| `capture_toggle` | Orchestrator (12) | `state(on/off), reason` | — (audit) |
| `cloud_send` | Gateway (09) | `payload_id, transport, bytes` | — (audit) |

## 3. DDL (authoritative)
```sql
PRAGMA journal_mode=WAL;

CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_ts INTEGER);

CREATE TABLE events (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,                 -- epoch ms
  type TEXT NOT NULL,                  -- taxonomy above
  app TEXT, process TEXT, window_title TEXT,
  payload TEXT,                        -- JSON: type-specific fields
  connector_id TEXT REFERENCES connector_state(id),
  session_id INTEGER,                  -- assigned by sessionizer (Doc 08)
  redaction_flags INTEGER DEFAULT 0
);
CREATE INDEX idx_events_ts ON events(ts);
CREATE INDEX idx_events_type_ts ON events(type, ts);
CREATE INDEX idx_events_session ON events(session_id);

CREATE TABLE screen_context (
  id INTEGER PRIMARY KEY,
  event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  ocr_text TEXT,                       -- cheap always-on OCR output (post-exclusion)
  ocr_confidence REAL,
  vlm_summary TEXT,                    -- only if the VLM was invoked (Doc 06)
  thumb_phash TEXT                     -- perceptual hash; RAW FRAMES ARE NOT STORED.
                                       -- Active consumer (ADR-032/Q72): a near-duplicate-frame gate
                                       -- (Doc 05 §4) skips OCR/embed when a new frame's pHash is within
                                       -- a Hamming threshold of the last. Threshold [ASSUMPTION], tuned at M2.
);
CREATE INDEX idx_ctx_event ON screen_context(event_id);

-- 768 dims pinned to nomic-embed-text-v1.5 (137M, Matryoshka 64..768). Confirmed dimension.
CREATE VIRTUAL TABLE ctx_vec USING vec0(
  event_id INTEGER PRIMARY KEY,
  embedding float[768]
);

CREATE TABLE patterns (
  id INTEGER PRIMARY KEY,
  signature TEXT UNIQUE,               -- normalized token n-gram (Doc 08)
  n INTEGER,                           -- gram length
  support INTEGER DEFAULT 0,
  confidence REAL DEFAULT 0,           -- recency-weighted P(next|prefix)
  last_seen INTEGER,
  dismiss_decay REAL DEFAULT 1.0,      -- multiplied down on dismissals
  action_template TEXT                 -- JSON: how to form the suggestion
);

CREATE TABLE connector_state (
  id TEXT PRIMARY KEY,                 -- uuid
  connector_type TEXT NOT NULL,        -- 'browser'|'youtube'|'document'|'ide'
  reconstruct_payload TEXT NOT NULL,   -- versioned JSON (Doc 10 per-type schemas)
  payload_version INTEGER DEFAULT 1,
  captured_ts INTEGER NOT NULL,
  stale_after_ts INTEGER               -- per-connector TTL (Doc 10)
);
CREATE INDEX idx_conn_type_ts ON connector_state(connector_type, captured_ts);

CREATE TABLE suggestions (
  id INTEGER PRIMARY KEY,
  pattern_id INTEGER REFERENCES patterns(id),
  connector_id TEXT REFERENCES connector_state(id),
  source TEXT NOT NULL,                -- 'local' | 'claude'
  title TEXT, glyph TEXT, confidence REAL,
  state TEXT,                          -- queued|shown|clicked|dismissed|expired
  shown_ts INTEGER, resolved_ts INTEGER, outcome TEXT,
  useful_rating TEXT                   -- 'up'|'down'|NULL: explicit "useful?" thumbs (Doc 08 §7, ADR-040/Q81)
);

CREATE TABLE exclusion_list (
  id INTEGER PRIMARY KEY,
  match_kind TEXT,                     -- 'process'|'window_class'|'title_regex'|'url_pattern' (ADR-040)
  pattern TEXT, enabled INTEGER DEFAULT 1
);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
```

## 4. Context-payload schema (previewed object == wire object)
The payload builder serializes **one** object; the preview panel renders it and the gateway transmits **those bytes**. JSON Schema (abridged):
```json
{
  "$id": "aperture/context-payload/v1",
  "type": "object",
  "required": ["payload_id","created_ts","intent","items","redactions","transport_target"],
  "properties": {
    "payload_id": {"type":"string","format":"uuid"},
    "created_ts": {"type":"integer"},
    "intent": {"enum":["summarize_current","answer_query","explain_pattern","custom"]},
    "items": {"type":"array","items":{"oneOf":[
      {"properties":{"kind":{"const":"ocr_text"},"source_event_id":{"type":"integer"},"text":{"type":"string"},"redacted":{"type":"boolean"}}},
      {"properties":{"kind":{"const":"event_trail"},"events":{"type":"array","maxItems":50}}},
      {"properties":{"kind":{"const":"connector"},"type":{"enum":["browser","youtube","document","ide"]},"payload":{"type":"object"}}},
      {"properties":{"kind":{"const":"screenshot"},"width":{"type":"integer"},"height":{"type":"integer"},"data_b64":{"type":"string"}}},
      {"properties":{"kind":{"const":"user_addition"},"text":{"type":"string"}}}
    ]}},
    "redactions": {"type":"array","items":{"properties":{"rule":{"type":"string"},"count":{"type":"integer"}}}},
    "enrichment_offered": {"type":"boolean"},
    "transport_target": {"enum":["claude-desktop-mcp","claude-cli","messages-api"]}
  }
}
```
Notes: `screenshot` items are **opt-in only** (user adds via enrichment; Doc 09 explains the token/caching cost of images). `event_trail` is capped at 50 events — **user-adjustable in the enrichment panel within that 50 cap** (ADR-040/Q71). The serialized payload's SHA-256 is logged in the `cloud_send` audit event.

## 5. Voice-query retrieval path over history
This same KNN+filter retrieval is **also invoked by the gated `aperture_search_history` MCP tool** (ADR-037, Doc 09 §3): Claude proposes a query, the tool handler runs the retrieval below, then **redacts + exclusion-filters + previews the matched results to the user before anything returns**, and audit-logs the return. Nothing leaves unseen.

1. Embed the transcript with the same nomic-embed model (768-d).
2. KNN + join + filter:
```sql
WITH knn AS (
  SELECT event_id, distance FROM ctx_vec
  WHERE embedding MATCH :query_vec AND k = 25
)
SELECT e.id, e.ts, e.type, e.window_title, e.payload, cs.connector_type, cs.reconstruct_payload,
       knn.distance
FROM knn JOIN events e ON e.id = knn.event_id
LEFT JOIN connector_state cs ON cs.id = e.connector_id
WHERE e.ts >= :recency_floor          -- e.g. now-7d unless the query names a time
ORDER BY knn.distance ASC;
```
3. Re-rank: `score = (1-dist_norm) * recency_decay(ts) * (resumable? 1.3 : 1.0)` [ASSUMPTION: weights tuned in M6]. Temporal phrases ("yesterday") set `recency_floor`/ceiling before KNN.
4. Top result → answer bubble (title, when, source) + resume action if a fresh `connector_state` exists. **No cloud unless the user escalates.**

## 6. Retention & lifecycle (defaults — user-configurable; Doc 13)
| Data | Default TTL | Purge mechanism |
|---|---|---|
| `events` + `ctx_vec` | 90 days [ASSUMPTION] | nightly job deletes by ts; vec rows cascade |
| `screen_context.ocr_text` | 30 days [ASSUMPTION] | nullify text, keep event skeleton |
| `voice_utterance` transcripts | 30 days [ASSUMPTION] | payload scrub |
| `connector_state` | per-connector TTL (Doc 10) | stale rows pruned |
| `suggestions`, `patterns` | 180 days | delete |
| Raw frames | **never persisted** | n/a |
One-click **Purge All** truncates every table and VACUUMs. `capture_toggle` and `cloud_send` audit rows survive purge for 30 days [ASSUMPTION: user accountability], then expire. Opt-in **diagnostics sends** are audited on the same footing (ADR-036) and surfaced in the Activity & Privacy view.

---
> **R2 amendments applied** (see docs/19–21): ADR-040/Q81 (`suggestions.useful_rating`), ADR-040 (`exclusion_list` `url_pattern`), ADR-032/Q72 (`thumb_phash` near-duplicate-frame gate), ADR-037 (retrieval SQL reused by gated `aperture_search_history`), ADR-040/Q71 (`event_trail` user-adjustable within the 50 cap), ADR-036 (diagnostics audited). Retention TTLs and 768-dim `ctx_vec` confirmed unchanged (Q73, Q2). Mirrored in `crates/db/migrations/0001_init.sql`.
