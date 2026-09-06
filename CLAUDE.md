# CLAUDE.md — Aperture project instructions

Read this first, every session. These rules override default behaviour.

## 1. Who you are working with

Rajeev — owner and sole developer. He is AuDHD; the response shape below is an
**accessibility requirement, not a style preference**. Apply it from the first line.

- **Answer first.** Conclusion in line one, reasoning after.
- **Short chunks, bullets over paragraphs.** Bold the one load-bearing line.
- **One question at a time, max.** No filler, no apology spirals, no throat-clearing.
- **Recommendation, then alternatives.** End with a concrete next step.
- Competent peer (Rust, Python, agentic AI, MCP, RAG, llama.cpp). Push back directly when he is wrong.
- **Don't over-engineer and don't reinvent.** Check whether the thing already exists first.

## 2. What this is

Aperture: a local-first, multimodal, proactive desktop assistant for Windows 11.
Tauri v2 shell + Rust workspace (one crate per subsystem) + React/Vite overlay +
llama.cpp / whisper sidecars + SQLite/sqlite-vec. Branch `r2-spec-integration`.

| Need | Where |
|---|---|
| Design authority (why) | `docs/00-README.md` → docs 01–24. Locked decisions + the three invariants are in docs/00 and the newest bridge — **never relitigate them**. |
| What is actually built (component + change + system-design docs) | `docs/engineering/` (§4) |
| The working checklist | `docs/TODO.md` (§5) |
| Narrative handoff | newest `docs/handoff/session-bridge-*.md` |

## 3. Session protocol

**Start**
1. Read the top of `docs/TODO.md` (last session's summary + carry-forwards).
2. Read the newest session bridge.
3. Write this session's curated TODO list in `docs/TODO.md` **before touching code**.

**Work** strictly from that list (§5). **End**: tick the list, write its summary, write the
session bridge, update `docs/engineering/` for everything built or changed, update memory, commit.

## 4. Documentation rules (mandatory)

Everything lives under `docs/engineering/`; its `README.md` holds the index and the templates.

- **Components** — `docs/engineering/components/<name>.md`, one per crate, shell module,
  UI surface, or tool. **Create it in the same change that creates the component.** Update it
  in the same change that alters the component's public API, data flow, invariants, settings,
  events, or failure modes. Detail bar: a reader who has never opened the code can say what
  it does, what it owns, what it talks to, how it fails, and how it is tested.
- **Big changes** — `docs/engineering/changes/YYYY-MM-DD-<slug>.md`, one per change.
  *Big* = any of: touches ≥ 3 source files; changes a public API, contract, schema/migration,
  setting, event, MCP tool, gate, or invariant; fixes a review finding. Small edits ride in
  the change doc of the work they belong to. **Write it as part of the change, not at session end.**
- **System design** — `docs/engineering/system-design/<topic>.md` for cross-cutting
  mechanisms (egress gate, GPU budget, agent loop, storage, overlay input…). Update when
  the mechanism changes.
- Docs 00–24 remain the design authority; engineering docs describe what is built. When
  they disagree, say so in the engineering doc and in the bridge — never silently edit the
  design doc to match the code.

## 5. The TODO list — `docs/TODO.md`

One file, newest session at the top. It is the checklist; the bridge is the narrative.
Per session:

```
## YYYY-MM-DD — <session title>
### TODO (curated at session start)
- [ ] 1. <verifiable item> — done when: <criterion>
### Discovered (not worked unless it blocks the list)
- <item> — <why it matters>
### Summary (written at session end)
Done · Not done and why · Carried forward · Verification state
```

Rules: **≤ 10 items, ordered, each verifiable.** Curate from the previous summary + the
newest bridge + the owner's ask. Work discovered mid-session goes under *Discovered* and is
not started unless it blocks a listed item. Items only the owner can do (moves the mouse,
needs Claude Desktop, needs a decision) are tagged `[owner]`. No drifting: if the list is
wrong, rewrite the list, then work it.

## 6. Engineering rules

- Smallest correct change. Reuse before build. No new dependency, setting, migration, or
  crate without a reason stated in the change doc.
- Pure logic gets tests; true I/O is marked UNVERIFIED honestly. Report test output as it is.
- `cargo run -p xtask -- lint-emitters` before every commit (the two-emitter invariant).
- Never run `#[ignore]`d gates unasked: V2-M0 moves the user's mouse and keyboard; SC6 kills
  sidecars. They are the owner's to run.
- Never a bare `cargo build` into the install dir. Sidecars are rebuilt into
  `src-tauri/binaries/` under the `-x86_64-pc-windows-msvc` names, then
  `ui\node_modules\.bin\tauri.cmd build` produces `target\release\bundle\nsis\`.
- **Subagents sparingly (owner, 2026-09-05): do the work yourself by default.** Agents burn
  tokens and die on the account session limit. Use at most a handful, only for genuinely
  parallel, disjoint file sets, and never a second wave in the same session. When you do:
  one integrator, `git status` after every fan-out, delete strays, and look at the disk after
  a dead agent (writers save their file before returning).
- Docs are checked mechanically before they are called checked:
  `PYTHONIOENCODING=utf-8 python scripts/doccheck.py . --all` (see `docs/engineering/README.md`).

## 7. Verification (run before claiming anything is done)

```
cargo test --workspace              # 0 failed, 0 warnings; on-hardware gates stay ignored
cargo run -p xtask -- lint-emitters
npm --prefix ui run test            # vitest
npm --prefix ui run build           # tsc + vite
```

## 8. Environment gotchas

- Bash heredocs and `sed` corrupt backslashes here — write source with the Edit/Write tools.
- PowerShell 5.1: `git commit -m` with embedded quotes splits into pathspecs — write the
  message to a file and `git commit -F <file>`.
- `LNK1104: cannot open file …exe` on parallel cargo = Defender holding the binary. Retry.
- Workflow subagents can die on the account session limit mid-task. Check `git status`.
- An installed app keeps its encrypted-DB settings; only missing keys backfill on upgrade.
- The rest: the "Gotchas" sections of the session bridges.

## 9. Memory

Auto-memory lives outside the repo. Keep it to: how Rajeev works, a pointer to the current
build state, and environment gotchas. Everything else belongs in the repo docs above.
