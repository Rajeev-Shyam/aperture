//! The "why am I not seeing bubbles?" verdict (doc 11 §6 diagnostics,
//  2026-09-06) is pure, so its ordering — capture, engine, events, habits at
//  the floor, gate, top rejection, success — is pinned here. Nothing imports
//  `@tauri-apps/*`.

import { describe, expect, it } from "vitest";

import type { Diagnostics, EngineSnapshot, RejectRow } from "../lib/ipc";
import { diagnose, topRejection } from "./diagnosis";

function rows(counts: Partial<Record<string, number>> = {}): RejectRow[] {
  return [
    "not confident enough",
    "not repeated enough yet",
    "nothing fresh to resume",
    "shown too recently",
    "hourly budget spent",
    "you were just there",
    "capture was off",
  ].map((label) => ({ label, hint: `hint for ${label}`, count: counts[label] ?? 0 }));
}

function engine(over: Partial<EngineSnapshot> = {}): EngineSnapshot {
  return {
    capture_on: true,
    tau_conf: 0.4,
    support_floor: 3,
    cap_per_hour: 4,
    patterns_cached: 120,
    patterns_at_floor: 9,
    current_session: 41,
    gate: {
      evaluated: 0,
      admitted: 0,
      rejected: [0, 0, 0, 0, 0, 0, 0],
      last_admitted: null,
      last_rejected: null,
      closest_miss: null,
    },
    rejected_by_reason: rows(),
    ...over,
  };
}

function diag(over: Partial<Diagnostics> = {}): Diagnostics {
  return {
    engine: engine(),
    capture_enabled: true,
    extension_hosts_connected: 1,
    events_24h: 300,
    navigation_24h: 20,
    document_ide_24h: 3,
    connector_states_fresh: 4,
    suggestions_24h: 0,
    last_suggestion_ts: null,
    now_ms: 1_000_000,
    ...over,
  };
}

describe("diagnose", () => {
  it("capture off wins over everything else", () => {
    const d = diagnose(diag({ capture_enabled: false, events_24h: 0 }));
    expect(d.verdict).toMatch(/Capture is off/);
  });

  it("no engine snapshot means no event since launch", () => {
    expect(diagnose(diag({ engine: null })).verdict).toMatch(/not processed an event/);
  });

  it("no events in 24 h is named before habit counts", () => {
    expect(diagnose(diag({ events_24h: 0 })).verdict).toMatch(/No app switches/);
  });

  it("habits below the repeat floor explain the wait with the floor value", () => {
    const d = diagnose(diag({ engine: engine({ patterns_at_floor: 0, patterns_cached: 57 }) }));
    expect(d.verdict).toMatch(/repeated 3×/);
    expect(d.verdict).toMatch(/57 candidate habits/);
  });

  it("all-rejected names the top rejection and its hint", () => {
    const e = engine({
      gate: {
        evaluated: 40,
        admitted: 0,
        rejected: [5, 0, 0, 0, 0, 35, 0],
        last_admitted: null,
        last_rejected: null,
        closest_miss: null,
      },
      rejected_by_reason: rows({ "not confident enough": 5, "you were just there": 35 }),
    });
    const d = diagnose(diag({ engine: e }));
    expect(d.verdict).toMatch(/40 candidates/);
    expect(d.verdict).toMatch(/you were just there — hint for you were just there/);
  });

  it("admitted bubbles report the count and the last time", () => {
    const e = engine({
      gate: {
        evaluated: 40,
        admitted: 3,
        rejected: [37, 0, 0, 0, 0, 0, 0],
        last_admitted: { signature: "a ⇒ b", score: 0.6, reject: null, at_ms: 1_000 },
        last_rejected: null,
        closest_miss: null,
      },
    });
    const d = diagnose(diag({ engine: e, suggestions_24h: 2 }));
    expect(d.verdict).toMatch(/3 bubbles were admitted/);
    expect(d.verdict).toMatch(/2 in the last 24 hours/);
    expect(d.verdict).toMatch(/The last one was at/);
  });

  it("notes mention the extension and document capture only when absent", () => {
    const withBoth = diagnose(diag());
    expect(withBoth.notes).toHaveLength(0);
    const without = diagnose(diag({ extension_hosts_connected: 0, document_ide_24h: 0 }));
    expect(without.notes).toHaveLength(2);
    expect(without.notes[0]).toMatch(/extension is not connected/);
    expect(without.notes[1]).toMatch(/No document or IDE state/);
  });
});

describe("topRejection", () => {
  it("returns the largest non-zero row, or null", () => {
    expect(topRejection(rows())).toBeNull();
    expect(topRejection(rows({ "shown too recently": 2, "you were just there": 9 }))?.label).toBe(
      "you were just there",
    );
  });
});
