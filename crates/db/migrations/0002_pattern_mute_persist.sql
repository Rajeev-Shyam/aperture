-- CONN-M2: persist the mute half of the decay/mute ladder (doc 08 §7, ADR-033).
--
-- 0001 persisted `dismiss_decay`, but the MuteState (muted-until + the trailing
-- trip-wire dismissals) lived only in RAM. So after a restart the engine re-mined
-- cold: a thrice-dismissed (muted) suggestion lost its mute and re-nagged, and the
-- next flush clobbered the saved decay. These two columns let `PatternEngine::hydrate`
-- restore the full ladder at startup so a dismissed/muted suggestion stays quiet.
--
-- Both nullable with no default: a NULL `muted_until` means "not muted"; a NULL
-- `recent_dismissals` means "no dismissals in the trailing window" (decoded to an
-- empty list). Forward-only, additive — existing rows read back as never-muted.

ALTER TABLE patterns ADD COLUMN muted_until INTEGER;
ALTER TABLE patterns ADD COLUMN recent_dismissals TEXT;
