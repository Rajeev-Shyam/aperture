//! The HUD orb's snap + layout rules are pure (hudLayout.ts); pin them here.
//  Nothing imports `@tauri-apps/*`.

import { describe, expect, it } from "vitest";

import { ANCHORS, anchorClasses, anchorStyle, nearestAnchor } from "./hudLayout";

const W = 3000;
const H = 1500;

describe("nearestAnchor", () => {
  it("maps the viewport thirds to the 8 anchors", () => {
    expect(nearestAnchor(100, 100, W, H, "top-right")).toBe("top-left");
    expect(nearestAnchor(1500, 100, W, H, "top-right")).toBe("top-center");
    expect(nearestAnchor(2900, 100, W, H, "top-left")).toBe("top-right");
    expect(nearestAnchor(100, 750, W, H, "top-right")).toBe("center-left");
    expect(nearestAnchor(2900, 750, W, H, "top-right")).toBe("center-right");
    expect(nearestAnchor(100, 1400, W, H, "top-right")).toBe("bottom-left");
    expect(nearestAnchor(1500, 1400, W, H, "top-right")).toBe("bottom-center");
    expect(nearestAnchor(2900, 1400, W, H, "top-left")).toBe("bottom-right");
  });

  it("keeps the current anchor for a drop in the dead middle", () => {
    expect(nearestAnchor(1500, 750, W, H, "bottom-left")).toBe("bottom-left");
  });

  it("always returns a known anchor", () => {
    for (let x = 0; x <= W; x += 250) {
      for (let y = 0; y <= H; y += 125) {
        expect(ANCHORS).toContain(nearestAnchor(x, y, W, H, "top-right"));
      }
    }
  });
});

describe("anchorClasses", () => {
  it("splits the anchor into a column and a row modifier", () => {
    expect(anchorClasses("top-right")).toBe("hud--col-right hud--row-top");
    expect(anchorClasses("center-left")).toBe("hud--col-left hud--row-center");
    expect(anchorClasses("bottom-center")).toBe("hud--col-center hud--row-bottom");
  });
});

describe("anchorStyle", () => {
  it("pins corners with two insets and nothing else", () => {
    const s = anchorStyle("bottom-right");
    expect(s.bottom).toBeDefined();
    expect(s.right).toBeDefined();
    expect(s.top).toBeUndefined();
    expect(s.left).toBeUndefined();
    expect(s.transform).toBeUndefined();
  });

  it("centres edge midpoints with a translate", () => {
    expect(anchorStyle("top-center").transform).toBe("translateX(-50%)");
    expect(anchorStyle("center-right").transform).toBe("translateY(-50%)");
  });

  it("covers every anchor", () => {
    for (const a of ANCHORS) expect(Object.keys(anchorStyle(a)).length).toBeGreaterThan(0);
  });
});
