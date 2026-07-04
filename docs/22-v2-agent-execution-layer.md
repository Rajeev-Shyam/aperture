# Doc 22 — Aperture v2: Agent Execution Layer Spec (Draft)

> **Place in the set.** This is the **v2** layer — an *additive* agent-execution
> capability on top of the v1 design (Docs 00–21). It is **not part of locked v1
> scope** and **not in the buildable skeleton**. It is promoted into the doc set
> here as the working v2 specification, but it must be **grilled** (Open Questions
> Q-V2-01…Q-V2-10, §12) before any v2 build starts. Do **not** implement v2 crates
> (`action-executor`, `agent-loop`, `task-manager`, `screen-serializer`) from this
> draft yet. v1 (M1→M9) ships first. See `docs/handoff/session-bridge.md` for the
> decision context.

> **Status:** Draft for review. Not architecture-faithful yet — this is the
> specification to be grilled, refined, and eventually promoted into the doc
> set alongside Docs 00–21. Conventions follow the existing set: **[VERIFY]**
> = must be confirmed at build time; **[ASSUMPTION]** = stated reasoning,
> revisit if contradicted; **[OPEN]** = unresolved decision blocking
> implementation.

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
- Launch application by name/path
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

v2 extends it to handle **iterative agent calls**:
- New method: `agent_step(payload) → ActionInstruction`
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

| # | Question | Blocking which milestone |
|---|---|---|
| Q-V2-01 | UIA label matching reliability across apps — is fuzzy match sufficient or do we need the coordinate fallback regularly? | V2-M0 |
| Q-V2-02 | Claude MCP payload size limit with base64 screenshot — does 768px downscale stay within it? | V2-M1 |
| Q-V2-03 | Behaviour when agent loop visits an excluded app — pause and notify, or skip action and continue? | V2-M5 |
| Q-V2-04 | Step cap default (50) — is this the right number or does it need to be higher for complex tasks? | V2-M2 |
| Q-V2-05 | Should the agent loop re-use the VLM for richer screen descriptions or is Claude's own vision sufficient? | V2-M2 |
| Q-V2-06 | Prior-steps-summary strategy — Claude generates it, but at what step count does the context window become a concern? | V2-M2 |
| Q-V2-07 | Error recovery — on `ActionError::ElementNotFound`, does Claude get another attempt with the error context, or does the loop pause for user? | V2-M2 |
| Q-V2-08 | Local-only fallback planning model (Qwen2.5-3B) — in or out of v2 scope? | V2-M3 |
| Q-V2-09 | UAC-elevated windows (Task Manager, installers) — hard block, or surface a "run as admin" prompt? | V2-M5 |
| Q-V2-10 | Does the agent loop need its own VRAM priority tier, or does it always defer to v1 VLM priorities? | V2-M0 |

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
