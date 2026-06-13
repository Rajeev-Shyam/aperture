# Doc 11 — Bubble UI / Overlay

## 1. Interface
| | |
|---|---|
| **Inputs** | `BubbleSpec`s (Doc 08), voice surfaces (Doc 07), preview requests + payloads (Docs 03/13), `gpu_busy` signal (Doc 12), design tokens (Doc 14) |
| **Outputs** | Rendered overlay; `suggestion_shown/clicked/dismissed` feedback events; approved-payload Send to the gateway |
| **Resource cost** | WebView2 RAM (within the shell line item, Doc 04 §7); GPU **compositing only**, bounded by Doc 14's budget |

## 2. Overlay windowing (Tauri)
- One transparent, **always-on-top**, taskbar-skipped window per monitor [ASSUMPTION: primary-monitor-only in v1, multi-monitor at M8].
- **Click-through by default** (`WS_EX_LAYERED|WS_EX_TRANSPARENT`); hit-testing is re-enabled only over live bubble rects so the overlay never steals input from the user's work. Focus is never grabbed; bubbles are activated by mouse, or by an optional summon hotkey [ASSUMPTION].
- `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` — bubbles never enter our own capture (Doc 05 §2).

## 3. Bubble anatomy & lifecycle
Anatomy: glyph (connector type) · title · sublabel (e.g. "12:34 · 2h ago") · primary action (Resume) · dismiss (×) · overflow (⋯ → "Ask Claude about this", "Mute this pattern").
```
queued ─► entering(180ms) ─► idle(dwell 12s, hover pauses) ─► clicked ─► resolving ─► exit
   ▲            [ASSUMPTION]        │            │
   └── overflow if >3 visible       ├─ dismissed ─► exit (feedback → Doc 08 §7)
       (Doc 14 hard cap)            └─ expired   ─► exit (mild decay)
```
- Max **3** concurrent visible bubbles (the Doc 14 performance cap is also the UX cap); excess stays `queued`, lowest score dropped first.
- Placement: bottom-right vertical stack, 16 px gutter, newest on top [ASSUMPTION].
- Every transition writes the corresponding `suggestion_*` event (SC7's data source).

## 4. The Context-Preview + Enrichment panel (G7 — the trust surface)
Opened by any "Ask Claude" affordance. Renders the **actual serialized Context Payload** (Doc 03 §4) — not a summary of it:
1. **Intent** line (editable preset).
2. **Items list** — each item typed with kind icon, full content expandable, and a per-item **remove** (×). What you see is what ships.
3. **Redaction report** — every applied rule with count (e.g. `email ×2`, `window_excluded: 1Password`), from Doc 13's pipeline.
4. **Enrichment affordances** ("make context richer"): *Add current selection* · *Add screen summary* (may trigger a local VLM job, Doc 06) · *Add more history* (time-range slider extends `event_trail`) · *Add screenshot* (opt-in; shows the downscaled image and its token estimate, Doc 09 §5) · free-text addition.
5. **Footer:** transport target + health dot · payload size/token estimate · **Cancel** / **Send**.
Invariant: edits mutate the payload object; the panel re-renders from that object; **Send transmits those exact bytes** (hash logged, Doc 03 §4).

## 5. Voice surfaces (Doc 07's UI)
- **Listening pill** while PTT is held (waveform, release-to-stop).
- **Transcript chip** on low confidence ("Did you say …?" → Run / Dismiss).
- **Answer bubble**: text answer + source line ("from your history, yesterday 14:02") + optional **Resume** + **Ask Claude** (gated). Honest empty state when retrieval finds nothing.

## 6. Degrade-under-load hook (the Doc 14 contract, wired)
The overlay subscribes to `gpu_busy` (mutex held — Doc 12). While true: glass surfaces swap to the opaque fallback class, entering/exit animations simplify to fades, and no new blur surfaces are created. On release, glass restores. This keeps inference bandwidth and frame pacing honest on the shared 5060.

## 7. Failure modes
| Failure | Behavior |
|---|---|
| Overlay covering a critical app area | Bubbles are draggable; position persists; per-app "don't overlay" honors the exclusion list |
| WebView2 crash | Tauri respawns the overlay; queued suggestions survive in SQLite |
| DPI / monitor change | Re-anchor on `WM_DPICHANGED` / display-change events |
| Click-through misconfiguration | Watchdog: if hit-test region exists with no visible bubble, reset to full click-through |
