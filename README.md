# Aperture

A **local-first, multimodal, proactive desktop assistant for Windows 11.**

Aperture watches how you work (only while you let it), learns your recurring
patterns **locally**, and offers one-click resumption of the things you keep
coming back to — rendered as proactive glass bubbles. Click a bubble and it
**resumes a state**: reopens a YouTube video at the exact timestamp, a document,
an IDE file at a line, a browser page. Push-to-talk voice feeds the same
behavioral model and can query it (answers render as text). Claude is the
optional heavy-reasoning tier, invoked **only when you explicitly ask** — and you
always see and approve the exact bytes that leave the machine.

> This repository is the **architecture-faithful skeleton**. The full design lives
> in [`docs/`](docs/) (22 dependency-ordered documents: 00–18 v1, **19–21 the R2
> refinement pass**, 22 the v2 draft; process/handoff notes in
> [`docs/handoff/`](docs/handoff/)). Start with
> [`docs/00-README.md`](docs/00-README.md). Docs 00–18 already read as R2 (the
> Doc 20 amendments are applied inline); Doc 21 supersedes Doc 18.

---

## The three invariants

Every crate honors these (see [`docs/00-README.md`](docs/00-README.md) §"The three invariants"):

1. **8 GB VRAM ceiling.** One heavyweight GPU model resident at a time (a single
   GPU mutex); 3B-VLM default loadout; projected-VRAM cap **7.0 GB** with the
   projection **counting co-resident weights** (ADR-030). Co-residency (VLM +
   faster-whisper STT) is conditional — STT is the swap victim under image-VLM
   pressure. Enforced by the [`orchestration`](crates/orchestration) crate (docs 04, 12).
2. **The transparency gate.** **Raw user data** leaves only via the
   [`reasoning-gateway`](crates/reasoning-gateway) crate, only after the user
   approves the **exact serialized payload** (an explicit Send **or** a scoped
   allow with payload preview + cancel window + audit). That crate is the only
   code that opens application network sockets — including the opt-in,
   off-by-default, **aggregate-only diagnostics** path; the Tauri updater is a
   separate framework path carrying no user data (docs 09, 13; ADR-026/036). A CI
   lint (`cargo xtask lint-emitters`) makes this a build-time guarantee, and the
   SC5 test proves zero **user-data** egress on the proactive path.
3. **The capture toggle.** OFF stops capture, halts recording, force-unloads
   GPU models, **and halts browser-extension forwarding**, within **3 s**, with a
   visible indicator (docs 05, 12; ADR-027/FIX 2.1). Proven by the SC6 gate.

## Locked decisions (do not relitigate — see docs/00)

Windows 11, local-first hybrid · screen understanding → proactive bubbles ·
bubbles are deep-link actions · bounded connector set (browser URL, video
timestamp, document, IDE file), sourced by a **committed v1 browser extension**
(Chrome + Opera GX) with UIA as the no-extension fallback (ADR-027) · proactive
loop is **fully local**, Claude only on explicit action via a **swappable
transport, MCP-primary** (Claude Desktop MCP → Claude Code CLI → Messages API;
ADR-025) · behavioral history in a local DB that **never leaves the device** ·
Chromemorphism & Liquid Meta design system (v1 = static glass; liquid refraction
deferred post-v1) · hardware ceiling **RTX 5060 8 GB VRAM / 16 GB RAM / Ryzen**.

## Stack

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri v2** (WebView2 UI) | ~30–50 MB idle vs Electron's 150–300 MB (doc 04 §7) |
| Core | **Rust** (one crate per subsystem) | the contracts crate makes drift a compile error (doc 15) |
| GPU inference | **llama.cpp** VLM + **whisper**-family STT as **sidecar processes** | process kill is the only *guaranteed* VRAM release (doc 02 §2) |
| Storage | **SQLite (WAL) + sqlite-vec**, SQLCipher-style at-rest encryption | one auditable, purgeable file (docs 03, 13) |
| Embeddings | **nomic-embed-text-v1.5** (137M, 768-d, CPU) | cheap, local, ~520 MB resident (doc 03 §5) |

---

## Repository layout

```
Cargo.toml                      # workspace
rust-toolchain.toml             # pinned 1.80 + msvc target
config/settings.default.json    # first-run settings seed (runtime copy lives in the encrypted DB)
docs/                           # 00–18 v1 · 19–21 R2 refinement · 22 v2 draft (authoritative)
docs/handoff/                   # process/handoff notes (session bridge, M1–M3 build prompt)

crates/
  contracts/            # ★ the five interface contracts + test fakes (doc 15) — depend on this, not on each other
  db/                   # SQLite + sqlite-vec + migrations + retention (doc 03)
  event-bus/            # in-process tokio broadcast<Event> (doc 15 §1)
  capture/              # Tier 0: WGC sampler, WinEvent/UIA hooks, exclusion, the toggle (doc 05)
  vision-ocr/           # cheap always-on OCR + on-demand VLM gating (doc 06)
  embedding/            # nomic-embed writer (doc 03 §5)
  pattern-engine/       # n-gram + recency stats → suggestion candidates (doc 08)
  suggestion-generator/ # candidate → BubbleSpec (doc 08 §6)
  connectors/           # browser / youtube / document / ide deep-link connectors (doc 10)
  orchestration/        # ★ the brain: capture toggle, GPU mutex, sidecar lifecycle, VRAM budget (doc 12)
  reasoning-gateway/    # ★ the ONLY network/CLI emitter: payload build → preview → transports (docs 09, 13)
  privacy/              # redaction, exclusion, audit log, DPAPI key, consent (doc 13)
  voice/                # push-to-talk, VAD, STT job, intent, retrieval (doc 07)
  vlm-host/             # GPU sidecar binary (llama.cpp VLM)
  stt-host/             # GPU sidecar binary (whisper)
  gates/                # milestone validation-gate tests (SC5, SC6, M0 round-trip — doc 16)

src-tauri/              # Tauri shell: IPC commands, core↔WebView events, per-monitor overlay windows
ui/                     # React + TypeScript overlay (Chromemorphism — doc 14)
xtask/                  # cargo xtask: lint-emitters (two-emitter rule), gate runner, seed-db
```

★ = the three crates that enforce the three invariants.

---

## Critical paths

- **Path A — proactive suggestion (fully local, ≤ 2 s, zero network/GPU on the path):**
  capture event → cheap OCR → embed → pattern engine → `BubbleSpec` → glass bubble. (doc 02 §4)
- **Path B — bubble click → resume:** resolve `action_ref` → `connector_state` →
  `reconstruct()` → `open()` via `ShellExecuteW`/protocol handler. (doc 02 §5)
- **Path C — voice:** PTT held → WASAPI → VAD → STT (GPU mutex) → `voice_utterance`
  + (if a query) retrieval → answer bubble. (doc 07)
- **Path D — explicit cloud:** enrichment click → payload assembled → redaction →
  **preview** → user **Send** → gateway → transport → structured suggestions. (doc 09)

## Build sequence (doc 16)

Each milestone has a **measured validation gate** on the real target.

| M | Scope | Gate |
|---|---|---|
| **M0** | Contracts crate, SQLite schema + migrations, Tauri shell skeleton, CI (SC5 monitor) | schema round-trips all event types; fakes compile; idle RAM in budget |
| M1 | Capture + the toggle | **SC6**: OFF releases everything, VRAM→~0 in < 3 s; SC5 holds |
| M2 | OCR + embeddings + store | OCR ≤ 400 ms; embed ≤ 300 ms; sane KNN |
| M3 | Pattern engine + minimal overlay | **SC2**: scripted workflow → bubble < 2 s; SC5 holds |
| M4 | The four connectors + Path B | **US1**: YouTube reopens at the right timestamp |
| M5 | VLM sidecar + GPU scheduler + budget enforcer | measured VRAM (incl. co-resident weights); never > 7.0 GB; SC3 |
| M6 | STT + PTT + retrieval | **SC4**: < 2 s transcription; US2 |
| M7 | Reasoning gateway + transparency gate (CLI **and** MCP) | **SC5 strict**: preview bytes == wire bytes; US3 |
| M8 | Design-system hardening (glass tokens, degrade-under-load, multi-monitor) | ≤ 2 glass surfaces + opaque 3rd; no overlay frame drops during a VLM job |
| M9 | Privacy hardening (encryption, retention/purge, consent, audit) | DB unreadable without the key; Purge All verified |

> **This skeleton targets M0.** Subsystem crates beyond M0 are stubbed with faithful
> signatures and `todo!("M<n>: …")` bodies tied to the milestone above.

---

## Getting started

### Prerequisites
- **Rust** 1.80+ (`x86_64-pc-windows-msvc`) — `rustup toolchain install 1.80.0` (the
  repo pins it via `rust-toolchain.toml`).
- **Node.js** 18+ and npm (for the `ui/` WebView frontend).
- **Tauri v2 prerequisites** on Windows: WebView2 runtime + the MSVC build tools.
  See <https://v2.tauri.app/start/prerequisites/>.
- GPU sidecars (`vlm-host`/`stt-host`) wrap **llama.cpp** + a **whisper** server and
  the model GGUFs — fetched/built out-of-band into `src-tauri/binaries/` and
  `models/` (both git-ignored). Exact binaries/flags are **[VERIFY]** (docs 02, 06, 07).

### Develop
```sh
# install UI deps
npm --prefix ui install

# run the desktop app (Tauri spawns the Vite dev server per tauri.conf.json)
cargo tauri dev          # or:  cargo run -p aperture

# the two-emitter guard (the transparency invariant), the gates, and a DB seed:
cargo xtask lint-emitters
cargo xtask gate m0
cargo xtask seed-db
```

> **Note:** this skeleton was scaffolded in an environment without a Rust toolchain,
> so the Rust workspace has **not been compile-verified** here. Expect to resolve
> dependency versions (all marked `# [VERIFY]`) and stub signatures on first
> `cargo check`. The `contracts`, `db` schema, `config`, and `docs` are the most
> settled; the per-subsystem crates are signature-level stubs.

## Privacy posture (doc 13)

Raw frames are **never persisted** (frame → OCR → drop). History lives in one
encrypted SQLite file that never leaves the device. Exactly one crate can talk to
the network, and only after you approve the exact payload in a preview. Every
capture toggle and every cloud send is written to a local audit log, so you can
always answer *"when was it watching?"* and *"what ever left this machine?"*

## License

Proprietary — see `Cargo.toml`. (Adjust to taste.)
