-- 0004 — suggestions.created_ts (owner decision #5, Doc 24, 2026-08-16).
--
-- The overlay admits bubbles into its 3 visible slots by score. That score was
-- pure confidence, so a stale-but-confident queued bubble could outrank every
-- fresher one forever (the code's own comment called it a placeholder). The
-- freshness half needs a creation time the UI can trust across a WebView2
-- respawn, where the queue is rebuilt from THIS table rather than from the live
-- event stream.
--
-- `shown_ts` could not serve: it is NULL while a row is queued (snoozed), which
-- is exactly the case that needs an age. Additive + nullable, so every existing
-- row keeps working and a NULL honestly reads as "unknown age" (the UI then
-- scores on confidence alone — the pre-decision behavior).
ALTER TABLE suggestions ADD COLUMN created_ts INTEGER;

-- Backfill what is knowable: a row that was shown was created no later than
-- that. Rows that never surfaced stay NULL rather than being given a guess.
UPDATE suggestions SET created_ts = shown_ts WHERE created_ts IS NULL AND shown_ts IS NOT NULL;
