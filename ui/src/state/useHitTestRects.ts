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
//  layout actually changed. A 250 ms interval catches CSS-animation drift that
//  MutationObserver cannot see (transform animations mutate no attributes);
//  the observer makes mount/unmount latency imperceptible.

import { useEffect } from "react";

import { setHitTestRects } from "../lib/ipc";

const SELECTOR = ".surface-interactive, .bubble__overflow";
const REMEASURE_MS = 250;

export function useHitTestRects(): void {
  useEffect(() => {
    let last = "";
    let raf = 0;

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
      void setHitTestRects(rects).catch((e) =>
        console.error("set_hit_test_rects failed; surfaces may be unclickable", e),
      );
    };

    const schedule = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(measure);
    };

    schedule();
    const interval = setInterval(measure, REMEASURE_MS);
    const observer = new MutationObserver(schedule);
    observer.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["class", "style"],
    });

    return () => {
      cancelAnimationFrame(raf);
      clearInterval(interval);
      observer.disconnect();
      // Unmounting the overlay root means nothing is interactive anymore.
      void setHitTestRects([]).catch(() => {});
    };
  }, []);
}
