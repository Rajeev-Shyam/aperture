//! Publish the overlay's interactive regions to the core (doc 11 §2).
//
//  The overlay window is click-through; the core re-enables input only while
//  the cursor is inside one of the rects published here (the Rust cursor
//  poller). Everything that opts into pointer events via `.surface-interactive`
//  — bubbles, the voice surfaces, the capture indicator, the privacy button —
//  is measured; the portalled bubble overflow menu is included explicitly since
//  it renders outside the stack.
//
//  Publishing is diff-gated (stringified compare) so the IPC only fires when the
//  layout actually changed. Latency layers (decision #11):
//    - MutationObserver → rAF: mount/unmount and class/style flips publish on
//      the next frame;
//    - animation/transition start+end events: bubbles move via transform
//      animations that mutate no attributes, so their settled rects publish the
//      moment the animation finishes instead of waiting out the interval;
//    - the interval is the last-resort net for MID-flight drift the events
//      bracket but don't cover; 100 ms keeps a moving rect at most one bubble
//      edge stale.
//  The Rust side reconciles immediately on every publish (no poll-tick wait).
//
//  The interval runs ONLY while the last published set is non-empty (SDLC
//  review 2026-08-19 finding 6). With nothing interactive on screen — the
//  common idle case — a 10 Hz querySelectorAll + getBoundingClientRect sweep in
//  every monitor's WebView forces layout forever to learn nothing, and that
//  per-monitor wakeup was never budgeted against doc 04 §8's <2 % idle CPU.
//  Mount/unmount and motion still publish through the observer + event layers,
//  which are what bring the first rect back; the interval re-arms on that
//  publish and stops again when a measurement publishes `[]`.

import { useEffect } from "react";

import { setHitTestRects } from "../lib/ipc";

const SELECTOR = ".surface-interactive, .bubble__overflow";
const REMEASURE_MS = 100;

/** Animation boundaries that settle/relocate rects without touching the DOM
 *  tree — remeasure immediately instead of waiting for the interval. */
const MOTION_EVENTS = [
  "animationstart",
  "animationend",
  "transitionstart",
  "transitionend",
] as const;

export function useHitTestRects(): void {
  useEffect(() => {
    let last = "";
    let raf = 0;
    // The drift net, armed only while something is on screen (finding 6).
    let interval: ReturnType<typeof setInterval> | null = null;

    const measure = () => {
      const dpr = window.devicePixelRatio || 1;
      const rects = Array.from(document.querySelectorAll<HTMLElement>(SELECTOR))
        .map((el) => el.getBoundingClientRect())
        .filter((r) => r.width > 0 && r.height > 0)
        .map((r) => ({
          x: Math.floor(r.left * dpr),
          y: Math.floor(r.top * dpr),
          width: Math.ceil(r.width * dpr),
          height: Math.ceil(r.height * dpr),
        }));
      const key = JSON.stringify(rects);
      if (key === last) return;
      last = key;
      if (rects.length === 0) {
        if (interval !== null) clearInterval(interval);
        interval = null;
      } else if (interval === null) {
        interval = setInterval(measure, REMEASURE_MS);
      }
      void setHitTestRects(rects).catch((e) =>
        console.error("set_hit_test_rects failed; surfaces may be unclickable", e),
      );
    };

    const schedule = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(measure);
    };

    schedule();
    const observer = new MutationObserver(schedule);
    observer.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["class", "style"],
    });
    // schedule() coalesces via rAF and measure() diff-gates, so per-property
    // event chatter costs one extra compare, not extra IPC.
    MOTION_EVENTS.forEach((name) => document.addEventListener(name, schedule));

    return () => {
      cancelAnimationFrame(raf);
      if (interval !== null) clearInterval(interval);
      observer.disconnect();
      MOTION_EVENTS.forEach((name) => document.removeEventListener(name, schedule));
      // Unmounting the overlay root means nothing is interactive anymore.
      void setHitTestRects([]).catch(() => {});
    };
  }, []);
}
