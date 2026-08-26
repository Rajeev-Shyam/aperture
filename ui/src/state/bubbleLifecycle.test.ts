//! Behavioural tests for the pure bubble-admission helpers (SDLC review
//  2026-08-19 finding 7). Ported one-for-one from the scratch node script that
//  verified the batch-4 fixes — they cover exactly the two regressions that
//  `tsc --noEmit` could not: the visible-cap off-by-one in `admit` and a freed
//  slot that was never re-filled. Pure modules only: nothing here may import
//  `@tauri-apps/*`, and `bubbleLifecycle.ts` must stay free of it too.

import { describe, expect, it } from "vitest";

import type { BubbleSpec, Settings, UiSettings } from "../lib/ipc";
import {
  DEFAULTS,
  admissionScore,
  admit,
  promote,
  tuningFromSettings,
  type BubbleInstance,
} from "./bubbleLifecycle";
import { DEFAULT_GLASS_CAP, isOpaqueForBudget } from "./glassBudget";

const spec = (confidence: number, created_ts: number | null): BubbleSpec => ({
  title: "t",
  glyph: "globe",
  sublabel: null,
  action_ref: "a",
  source: "local",
  confidence,
  created_ts,
});

const NOW = 1_000_000_000;
const HL = 10 * 60_000;

const visible = (list: BubbleInstance[]) => list.filter((b) => b.state !== "queued");
const byId = (list: BubbleInstance[], id: string) => {
  const b = list.find((x) => x.id === id);
  if (!b) throw new Error(`no bubble ${id}`);
  return b;
};

describe("admissionScore (decision #5: freshness × confidence)", () => {
  it("a fresh bubble scores its confidence", () => {
    expect(admissionScore(spec(0.8, NOW), NOW, HL)).toBeCloseTo(0.8, 9);
  });

  it("one half-life old halves the score", () => {
    expect(admissionScore(spec(0.8, NOW - HL), NOW, HL)).toBeCloseTo(0.4, 9);
  });

  it("an unknown created_ts is not penalised", () => {
    expect(admissionScore(spec(0.8, null), NOW, HL)).toBe(0.8);
  });

  it("a future created_ts cannot exceed confidence", () => {
    expect(admissionScore(spec(0.8, NOW + 60_000), NOW, HL)).toBe(0.8);
  });

  it("stale-but-confident loses to fresh-but-less-confident", () => {
    expect(admissionScore(spec(0.95, NOW - HL), NOW, HL)).toBeLessThan(
      admissionScore(spec(0.7, NOW), NOW, HL),
    );
  });
});

/** Three arrivals into an empty list at `NOW`, cap 3. */
function threeVisible(): BubbleInstance[] {
  let list: BubbleInstance[] = [];
  for (let i = 0; i < 3; i++) {
    list = admit(list, { id: `v${i}`, spec: spec(0.5, NOW) }, 3, NOW, HL);
  }
  return list;
}

describe("admit: the ≤maxVisible cap (doc 11 §3, doc 14 §5)", () => {
  it("three arrivals fill three slots", () => {
    expect(visible(threeVisible())).toHaveLength(3);
  });

  // The old bug: a high-scoring arrival while full produced a 4th visible bubble.
  it("a 4th arrival cannot break the visible cap", () => {
    const list = admit(threeVisible(), { id: "hot", spec: spec(0.99, NOW) }, 3, NOW, HL);
    expect(visible(list)).toHaveLength(3);
  });

  it("...and waits in the queue instead", () => {
    const list = admit(threeVisible(), { id: "hot", spec: spec(0.99, NOW) }, 3, NOW, HL);
    expect(byId(list, "hot").state).toBe("queued");
  });
});

/** Three visible + "hot" (0.99) and "cold" (0.6) queued. */
function fullWithQueue(): BubbleInstance[] {
  let list = admit(threeVisible(), { id: "hot", spec: spec(0.99, NOW) }, 3, NOW, HL);
  list = admit(list, { id: "cold", spec: spec(0.6, NOW) }, 3, NOW, HL);
  return list;
}

describe("promote: freed-slot ordering", () => {
  it("the freed slot goes to the highest-scoring queued bubble", () => {
    const freed = promote(fullWithQueue().filter((b) => b.id !== "v0"), 3);
    expect(byId(freed, "hot").state).toBe("entering");
    expect(byId(freed, "cold").state).toBe("queued");
  });

  it("only ONE slot is filled by one departure", () => {
    const freed = promote(fullWithQueue().filter((b) => b.id !== "v0"), 3);
    expect(visible(freed)).toHaveLength(3);
  });

  // Ageing flips the ranking: re-admitting later re-scores the queue.
  it("an aged queued bubble yields its slot to a fresh arrival (decision #5)", () => {
    const LATER = NOW + 40 * 60_000; // 'hot' is now 4 half-lives old
    const aged = admit(fullWithQueue(), { id: "new", spec: spec(0.7, LATER) }, 3, LATER, HL);
    const agedFreed = promote(aged.filter((b) => b.id !== "v0"), 3);
    expect(byId(agedFreed, "new").state).toBe("entering");
    expect(byId(agedFreed, "hot").state).toBe("queued");
  });

  it("a lowered cap does not yank a visible bubble off screen", () => {
    const freed = promote(fullWithQueue().filter((b) => b.id !== "v0"), 3);
    expect(visible(promote(freed, 1))).toHaveLength(3);
  });
});

/** A settings read with only some `ui` keys present — what a pre-upgrade DB
 *  (or a hand-edited seed) really returns; the resolver must tolerate it. */
const withUi = (ui: Partial<UiSettings>): Settings => ({ ui: ui as UiSettings });

describe("tuningFromSettings: clamps (decision #7)", () => {
  it("absent settings give the shipped defaults", () => {
    expect(tuningFromSettings(null).dwellMs).toBe(DEFAULTS.dwellMs);
  });

  it("a valid dwell is applied", () => {
    expect(tuningFromSettings(withUi({ bubble_dwell_sec: 45 })).dwellMs).toBe(45_000);
  });

  it("an out-of-range dwell falls back rather than breaking the bubble", () => {
    expect(tuningFromSettings(withUi({ bubble_dwell_sec: 0 })).dwellMs).toBe(DEFAULTS.dwellMs);
  });

  it("a glass cap of 0 is honoured (every surface opaque)", () => {
    expect(tuningFromSettings(withUi({ max_glass_surfaces: 0 })).glassCap).toBe(0);
  });
});

describe("glassBudget (ADR-039, doc 14 §5)", () => {
  it("the default cap renders the 3rd visible bubble opaque", () => {
    expect(DEFAULT_GLASS_CAP).toBe(2);
    expect(isOpaqueForBudget(0)).toBe(false);
    expect(isOpaqueForBudget(1)).toBe(false);
    expect(isOpaqueForBudget(2)).toBe(true);
  });

  it("a cap of 0 makes every surface opaque", () => {
    expect(isOpaqueForBudget(0, 0)).toBe(true);
  });

  it("a negative cap is clamped to 0, not treated as unlimited", () => {
    expect(isOpaqueForBudget(0, -1)).toBe(true);
  });
});
