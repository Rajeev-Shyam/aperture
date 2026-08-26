# Doc 22 — Aperture v2: Agent Execution Layer Spec

> **Place in the set.** This is the **v2** layer — an *additive* agent-execution
> capability on top of the v1 design (Docs 00–21). v1 (M1→M9) shipped first;
> v1 hardening (Doc 24 batches 1–5) landed before the executor was wired
> (owner decision #4's sequencing).

> **Status: [AMENDED 2026-08-22] — BUILT AND WIRED (V2-M0 → V2-M6), owner
> confirmation pending on the [PROVISIONAL] answers in §12.** The four v2
> crates plus the shell runtime (`src-tauri/src/agent.rs`) and the two MCP
> tools (`aperture_agent_start`, `aperture_agent_step`) are live on branch
> `r2-spec-integration`. Owner decisions #47–#54 + F2 (Doc 24 §K) are
> implemented as written. The remaining open questions were answered
> provisionally by the implementing session (each marked **[PROVISIONAL]**
> in §12 with the reasoning and what to change if the owner disagrees) — the
> grilling never happened live, so none of them is "decided"; they are the
> defaults the code runs with today. Conventions: **[VERIFY]** = must be
> confirmed on real target; **[ASSUMPTION]** = stated reasoning, revisit if
> contradicted; **[PROVISIONAL]** = implemented default awaiting the owner.
> See `## Implementation status (2026-08-22)` at the end for what exists.

---

## 0. What v2 Is (and isn't)

**v2 adds a single capability on top of v1:** the ability to execute actions
on the user's machine in pursuit of a user-defined task.

v1 = passive observer + pattern recommender
v2 = v1 + agent loop that can act

**v2 is NOT:**
- An always-on autonomous agent
- A general-purpose "do anything I say" assistant
- A replacement for the v1 proactive bubble system (that keeps running underneath)
- A system that acts without explicit user task initiation

**One-line definition:**
> The user defines a task. The agent observes the screen, asks Claude what
> to do next, executes that action locally, and repeats until the task is
> done or the user stops it.

---

## 1. The Fundamental Architecture Split (Hybrid)

Every component is either **local** or **cloud**. The split is non-negotiable
given the hardware ceiling (8 GB VRAM, 16 GB RAM, RTX 5060, Ryzen 9).

### Local (on-device, free, always)
| Component | What it does |
|---|---|
| Screen capture + OCR | Reads what's on screen — unchanged from v1 |
| nomic-embed | Embedding — unchanged from v1 |
| v1 pattern engine | Still running underneath — unchanged |
| **Screen serializer** (new) | Converts screen state into structured payload for Claude |
| **Action executor** (new) | Physically clicks, types, opens apps via Win32/UIA |
| **Agent loop controller** (new) | Manages the observe→plan→act→observe cycle |
| **Task manager** (new) | Tracks user-defined tasks, progress, history |

### Cloud (Claude Pro/Max via MCP, per-call)
| Component | What it does |
|---|---|
| Claude (Sonnet/Opus) | Receives screen state, decides next action, returns instruction |

Claude never touches the machine directly. It only returns a structured
instruction. The local executor carries it out.

---

## 2. The Agent Loop (Step by Step)

```
User defines task
       ↓
Take screenshot + run OCR
       ↓
Redact sensitive content (passwords, card numbers, etc.)
       ↓
Serialize screen state → structured payload
       ↓
[TRANSPARENCY GATE] Show payload to user + cancel window
       ↓
Send to Claude via MCP (aperture_get_context tool)
       ↓
Claude responds with: { action_type, target, value, reasoning }
       ↓
[APPROVAL GATE] User sees the action + approves (or scoped allow)
       ↓
Local action executor performs the action
       ↓
Wait for screen to settle (~300–800ms) [VERIFY]
       ↓
Take new screenshot → loop back
       ↓
Until: Claude says "task_complete" OR user cancels OR error threshold hit
```

**Key invariant:** Claude decides. Local executes. Neither can act alone.

---

## 3. New Crates

### 3.1 `action-executor`
The "hands" of the agent. Wraps Win32/UIA.

**Responsibilities:**
- Click at element (by UIA label/role, not raw pixel) [ASSUMPTION: UIA label
  matching is reliable enough; fallback to pixel coords if not — VERIFY M-V2-1]
- Type text into focused element
- Press keyboard shortcuts
- Launch application by name **[AMENDED 2026-08-22, decision #52]** — as a
  *simulated Start-menu search* (Win key → type the name → Enter via
  `SendInput`), never a process spawn; the crate has no spawn API at all
- Switch focus to a window
- Scroll within a region
- Read current element focus (for verification after action)

**What it does NOT do:**
- File system writes (out of scope v2)
- Network requests (out of scope v2)
- Registry edits (out of scope v2)
- Anything outside the visible UI surface

**Failure modes:**
- Element not found → return `ActionError::ElementNotFound`, report to loop
- Element not interactable → return `ActionError::NotInteractable`
- Timeout waiting for screen settle → return `ActionError::Timeout`
- Access denied (UAC-elevated window) → return `ActionError::Elevated`,
  notify user

**Safety constraint:** The executor only acts on actions originating from
an active agent loop with a user-initiated task. It cannot be called
arbitrarily from other crates. Enforced via the contracts crate.

---

### 3.2 `screen-serializer`
Converts the current screen state into a structured payload suitable for
sending to Claude.

**Output schema (per step):**
```json
{
  "task": "string — the user's stated task",
  "step_number": 4,
  "screenshot_b64": "...",
  "ocr_text": "structured OCR output with regions",
  "focused_window": { "app": "Chrome", "title": "...", "url": "..." },
  "last_action": { "type": "click", "target": "Submit button", "result": "success" },
  "open_windows": ["VSCode", "Chrome", "Terminal"],
  "prior_steps_summary": "brief Claude-generated summary of what's happened so far"
}
```

**Redaction:** runs the existing privacy/redaction pipeline from v1 before
serializing. Passwords, card numbers, and anything matching the v1 redaction
rules are stripped before this payload is built. [ASSUMPTION: v1 redaction
rules are sufficient for agent payloads — OPEN: review what new surfaces
the agent exposes that v1 didn't consider]

**Screenshot handling:** downscale to 768px (same as v1 VLM adaptive path,
ADR-032) before base64 encoding to keep payload size manageable.
[VERIFY: Claude MCP payload size limits — M-V2-2]

**prior_steps_summary:** after step 5+, instead of sending full history,
send Claude's own summarisation of prior steps (Claude generates this at
each step as part of its response). Keeps context window from bloating on
long tasks.

---

### 3.3 `agent-loop`
The controller that runs the observe→plan→act cycle.

**Responsibilities:**
- Owns the active task state machine
- Calls screen-serializer to build payloads
- Routes payloads through reasoning-gateway (existing crate, extended)
- Receives Claude's action instruction
- Routes instruction to action-executor
- Monitors for loop termination conditions
- Writes every step to the audit log

**Task state machine:**
```
IDLE → RUNNING → (PAUSED | COMPLETE | FAILED | CANCELLED)
```

**Termination conditions:**
- Claude returns `{ "status": "task_complete" }`
- User hits the hard stop (keyboard shortcut / UI button)
- Error count exceeds threshold (3 consecutive action failures) [ASSUMPTION]
- Step count exceeds cap (50 steps default, user-configurable) [ASSUMPTION]
- VRAM pressure forces a sidecar unload mid-task (graceful pause)

**Hard stop mechanism:** a always-visible, always-accessible UI affordance
(floating pill or system tray) that immediately halts the loop, performs no
further actions, and writes a `task_cancelled` audit event. Must work even
if the main UI is unresponsive. Implemented as a separate lightweight process
that sends a signal to the agent-loop. [OPEN: exact IPC mechanism — M-V2-0]

---

### 3.4 `task-manager`
Tracks tasks across sessions. Backed by the existing SQLite DB (new tables).

**New DB tables:**
```sql
CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  description TEXT NOT NULL,
  status TEXT NOT NULL, -- idle|running|paused|complete|failed|cancelled
  created_at INTEGER NOT NULL,
  completed_at INTEGER,
  step_count INTEGER DEFAULT 0,
  outcome_summary TEXT -- Claude's final summary on completion
);

CREATE TABLE task_steps (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id),
  step_number INTEGER NOT NULL,
  screen_payload_hash TEXT, -- SHA-256 of what was sent to Claude
  action_type TEXT,
  action_target TEXT,
  action_value TEXT,
  result TEXT, -- success|failure|skipped
  claude_reasoning TEXT, -- Claude's stated reason for the action
  timestamp INTEGER NOT NULL
);
```

**Retention:** tasks retained 90 days (same as events), task_steps 30 days.
[ASSUMPTION: consistent with v1 retention philosophy — VERIFY Doc 03 R3]

---

## 4. Changes to Existing Crates

### 4.1 `reasoning-gateway` (extended)
Currently handles one-shot Claude calls (user clicks "Ask Claude", approves
payload, gets response).

v2 extends it to handle **iterative agent calls** **[AMENDED 2026-08-22]** —
on the EXISTING MCP plumbing rather than a new gateway method (v2 kickoff §1):
- `aperture_agent_start(task?)` + `aperture_agent_step(task_id, instruction?)`
  are the 5th/6th tools on the `aperture-mcp` pipe; Claude Desktop holds the
  conversation and calls `agent_step` once per turn (instruction for the last
  screen in, the next screen out). A push-transport-driven loop (Aperture
  calling the CLI/API itself with `agent_step(payload) → ActionInstruction`)
  is **not built** — MCP-primary per ADR-025; it is the natural follow-up if
  the owner wants agent mode without Claude Desktop.
- Scoped allow applies per-task (not per-step) — user approves the task
  loop once, not every step
- Cancel window still shown per-step in the overlay (user can cancel
  any step before execution)
- Every step's payload hash written to audit log regardless of scoped allow

**The transparency gate in agent mode:**
The invariant still holds — the user sees what's being sent. In agent mode
this means:
- The overlay shows a compact "step N: [action description]" pill
- Full payload viewable on demand (not forced — reduces friction)
- Cancel available at every step
- Scoped allow = approve the whole task loop, still see each action, still
  have per-step cancel

### 4.2 `orchestration`
GPU mutex must account for agent loop demanding screen-serializer
(which may want VLM for richer descriptions) competing with v1's VLM
wake schedule.

New priority: `agent-VLM` sits between `user-VLM (80)` and `enrichment-VLM (70)`.
[OPEN: exact priority value — ASSUMPTION: 75]

Agent loop must pause gracefully if VRAM pressure forces VLM unload
mid-task rather than crashing.

### 4.3 `privacy`
Redaction pipeline must now run on agent payloads (screen state sent
every step) not just on the one-shot enrichment payloads.

New concern: the agent loop may visit pages/apps that were in the
exclusion list. The action-executor must check exclusions before acting.
If the target window matches an exclusion, the loop pauses and notifies
the user rather than proceeding blind. [OPEN: exact behaviour — M-V2-3]

---

## 5. Claude's Response Schema

Claude must return a structured JSON response at every step. This is
enforced via the system prompt sent with each agent_step call.

```json
{
  "status": "continue | task_complete | need_clarification | cannot_proceed",
  "reasoning": "one sentence — why this action",
  "action": {
    "type": "click | type | key | launch | switch_window | scroll | wait | none",
    "target": "element label or window name (for click/switch)",
    "value": "text to type or key combo (for type/key)",
    "direction": "up|down|left|right (for scroll)",
    "amount": 3
  },
  "step_summary": "one sentence summary of all steps so far including this one",
  "confidence": "high | medium | low"
}
```

**On `confidence: low`:** the agent loop pauses and surfaces a confirmation
chip to the user before executing. [ASSUMPTION: low confidence = Claude
is uncertain about what to click — safer to ask]

**On `status: need_clarification`:** the loop pauses, surfaces Claude's
question to the user as a text input bubble, resumes on answer.

**On `status: cannot_proceed`:** loop terminates gracefully, surfaces
reason to user.

---

## 6. Action Grounding — the Hard Problem

**The problem:** Claude says `"click the Submit button"`. How does the local
executor find exactly where that is on screen?

**Primary approach — UIA label matching:**
- Windows UIA exposes every interactive element with a name/role/state
- The executor walks the UIA tree to find an element whose name matches
  Claude's target string (fuzzy match, Levenshtein distance ≤2) [ASSUMPTION]
- If found: click the element's bounding rect centre
- If multiple matches: pick the one in the focused window

**Fallback — pixel coordinate from Claude:**
- If Claude has high confidence about location, it can optionally include
  `"coords": { "x": 423, "y": 891 }` derived from the screenshot
  **[AMENDED 2026-08-22]**: the executor treats coords as *primary-monitor
  physical pixels*; scaling from the 768-px screenshot space is not done
  yet (the system prompt does not advertise coords — UIA grounding is the
  only path Claude is told about). [VERIFY] before advertising.
- Used only when UIA match fails [ASSUMPTION: Claude's coordinate
  estimation from a 768px downscaled image is accurate enough — VERIFY M-V2-1]

**Hard fallback — pause + ask user:**
- If both fail: loop pauses, highlights the screenshot, asks user to click
  the right element manually. Records that element for future steps.

**[OPEN]:** whether a small local action-grounding model (e.g. fine-tuned
Moondream, ~2GB VRAM) improves accuracy enough to justify the VRAM cost.
Leave as a v2.1 option. Do not block v2 on it.

---

## 7. Hardware Budget in Agent Mode

The agent loop adds load but doesn't change the VRAM ceiling.

**Per-step VRAM usage:**
- Screen capture + OCR: CPU only, 0 VRAM
- Screen serializer: CPU only, 0 VRAM (screenshot resize is trivial)
- Claude call: cloud, 0 local VRAM
- Action execution: CPU/Win32, 0 VRAM
- nomic-embed (if running): ~0 VRAM (CPU model)

**VRAM only consumed if:**
- VLM is woken to enrich the screen state description (optional, not default)
- STT is running simultaneously (user is doing voice + agent at same time)

**Default agent loop = 0 VRAM.** The VLM is NOT required for basic agent
operation. Claude sees the screenshot directly. The VLM would only add
value if Claude needs a richer semantic description — that's a v2.1
optimisation. [ASSUMPTION]

**RAM per-step overhead:** minimal — one screenshot buffer (~8MB at 1080p
before downscale), one JSON payload (~50-200KB), cleared after each step.

---

## 8. Privacy & Transparency in Agent Mode

The v1 transparency invariant survives but gets harder to maintain.

**What leaves the machine every agent step:**
- A 768px screenshot (downscaled)
- The OCR text of the screen (redacted)
- The task description
- The prior steps summary

**This is a meaningful privacy tradeoff.** On a heavy work session this
means Claude sees a sequence of your screens. The user must understand
this before enabling agent mode.

**Mitigations (all required for v2 ship):**
- Onboarding screen for agent mode explicitly states what leaves the machine
  per step
- Redaction pipeline runs on every payload
- Exclusion list applies — if an excluded app is in focus, the loop pauses
- Per-task audit log: user can always see exactly what screenshots/payloads
  were sent for any task
- "Purge task history" deletes all task steps and associated payloads from
  the audit log

**[OPEN]:** whether to offer a "local-only planning" fallback mode using a
small local model (Qwen2.5-3B) for simple tasks. Slower, weaker, but zero
egress. Leave as a post-v2 option.

---

## 9. New UI Surfaces

### 9.1 Task input
- A text field (hotkey-triggered, e.g. Ctrl+Win+A) — user types task
- Optional: voice input via v1's PTT mechanism
- Task description stored in `tasks` table immediately on submission

### 9.2 Agent status pill
- Persistent floating pill while a task is running
- Shows: `[Task name] — Step N — [last action]`
- Hard stop button always visible on the pill
- Click pill → expands to show full step history

### 9.3 Step confirmation chip (optional, for low-confidence steps)
- Small overlay showing: `Claude wants to: [action description]`
- Approve / Skip / Stop buttons
- Auto-approves after configurable timeout if scoped allow is active

### 9.4 Task complete bubble
- Rendered as a standard v1 bubble on task completion
- Shows: task description + outcome summary from Claude
- "Useful?" thumbs — feeds task quality signal

---

## 10. Milestone Plan

| Milestone | Scope | Gate |
|---|---|---|
| **V2-M0** | `action-executor` crate — UIA click/type/launch, no loop yet | Scripted: click a known element in a test app reliably |
| **V2-M1** | `screen-serializer` — screenshot + OCR → JSON payload, redaction applied | Payload builds correctly; no sensitive content leaks through redaction |
| **V2-M2** | `agent-loop` + `task-manager` — single-app tasks, step-by-step manual approval every step | Complete a 5-step task in one app (e.g. fill and submit a form) |
| **V2-M3** | Scoped allow for agent loop — approve task once, per-step cancel still available | Complete a 10-step task without per-step approval; cancel at step 7 works cleanly |
| **V2-M4** | Multi-app tasks — loop handles app switching, window focus changes | Task spanning Chrome + VSCode completes correctly |
| **V2-M5** | Hard stop hardening + exclusion list enforcement in executor | Hard stop works under load; excluded app focus correctly pauses loop |
| **V2-M6** | Task history UI + audit view integration | User can review full step history and payloads for any past task |
| **V2-M7** | Privacy audit — redaction coverage review for agent payloads, onboarding screen | No sensitive content in any test payload; onboarding shown on first agent use |

---

## 11. Locked Decisions (v2)

1. **Hybrid architecture is fixed.** Local eyes + hands, cloud brain. No
   fully-local planning model in v2.
2. **Claude is the only planner.** No other LLM provider in v2.
3. **Action surface is UI-only.** Win32/UIA. No file system writes, no
   registry, no network calls from the executor.
4. **User initiates every task.** No autonomous task creation by the agent.
5. **Hard stop is always available.** Cannot be disabled, even under scoped
   allow.
6. **Every step is audited.** Payload hash + action + result written to DB
   regardless of scoped allow state.
7. **v1 runs underneath v2 unchanged.** The proactive bubble system, pattern
   engine, and voice query layer are unaffected by agent mode being active.

---

## 12. Open Questions (to resolve during grilling)

**[AMENDED 2026-08-22]** — status per question. "Decided" = an owner
decision in Doc 24; **[PROVISIONAL]** = the default the code runs with,
chosen by the implementing session, to be confirmed or overturned by the owner.

| # | Question | Status (2026-08-22) |
|---|---|---|
| Q-V2-01 | UIA label matching reliability — is fuzzy match sufficient? | **Answered by the V2-M0 gate** (`gates/tests/v2m0_uia_executor.rs`, run on the dev box 2026-08-22): exact → contains → Levenshtein ≤ 2 on normalised labels found "File"/"Notepad" first try. Coordinate fallback exists but is not advertised to Claude. [VERIFY] across Electron/Chromium UIs — Notepad is one app. |
| Q-V2-02 | Does a base64 768 px screenshot fit the MCP result cap? | **Measured**: worst-case noise frame 768×432 q85 = 525 KB JPEG / 700 KB base64 (`screen-serializer` test `q_v2_02_…`), under the 1 MiB cap (decision #42) with headroom; real screens compress far smaller. The cap is enforced on text+image together in `mcp_bridge::agent_step`. |
| Q-V2-03 | Excluded app in front | **Decided — #49 pause and notify.** Executor refuses (`ActionError::Excluded`) and `observe_now` refuses (`CaptureError::Excluded`); the surface shows Resume/Stop. |
| Q-V2-04 | Step cap default | **[PROVISIONAL] 50**, now a setting (`agent.step_cap`, clamped 1–500) rather than a constant, so the owner can move it without a rebuild. |
| Q-V2-05 | VLM in the loop | **[PROVISIONAL] No** — Claude sees the redacted screenshot; the loop is 0-VRAM (§7). The VLM stays available for v1 enrichment. Revisit if grounding quality on dense screens disappoints. |
| Q-V2-06 | Prior-steps summary | **Dissolved by the MCP pull shape** (v2 kickoff §2): Claude Desktop holds the conversation; `prior_steps_summary` is Claude's own one-sentence rolling summary (§5 `step_summary`) echoed back, plus the user's clarification answers. No local history is sent. |
| Q-V2-07 | Error recovery on a failed action | **[PROVISIONAL] Claude retries with the error context**: the executor's error text is echoed as `last_action.result` in the next payload; the 3-consecutive-failure threshold (§3.3) bounds the retries; a malformed instruction counts as a failed step. |
| Q-V2-08 | Local-only planning model | **Out** (locked decision 1 + kickoff recommendation). Nothing built. |
| Q-V2-09 | UAC-elevated windows | **Decided — #50 prompt, not block.** Executor refuses (`ActionError::Elevated`); the surface explains that Windows UIPI blocks input from an unelevated Aperture and offers Resume after the user handles that step. **Not built:** an actual "restart Aperture as administrator" affordance — that is a privilege change the owner should choose explicitly (see Implementation status). |
| Q-V2-10 | Own VRAM priority tier | **[PROVISIONAL] None** — the default loop uses no VRAM (§7), so no tier was added; `StopReason::VramPause` / `PauseReason::Vram` exist but nothing raises them yet (no agent-side GPU job exists to be pre-empted). |

---

## 13. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| RK-V2-01: UIA not exposed by some apps (games, Electron, some web UIs) | High | Med | Coordinate fallback + hard pause |
| RK-V2-02: Claude misidentifies UI element → wrong click in sensitive context | Med | High | Low-confidence pause gate + step audit |
| RK-V2-03: Agent loop runs away (task never completes, keeps looping) | Med | Med | Step cap + error threshold hard stops |
| RK-V2-04: Screenshot contains sensitive content Claude shouldn't see | Med | High | Redaction pipeline + exclusion enforcement |
| RK-V2-05: Claude API latency spikes → task feels broken | Med | Low | Per-step timeout + user notification |
| RK-V2-06: MCP transport breaks mid-task (Claude Desktop update etc.) | Low | High | Graceful pause + task resumption on reconnect |
| RK-V2-07: User grants scoped allow then forgets agent is running | Med | Med | Persistent always-visible status pill |

---

## 14. What v2 Deliberately Does NOT Include

These are explicitly out of scope to keep v2 scoped and shippable:

- File system read/write operations
- Terminal/shell command execution
- Programmatic web scraping (not UI interaction)
- Multi-monitor agent operation (primary monitor only, same as v1)
- Parallel task execution (one task at a time)
- Agent-to-agent communication
- Any local planning model
- Any provider other than Claude as the planner

---

*End of draft. To be grilled, refined, and promoted to the doc set.*

---

## Implementation status (2026-08-22) — V2-M0 → V2-M6 wired; Doc 24 §K decisions implemented

What exists on `r2-spec-integration` (commit after `108c05c`), mapped to §10:

| Milestone | Status | Where |
|---|---|---|
| **V2-M0** executor | **Done, gate passed on-target** | `crates/action-executor` — `UiaExecutor` (UIA tree walk, `grounding` exact→contains→Levenshtein≤2, `SendInput` click/type/key/scroll, simulated-Start `launch` per #52, `switch_window` with foreground retry + settle), elevation detection (#50), `ExclusionProbe` (#49), hard-stop flag (locked 5), `risk` keywords (#47/#51), `reversibility` (#54). Gate: `gates/tests/v2m0_uia_executor.rs` drove a real Notepad: switch → type (verified by UIA read-back) → click "File" → Esc → Ctrl+A → Backspace → Alt+F4. Three WinUI input quirks fixed by that gate: keys must be *held* until the target pumps (else XAML drops them), printable characters go as layout VK events not Unicode packets (which coalesce), and a freshly-foregrounded app drops input for ~500 ms. |
| **V2-M1** serializer | **Done** | `crates/screen-serializer::screenshot::observe_frame` — OCR with word boxes (`OcrOutput.lines`, new) → `privacy::image_redaction::redact_bgra` (solid block over every word a text rule covers, same coordinate space) → 768 px JPEG → base64. Q-V2-02 measured. Live feed: `CaptureSubsystem::observe_now` (same exclusion gate as a scheduled sample, frame returned not sunk). |
| **V2-M2** loop + task manager | **Done** | `crates/agent-loop::driver::AgentDriver` (pure policy, 16 offline tests) + `src-tauri/src/agent.rs` (I/O: waits for the user inside the MCP call ≤110 s, executes on a blocking thread with the driver moved out of the mutex, observes, audits **before** release fail-closed). `task-manager` gained `list_tasks`. |
| **V2-M3** once-per-task approval | **Done (#48)** | `PauseReason::Approval` card on the overlay; a user-typed task is approved by construction (locked 4). Every step still audited; STOP always visible. |
| **V2-M4** multi-app | **Built, not gated** | `switch_window` + `launch` exist; no multi-app gate test yet. |
| **V2-M5** hard stop + exclusions | **Done** | Stop = tray item "Stop agent task" (own thread) + surface button + `agent_decide(stop)`: flags the executor first, then transitions. Exclusions/elevation pause-and-notify. |
| **V2-M6** history UI | **Done** | Dashboard → **Agent** tab: tasks, per-step audit rows (payload hash, action, result, reasoning), purge. |
| **V2-M7** privacy audit + onboarding | **Partial** | The approval card states what leaves per step (§8). A formal redaction-coverage review of agent payloads has not been done; the owner QA list in the session bridge covers it. |

**Deviations from this spec, stated plainly:**
- §3.3 "hard stop … separate lightweight process": implemented **in-process on the tray thread** (M-V2-0 resolved per the kickoff's recommendation). The tray survives an unresponsive WebView; it does not survive an unresponsive `aperture.exe`.
- §9.4 "task-complete bubble": the terminal card on the agent surface plays that role (with the #54 undo offer). A v1 bubble with no Resume action does not exist yet; bubbling it would have meant a no-action bubble variant.
- §9.3 "auto-approves after timeout": **not implemented** — a consequential action waits for the user (the MCP call returns "still waiting" after 110 s and Claude re-calls). Auto-approval would contradict #47.
- §4.2 agent-VLM priority: not added (Q-V2-10).
- Decision #50's "run as admin" prompt explains and offers Resume; it does not relaunch Aperture elevated.

**Invariants confirmed by gates this session:** SC5 is now a real byte-level harness (`gates/tests/sc5_network_monitor.rs`, not `#[ignore]`): zero bytes / zero non-loopback connections / zero child processes through the proactive path, and the approved Send's body hash == preview hash == `cloud_send` row. The executor is not an emitter (`lint-emitters` passes with the new crates scanned; the F2 ticket lint refuses `ExecutorTicket::for_task(` outside `agent-loop`).

Full detail: `docs/handoff/session-bridge-2026-08-22-v2-wiring.md`.
