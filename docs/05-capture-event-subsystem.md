# Doc 05 — Capture & Event Subsystem (Tier 0)

## 1. Interface
| | |
|---|---|
| **Inputs** | OS frames (WGC), WinEvent/UIA notifications, foreground-window queries, **browser-extension feed (URLs + video position via the native-messaging host, ADR-027/028)**, toggle state (from Doc 12), exclusion list (Doc 13) |
| **Outputs** | Normalized `Event`s on the bus (Doc 15 §1); ephemeral frames handed to OCR (Doc 06); connector-state snapshots (Doc 10) |
| **Resource cost** | CPU-only; ~0 VRAM; target < 300 MB RAM, < 2 % idle CPU [VERIFY] |

## 2. Screen capture — Windows.Graphics.Capture (WGC)
- One `GraphicsCaptureItem` per monitor, a `Direct3D11CaptureFramePool` (1–2 buffers), frames pulled **on demand** — never a continuous stream.
- **Known caveat (RK5):** WGC draws a system **yellow border** on actively captured items on stock Windows 11. Mitigation: per-monitor capture + the tray indicator as the honest status surface; investigate border-suppression availability for non-UWP callers [VERIFY — if unacceptable, evaluate `IsBorderRequired=false` support or duplication-API fallback].
- **Self-exclusion:** the overlay window sets `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` so bubbles never appear in our own frames (no feedback loop into the model).
- Frames are **ephemeral**: downscale → OCR → drop. Raw frames are never written to disk (Doc 13).

## 3. Event hooks
| Source | Mechanism | Yields |
|---|---|---|
| Foreground change | `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` + `GetForegroundWindow`/`GetWindowThreadProcessId` | `window_focus` (app, process, title) |
| Window open/close | WinEvent `EVENT_OBJECT_SHOW/DESTROY` filtered to top-level, or UIA `WindowOpenedEvent` | `window_open`/`window_close` |
| Title change | `EVENT_OBJECT_NAMECHANGE` on the foreground hwnd | refreshes title (feeds document/IDE connectors) |
| Browser URL | **Browser extension (tabs API) — the primary source** (ADR-027); the **UIA** read of the address-bar Edit element ("Address and search bar" in en-US Chrome — **language/version dependent**) is the **no-extension fallback**, on focus/title-change of a browser process | `navigation{url}` |
All hooks run on a dedicated thread; handlers do **no work** beyond posting to the bus (keeps OS hook latency rules).

## 4. Sampling policy (event-driven, not FPS)
- **Trigger sample** on: focus change, window open, navigation, title change of foreground — debounced **300 ms** [ASSUMPTION] so focus storms coalesce.
- **Heartbeat sample** on an **adaptive ~5–20 s** interval [ASSUMPTION] (modulated by input activity + event density; **10 s default**, ADR-032) while the user is active (input within 60 s); suspended when idle.
- Per-sample work: capture one frame of the foreground monitor → crop to foreground window rect when cheap → hand to OCR.
- **Near-duplicate gate (pHash):** compute a perceptual hash of the captured frame; if it is within a Hamming threshold [ASSUMPTION, tuned at M2] of the last processed frame, **skip OCR/embed** — static screens don't re-do redundant work (writes/reuses `thumb_phash`, Doc 03; Q72). This gate sits **before OCR and embedding.**
- **Exclusion enforcement happens here:** if the foreground process/window matches `exclusion_list`, no frame is captured, the event is recorded as metadata-only with `redaction_flags|=EXCLUDED`, and no OCR/connector capture runs. This is the earliest possible gate (Doc 13). **Extension-sourced URLs are not privileged (FIX 2.2):** URLs arriving from the browser extension traverse this **same exclusion + redaction pipeline** (exclusions gate them, the redaction rules of Doc 13 §5 apply) before they reach the bus — the extension is not a bypass around the gate (ADR-029).

## 5. The enable/disable toggle (G8 / SC6) — state machine
```
            user toggles OFF
   ON ───────────────────────► STOPPING ───────► OFF
   ▲   1. stop sampler thread     (≤3 s SLA)      │
   │   2. Close() WGC session, frame pool,        │ user toggles ON
   │      release D3D refs                        ▼
   └── 3. UnhookWinEvent / remove UIA handlers   STARTING: re-acquire WGC item/pool,
       4. signal native-messaging host → stop     re-register hooks, resume sampler
          extension URL/video forwarding (FIX 2.1)
       5. signal Doc 12 → kill vlm-host/stt-host
       6. flip tray + overlay indicator to ⏸      indicator ▶, emit capture_toggle(on)
       7. emit capture_toggle(off) audit event
```
- **Extension forwarding stops too (FIX 2.1):** toggling OFF also signals the native-messaging host to stop forwarding URLs / video position from the browser extension — the extension path is part of "everything stops," never a route around the toggle (ADR-027/028).
- OFF guarantees: **no events written, no frames taken, extension forwarding halted, sidecars dead, VRAM released** (verified at the M1 gate with `nvidia-smi`).
- The toggle state is owned by the Orchestration Manager (single writer); this subsystem obeys it.

## 6. Internal logic summary
`hook thread → debouncer → sampler → (frame → OCR) + (event → normalizer → bus)`; the normalizer attaches app/process/title, assigns `session_id` (Doc 08 sessionizer), checks exclusions, and forwards `can_capture` events to the connector registry (Doc 10).

## 7. Failure modes
| Failure | Behavior |
|---|---|
| WGC unsupported / capture denied for a window (DRM, secure desktop) | Event-only mode for that context; flag once in UI |
| Extension unavailable (not installed) → UIA fallback read also fails (localization, browser update) | Fall back to last-known URL or skip `navigation`; connector marks state stale — RK4 mitigated by shipping the extension as the primary URL source (ADR-027) |
| Hook callback starvation (system load) | Heartbeat still samples; missed events tolerated by design |
| Frame pool device-lost | Recreate pool; if persistent, degrade to event-only and surface a notice |
| Toggle OFF exceeds 3 s | Hard-kill sidecars (process kill), force-release WGC; log SLA breach |

---
> **R2 amendments applied** (see docs/19–21): ADR-027, ADR-028, ADR-029, ADR-032; Q72 (pHash near-duplicate gate); FIX 2.1, FIX 2.2.
