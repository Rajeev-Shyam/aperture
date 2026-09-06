//! The one-paragraph answer to "why am I not seeing bubbles?" (doc 11 §6
//  diagnostics, 2026-09-06). Pure: turns a `Diagnostics` snapshot into a
//  verdict line plus notes, checked in the order the seven trigger rules and
//  their inputs actually fail — capture, events, habits, the gate, then what
//  the gate rejected most. Rendered by the Dashboard's Advanced tab.

import type { Diagnostics, RejectRow } from "../lib/ipc";

export interface Diagnosis {
  /** The single sentence that explains the current state. */
  verdict: string;
  /** Secondary facts worth knowing, each one sentence. */
  notes: string[];
}

/** The rule that rejected the most candidates, or `null` when nothing was rejected. */
export function topRejection(rows: RejectRow[]): RejectRow | null {
  let best: RejectRow | null = null;
  for (const row of rows) {
    if (row.count > 0 && (best === null || row.count > best.count)) best = row;
  }
  return best;
}

export function diagnose(d: Diagnostics): Diagnosis {
  const notes: string[] = [];
  if (d.extension_hosts_connected === 0) {
    notes.push(
      "The browser extension is not connected, so pages cannot be resumed; app-switch habits still work. Load it from the Aperture install folder (extension/) in your browser.",
    );
  }
  if (d.document_ide_24h === 0) {
    notes.push(
      "No document or IDE state was captured in the last 24 hours — Office titles resolve through Recent Items; VS Code through its window title.",
    );
  }

  if (!d.capture_enabled) {
    return { verdict: "Capture is off. Nothing is learned or suggested while it is off.", notes };
  }
  const e = d.engine;
  if (!e) {
    return {
      verdict: "The habit engine has not processed an event since launch yet.",
      notes,
    };
  }
  if (!e.capture_on) {
    return {
      verdict:
        "Capture is on, but the engine has not been told yet — it wakes on the next toggle broadcast.",
      notes,
    };
  }
  if (d.events_24h === 0) {
    return {
      verdict: "No app switches were captured in the last 24 hours, so there is nothing to learn from.",
      notes,
    };
  }
  if (e.patterns_at_floor === 0) {
    return {
      verdict: `No sequence of apps has repeated ${e.support_floor}× yet (${e.patterns_cached} candidate habits are being tracked). Keep working normally; this fills in on its own.`,
      notes,
    };
  }
  if (e.gate.evaluated === 0) {
    return {
      verdict: `${e.patterns_at_floor} habits have repeated enough, but none matched what you were doing since launch.`,
      notes,
    };
  }
  if (e.gate.admitted === 0) {
    const top = topRejection(e.rejected_by_reason);
    const because = top ? ` Most often: ${top.label} — ${top.hint}` : "";
    return {
      verdict: `${e.gate.evaluated} candidates were judged since launch and every one was held back.${because}`,
      notes,
    };
  }
  const when = e.gate.last_admitted
    ? ` The last one was at ${new Date(e.gate.last_admitted.at_ms).toLocaleTimeString()}.`
    : "";
  return {
    verdict: `${e.gate.admitted} bubbles were admitted since launch (${d.suggestions_24h} in the last 24 hours).${when}`,
    notes,
  };
}
