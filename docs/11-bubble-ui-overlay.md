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
Anatomy: glyph (connector type) · title · sublabel (e.g. "12:34 · 2h ago") · primary action (Resume) · dismiss (×) · an explicit **"useful?" thumbs** (👍/👎 — the SC7 signal, also feeding the Doc 08 §7 dismissal-decay loop) · overflow (⋯ → "Ask Claude about this", "Mute this pattern").
```
queued ─► entering(180ms) ─► idle(dwell 20s, hover pauses) ─► clicked ─► resolving ─► exit
   ▲            [ASSUMPTION]        │            │
   └── overflow if >3 visible       ├─ dismissed ─► exit (feedback → Doc 08 §7)
       (Doc 14 hard cap)            └─ expired   ─► exit (mild decay)
```
- Max **3** concurrent visible bubbles — under ADR-039/C4 that resolves to **≤2 glass surfaces + an opaque 3rd bubble** (the interim Doc 14 glass cap; final cap set at the M8 PresentMon test), and is simultaneously the UX cap; excess stays `queued`, lowest score dropped first.
- Placement: **user-configurable corner** (default **bottom-right**), 16 px gutter, newest on top; bubbles are **draggable and the position persists** [ASSUMPTION].
- Every transition writes the corresponding `suggestion_*` event (SC7's data source).

## 4. The Context-Preview + Enrichment panel (G7 — the trust surface)
Opened by any "Ask Claude" affordance. Renders the **actual serialized Context Payload** (Doc 03 §4) — not a summary of it:
1. **Intent** line (editable preset).
2. **Items list** — each item typed with kind icon, full content expandable, and a per-item **remove** (×). What you see is what ships.
3. **Redaction report** — every applied rule with count (e.g. `email ×2`, `window_excluded: 1Password`), from Doc 13's pipeline.
4. **Enrichment affordances** ("make context richer"): *Add current selection* · *Add screen summary* (may trigger a local VLM job, Doc 06) · *Add more history* (time-range slider extends `event_trail`, up to the 50-event cap — Doc 03 §4) · *Add screenshot* (opt-in; shows the downscaled image and its token estimate, Doc 09 §5) · free-text addition.
5. **Footer:** transport target + health dot · payload size/token estimate · **Cancel** / **Send**.
Invariant: edits mutate the payload object; the panel re-renders from that object; **Send transmits those exact bytes** (hash logged, Doc 03 §4).

**Scoped allow (ADR-026):** when the user has granted an app+intent scoped allow, the panel still renders this exact payload and shows a **cancel-window countdown** (default **3 s**, configurable) before the payload **auto-sends** — only the manual **Send** click is skipped; the payload display, cancel window, and `cloud_send` audit are not.

## 5. Voice surfaces (Doc 07's UI)
- **Listening pill** while PTT is held (waveform, release-to-stop); in **click-to-toggle** mode (Doc 07 §2) the pill is **clickable to stop** listening.
- **Transcript chip** on low confidence ("Did you say …?" → Run / Dismiss).
- **Answer bubble**: text answer + source line ("from your history, yesterday 14:02") + optional **Resume** + **Ask Claude** (gated). Honest empty state when retrieval finds nothing.

## 6. Global controls & privacy surfaces (ADR-040)
- **Global snooze** — quiets **all** bubbles for **15 min / 1 h / until re-enabled**. **Distinct from the capture toggle:** snooze silences *bubbles* while capture + pattern-learning continue; the toggle stops *everything*.
- **Activity & Privacy view** — a settings view rendering the audit log (`capture_toggle` + `cloud_send` + opt-in diagnostics sends): the concrete answer to "when was it watching / what left the machine" (Doc 13 §7).
- **Tiered settings** — a simple default view plus an **Advanced** panel for the proliferating tunables (TTLs, thresholds, cancel-window, hotkey, corner, dwell, diagnostics).

## 7. Degrade-under-load hook (the Doc 14 contract, wired)
The overlay subscribes to `gpu_busy` (mutex held — Doc 12). While true: glass surfaces swap to the opaque fallback class, entering/exit animations simplify to fades, and no new blur surfaces are created. On release, glass restores. This keeps inference bandwidth and frame pacing honest on the shared 5060.

## 8. Failure modes
| Failure | Behavior |
|---|---|
| Overlay covering a critical app area | Bubbles are draggable; position persists; per-app "don't overlay" honors the exclusion list |
| WebView2 crash | Tauri respawns the overlay; queued suggestions survive in SQLite |
| DPI / monitor change | Re-anchor on `WM_DPICHANGED` / display-change events |
| Click-through misconfiguration | Watchdog: if hit-test region exists with no visible bubble, reset to full click-through |

---
## Implementation status (2026-07-08) — M8 (software)

Overlay + surfaces in `ui/` + `src-tauri/src/overlay.rs`:
- Per-monitor overlays via `overlay::create_overlays` (primary reuses the config `overlay` window; others cloned + hardened click-through/capture-excluded). §2 click-through + `WDA_EXCLUDEFROMCAPTURE` are wired; DPI re-anchor (§7) is on-hardware.
- **§3 bubble overflow menu** is portalled to `<body>` (the `contain:strict` bubble was clipping it — the three actions were unreachable) and rendered as opaque chrome; it gained Escape-to-close + focus.
- **§4 Context-Preview panel** now backs its `aria-modal` with the real modal contract: focus-in on open, focus restored to the opener on close, Tab trap, Escape → Cancel (the zero-residue path).
- **§5 voice confirm chip** ("Did you say…?") is no longer a dead-end: Dismiss + Escape clear it and it takes focus on appear (Run's re-issue awaits a core command).
- Bubble concurrency cap + the glass budget are settings-driven (`ui.max_concurrent_bubbles`, `ui.max_glass_surfaces`).

Full session detail: `docs/handoff/session-bridge-2026-07-08-m6-m8.md`.

> **R2 amendments applied** (see docs/19–21): ADR-026, ADR-039, ADR-040 · Q42, Q49, Q65, Q66, Q71, Q81.

## Implementation status (2026-08-13) — hit-testing wired; dashboard + movable HUD

- **§2 bubble hit-testing is real**: the UI publishes every `.surface-interactive` rect (`useHitTestRects`), and a ~30 Hz cursor poller (`src-tauri/src/hit_test.rs`) clears `WS_EX_TRANSPARENT` only while the cursor is inside one — bubbles/indicator/chips are clickable and the rest of the screen stays pass-through. Modal state is a per-window COUNT that composes with hover; `reset_overlay_interactivity` (App mount) heals a reloaded/crashed WebView; the poller caches the applied mechanism so a modal mounting under a hovering cursor still gets focus.
- **Panels are non-exclusive** (`useModalSurface(ref, {exclusive:false})`): PrivacyPanel/ContextPreviewPanel/Dashboard are clickable via their own rects while clicks outside pass through to the user's apps. Only FirstRunConsent owns the screen.
- **New surfaces**: the HUD cluster (indicator + ◎ dashboard + 🛡 privacy) drags to any corner/edge midpoint and persists in `ui.hud_anchor`; the Dashboard (opaque, sidebar+content) exposes Overview/History(search)/Patterns/Suggestions/Voice over four read-only commands.
Full detail: `docs/handoff/session-bridge-2026-08-13-v1-wiring.md`.

## Implementation status (2026-08-19) — Doc 24 batch 4: the bubble UI's owner decisions

- **§3 slot admission is `freshness × confidence`** (decision #5). `BubbleSpec` gained `created_ts` (durable: `suggestions.created_ts`, migration 0004), and `bubbleLifecycle.admissionScore` decays it as `0.5^(age / half_life)` with the half-life in `ui.bubble_freshness_half_life_sec` (default 10 min). Queued bubbles are re-scored on every admission, so a stale-but-confident one can no longer hold a slot against a fresher arrival. The hard TTL freshness rule is unchanged and still core-side (doc 08 §5).
- **Two cap bugs fixed while wiring it.** `admit()` promoted anything sorting into the first `maxVisible` positions but never demoted a visible bubble, so a high-scoring arrival while 3 were on screen produced a **4th** — breaking the §3 UX cap and the doc 14 §5 glass budget together. And the queue was never drained: promotion lived in a `removeBubble` wired to `onExited`, which cannot fire (the `exit` transition removes the bubble, unmounting the Bubble before its exit-complete effect runs). Both now go through one `promote()` used by arrivals and by the freed-slot path. Visible bubbles are never re-ranked or demoted — a bubble that earned a slot keeps it for its dwell.
- **§3 dwell is a real control** (decision #7). `ui.bubble_dwell_sec` drives the countdown (it was a compile-time constant) and the Dashboard's new **Advanced** tab edits it. `set_settings` now emits `settings_changed`, so the change lands on the next bubble without a restart. The ⋯ menu also pauses the dwell while open — it is portalled to `<body>`, so moving the cursor onto it fired the bubble's `mouseleave` and the bubble could expire mid-decision.
- **§3 "Exclude this app" is real** (decision #8). `BubbleSpec.exclusion_offers` carries ready-to-apply `(match_kind, pattern)` rules derived core-side from the bubble's own connector state (`suggestion_generator::exclusion_offers_for`): browser/YouTube offer the **site** (`url_pattern`), document/app-focus offer the **app** (`process`), IDE offers nothing (its payload names no process, and a guessed rule protects nothing). One click writes a durable, hot-reloaded rule through the existing `add_exclusion` path, with an inline Undo; "Exclusions…" remains for everything the offers cannot express.
- **§3 glyphs render as marks, not words.** `BubbleSpec.glyph` is a semantic token that is also PERSISTED, so the mark is now chosen at render (`glyphMark`) — previously the literal string "video" was drawn inside the 28 px chip. Decision #15's app-focus bubbles got their own token (`switch`).
- **Multi-monitor bubble state (decision #10)** is closed at the source rather than per-window: `record_feedback` makes terminal transitions **exactly-once** (`UPDATE … WHERE state IN ('queued','shown')`; a 0-row update means someone already resolved it, so no engine signal and no second broadcast). This was a live bug — every monitor ran its own dwell timer, so one ignored bubble reported `expired` once per screen and the decay ladder was applied N times. Thumbs are guarded the same way on the rating actually changing, and a new `suggestion_rated` broadcast lights the pressed thumb on every monitor.
- **Dashboard → Advanced** also exposes the (already-live) `pattern_engine` knobs — certainty, repeats-before-a-habit, quiet time, suggestions/hour — and the push-transport preference (decision #39; see doc 09). A settings write now pings the pattern task directly, so an edit applies at once instead of on the next 24-hour maintenance tick.

Full session detail: `docs/handoff/session-bridge-2026-08-19-doc24-batch4.md`.
