-- Aperture initial schema (authoritative DDL — doc 03 §3).
--
-- Storage: SQLite, WAL mode, single file %LOCALAPPDATA%\Aperture\history.db.
-- One file => the "never leaves the device" boundary is auditable, backupable,
-- purgeable. sqlite-vec provides KNN over embeddings (vec0 virtual tables).
-- At-rest encryption (SQLCipher-style page encryption, key in Windows DPAPI /
-- Credential Manager) is applied at connection time (doc 13 §6) — not here.
--
-- Single-writer rule: only the Tier-0 pipeline writes (doc 02 §7, doc 03 §1).

PRAGMA journal_mode = WAL;

CREATE TABLE schema_migrations (
  version    INTEGER PRIMARY KEY,
  applied_ts INTEGER
);

CREATE TABLE events (
  id              INTEGER PRIMARY KEY,
  ts              INTEGER NOT NULL,            -- epoch ms
  type            TEXT    NOT NULL,            -- taxonomy, doc 03 §2
  app             TEXT,
  process         TEXT,
  window_title    TEXT,
  payload         TEXT,                        -- JSON: type-specific fields
  connector_id    TEXT REFERENCES connector_state(id),
  session_id      INTEGER,                     -- assigned by the sessionizer (doc 08)
  redaction_flags INTEGER DEFAULT 0
);
CREATE INDEX idx_events_ts       ON events(ts);
CREATE INDEX idx_events_type_ts  ON events(type, ts);
CREATE INDEX idx_events_session  ON events(session_id);

CREATE TABLE screen_context (
  id             INTEGER PRIMARY KEY,
  event_id       INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
  ocr_text       TEXT,                         -- cheap always-on OCR (post-exclusion)
  ocr_confidence REAL,
  vlm_summary    TEXT,                         -- only if the VLM was invoked (doc 06)
  thumb_phash    TEXT                          -- perceptual hash; RAW FRAMES ARE NOT STORED
);
CREATE INDEX idx_ctx_event ON screen_context(event_id);

-- 768 dims pinned to nomic-embed-text-v1.5 (137M, Matryoshka 64..768). Confirmed.
-- NOTE: requires the sqlite-vec extension to be loaded on the connection.
CREATE VIRTUAL TABLE ctx_vec USING vec0(
  event_id  INTEGER PRIMARY KEY,
  embedding float[768]
);

CREATE TABLE patterns (
  id              INTEGER PRIMARY KEY,
  signature       TEXT UNIQUE,                 -- normalized token n-gram (doc 08)
  n               INTEGER,                     -- gram length 2..4
  support         INTEGER DEFAULT 0,
  confidence      REAL    DEFAULT 0,           -- recency-weighted P(next|prefix)
  last_seen       INTEGER,
  dismiss_decay   REAL    DEFAULT 1.0,         -- multiplied down on dismissals
  action_template TEXT                         -- JSON: how to form the suggestion
);

CREATE TABLE connector_state (
  id                  TEXT PRIMARY KEY,        -- uuid
  connector_type      TEXT NOT NULL,           -- 'browser'|'youtube'|'document'|'ide'
  reconstruct_payload TEXT NOT NULL,           -- versioned JSON (doc 10 per-type schemas)
  payload_version     INTEGER DEFAULT 1,
  captured_ts         INTEGER NOT NULL,
  stale_after_ts      INTEGER                  -- per-connector TTL (doc 10)
);
CREATE INDEX idx_conn_type_ts ON connector_state(connector_type, captured_ts);

CREATE TABLE suggestions (
  id           INTEGER PRIMARY KEY,
  pattern_id   INTEGER REFERENCES patterns(id),
  connector_id TEXT REFERENCES connector_state(id),
  source       TEXT NOT NULL,                  -- 'local' | 'claude'
  title        TEXT,
  glyph        TEXT,
  confidence   REAL,
  state        TEXT,                           -- queued|shown|clicked|dismissed|expired
  shown_ts     INTEGER,
  resolved_ts  INTEGER,
  outcome      TEXT
);

CREATE TABLE exclusion_list (
  id         INTEGER PRIMARY KEY,
  match_kind TEXT,                             -- 'process'|'window_class'|'title_regex'
  pattern    TEXT,
  enabled    INTEGER DEFAULT 1
);

CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT
);
