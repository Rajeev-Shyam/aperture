# Doc 14 — Design System Spec: Chromemorphism & Liquid Meta

## 1. Principles
Chrome and liquid glass as a **documented system**: a dark canvas world where surfaces are thin, lit, and refractive; restraint over spectacle; and **performance is a design token** — this UI shares one RTX 5060 with inference, so every effect has a cost class and a fallback.

## 2. Design tokens
**Color**
| Token | Value | Use |
|---|---|---|
| `canvas/0` | `#0B0C0E` | conceptual base behind glass |
| `glass/1..3` | `rgba(255,255,255,0.06 / 0.10 / 0.15)` | surface fills — **rgba alpha, never `opacity`** (text stays crisp) |
| `border/hairline` | `rgba(255,255,255,0.08)` → `0.20` on hover | the chrome edge |
| `ink/primary` | `rgba(255,255,255,0.92)` | text (APCA-checked over worst-case backdrops) |
| `accent` | OKLCH ramp around a cooler **steel / silver-blue** hue [VERIFY tooling — exact OKLCH values pending; a cooler, lower-chroma tone than the former chrome-cyan] | actions, focus |
**Geometry & elevation:** radii 12/16/24 px (bubble = 16); shadow `0 8px 24px rgba(0,0,0,0.35)`; inner top highlight `inset 0 1px 0 rgba(255,255,255,0.12)` (the "lens" lip).
**Blur scale:** 8 / 12 px — **12 is the ceiling** (ADR-039); beyond ~12–16 px the backdrop smears and the GPU copy-blur-paste cost climbs for no legibility gain on a card shared with inference.
**Motion:** durations 120/180/240 ms; easing `cubic-bezier(0.2, 0.8, 0.2, 1)`; **animatable properties: `opacity` and `transform` only.**

## 3. Effect recipes
**Bubble (the core component):**
```css
.bubble {
  background: var(--glass-2);                      /* rgba(255,255,255,0.10) */
  backdrop-filter: blur(12px) saturate(160%);
  border: 1px solid var(--border-hairline);
  border-radius: 16px;
  box-shadow: 0 8px 24px rgba(0,0,0,.35), inset 0 1px 0 rgba(255,255,255,.12);
  contain: strict;                                  /* bound rasterization */
}
.bubble::before {                                   /* specular sweep */
  background: linear-gradient(115deg, rgba(255,255,255,.18) 0%, transparent 35%);
}
@supports not (backdrop-filter: blur(1px)) {
  .bubble { background: rgba(22,24,28,0.95); }      /* opaque fallback */
}
```
**Liquid refraction (deferred post-v1 — ADR-039):** **v1 ships static glass only** (specular sweep + lens-lip highlight + blur carry "Liquid Meta"); this refraction recipe is **retained as a documented future enhancement**, not shipped in v1. Recipe (for post-v1): SVG `feDisplacementMap` warp + `feSpecularLighting` with the single global light direction (top-left ~115°) — compositor-thread only, applied to at most one "hero" surface, auto-disabled under `gpu_busy` and on `prefers-reduced-motion`.

## 4. Bubble component states
| State | Treatment (animating only opacity/transform) |
|---|---|
| enter | scale .96→1 + fade, 180 ms |
| idle | static glass; **`backdrop-filter` is never animated** |
| hover | "lift" = background `glass/2→glass/3` + border to 0.20 (paint-only, cheap) |
| active | scale .98, 120 ms |
| exit | fade + 4 px translate-down, 180 ms |

## 5. Performance budget & fallbacks (the contract with Doc 12)
- **Hard cap (interim, ADR-039): ≤ 2 concurrent glass (backdrop-filter) surfaces + an opaque 3rd bubble** — every glass surface forces a copy→blur→composite of the backdrop region. With 3 bubbles visible (the UX cap, Doc 11 §3) the 3rd renders in the `--fallback-opaque` class (2 glass + 1 opaque = 3 visible). The **final cap is set at the M8 PresentMon test.**
- **Degrade-under-load contract:** while the GPU mutex is held (`gpu_busy=true`), the overlay swaps glass → `--fallback-opaque` (rgba .95, no blur), disables refraction, and reduces motion to fades; restore on release. *The design system is wired to the resource manager — a design force, not vibes.*
- `prefers-reduced-motion` ⇒ same fallback class + minimal motion. Low-contrast backdrops ⇒ `ink` gains a 1 px text-shadow (APCA floor).
- **Verification (M8 gate):** PresentMon shows no overlay frame drops while a VLM job runs; glass↔fallback swap is visually clean; the M8 test also **sets the final glass-surface cap.** [VERIFY]

---
> **R2 amendments applied** (see docs/19–21): ADR-039 · Q68, Q80.
