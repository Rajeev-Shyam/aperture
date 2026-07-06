<!-- Handoff/process doc (not an architecture doc). Bridges the session that
     closed M5 and self-reviewed M4+M5 into the M6/M7 build work. The authoritative
     design remains Docs 00–21; this captures state, decisions, and carry-forward. -->

# 🔄 CLAUDE SESSION BRIDGE — M6 / M7 — read this first

**Session date: 2026-07-06**
**Bridges into: M6 (voice / STT, Doc 07) → M7 (reasoning gateway, Docs 09/13)**
**Repo:** https://github.com/Rajeev-Shyam/aperture · branch `r2-spec-integration`

You are a fresh Claude session picking up **Aperture** — a local-first, privacy-preserving
desktop assistant for Windows 11 (Tauri + Rust). Read this whole file before responding.
Your first response should: confirm you understand the state, apply "How This User Works"
from the very first line, and ask at most ONE clarifying question if genuinely blocked.

---

## Where the build is

**M0–M5 are landed and verified green** (workspace `cargo test` passes; each affected crate
confirmed individually). The last committed milestone is **m4**; the **M5 work is
uncommitted** in the working tree at bridge time (Rajeev may commit it — check `git status`).

| Milestone | State |
|---|---|
| M0–M4 | Landed + committed. Contracts/schema/shell, capture+toggle, OCR/embed/store, pattern engine + overlay, 4 connectors + browser extension + native-messaging bridge. |
| **M5** | **Landed in software (uncommitted).** Orchestration resource manager (scheduler/budget/lifecycle/tier-router/telemetry) + on-demand VLM enrichment path, wired into the shell off the bubble path. Gate harness `crates/gates/tests/m5_*` passes the software-checkable criteria; measured-VRAM + SC3 load-times are `#[ignore]` pending the RTX 5060. |
| M6 → | Next. Not started. |

**The three invariants (never re-open):** ① 8 GB VRAM ceiling — BudgetEnforcer admits only
≤ **7.0 GB** projected (ADR-030, counts co-resident weights); ② two-emitter transparency gate
(nothing leaves the device on the proactive path; only the M7 reasoning gateway opens a socket);
③ capture toggle — OFF releases VRAM to ~0 in < 3 s via killing sidecars.

---

## What this session did

**1. Closed out M5:**
- Wired the VLM enrichment path into the shell (`OcrStoreSink` → cheap gate → `maybe_enrich_vlm`
  → `prio:50` job → `attach_vlm_summary`). It was half-done (dangling imports) at session start.
- Added the **idle-unload sweep timer** (`spawn_idle_sweep` in `main.rs`) and fixed a latent
  clock bug — the runner was stamping `last_job_at_ms = 0`; now wall-clock epoch ms, consistent
  with the sweep. Removed the dead `base: Instant`.
- Wrote the **M5 gate harness**: `m5_budget_ceiling` (no admission > 7.0 GB; STT swap victim),
  `m5_wake_band` (~10/h hard ceiling protects voice), `m5_load_times` (on-target, `#[ignore]`).
- Determined **L2 min-residency is M6**, not M5 (`l2_swap_to_stt` is an explicit `todo!`).

**2. Full self-review of M4 + M5** (4 adversarial review agents). Fixed, all verified:

| ID | Sev | Fix |
|---|---|---|
| ORCH-1 | HIGH | Lost preempt/cancel via late `watch::subscribe` (voice starvation) → carry the original receiver from enqueue. `gpu_scheduler.rs` |
| VLM-H1 | HIGH | `capture_on=true` hardcoded let a VLM job re-spawn the sidecar *after* OFF (SC6 breach) → read `toggle().state()` under the lock. `pipeline.rs` |
| CONN-H1 | HIGH | `url:<host>` token dropped its domain → wrong-site bubble. Now host-matches via the tokenizer's `host_of`. `pipeline.rs` + `pattern-engine/normalizer.rs` |
| ORCH-2 | MED | `Notify` lost-wakeup burned the full OFF grace → `notify_one` (stores a permit). |
| NM-1 | MED | `forwarding` TOCTOU could persist a browser row / repopulate the URL cache post-OFF → `RwLock<bool>` held across `handle_message`. `nm_bridge.rs` |
| NM-2 | MED | Extension gate closed *last* in teardown (browser data flowed for ~3 s) → moved first. `toggle.rs` |
| — | LOW | Telemetry peak relabeled (projection, not measurement); gate constants single-sourced from `tier_router`; explicit `reject_remote_clients`; `externally_connectable` manifest; CONN-M3 lookup window 8→32. |

**The reviews verified CLEAN:** no network egress; pipe local-only + authed; bubble-gating
invariant holds (VLM never gates a bubble); FK integrity; single-mutex serialization + deadlock
freedom; budget admission never exceeds 7.0 GB; wake band exact.

---

## Deferred — carry into M6/M7 (each has an in-code `TODO`)

Ranked by priority. **CONN-M2 is the most user-visible — do it early.**

1. **CONN-M2 (MED) — the decay/mute ladder does NOT survive a restart.** The engine re-mines
   cold on boot (`dismiss_decay = 1.0`) and the flush clobbers the persisted value, so a
   dismissed suggestion re-nags after restart. Fix = hydrate the engine's pattern cache by
   `signature` at startup (needs a new `pattern-engine` hydrate method). `pipeline.rs` TODO(CONN-M2).
2. **L2 STT swap + 20 s min-residency (M6 core)** — `ModelLifecycle::l2_swap_to_stt` is `todo!`;
   `loaded_at_ms` is stamped and ready. M6's co-residency/swap gate needs this.
3. **ORCH-3 (MED) — crash-restart ladder unwired.** `handle_crash` (backoff + 3-strike +
   Degraded→OcrOnly/CpuWhisper) is implemented + tested but not called from `SidecarRunner::run`
   (load failure maps flat to `SidecarDown` — safe soft-degrade, not resilient). `gpu_scheduler.rs` TODO(M6).
4. **CONN-M1 (MED) — coalesce position clobber.** A later position-less navigation can overwrite
   a known media position → "resume from start". Fix = store `captured_ts` + a position-source
   rank in the coalesce map. `pipeline.rs` TODO(CONN-M1).
5. **LOW / on-hardware:** vlm-host JSON-schema/grammar decoding never actually requested (VLM-4,
   needs real llama.cpp grammar at the M5 hardware gate); dead `HostError::Deadline` (VLM-6);
   media `url:None` bypasses `url_pattern` (NM-3); unbounded pipe read vs the host's 256 KB cap
   (NM-4); non-constant-time token compare (NM-5); 7B inadmissible under the conservative seed
   table until remeasured (ORCH-5); idle-sweep on non-monotonic wall-clock, safe given ≤15 s
   deadlines << 60 s idle (ORCH-6).

---

## M6 — next up (Doc 07)

**Scope:** `stt-host` sidecar (faster-whisper small on GPU / whisper.cpp on CPU fallback) +
push-to-talk + intent classification + RAG retrieval (voice query → embed → KNN over stored OCR
→ grounded answer). **Gate (Doc 16):** SC4 (< 2 s for a 5–10 s utterance, GPU path); US2
end-to-end incl. the low-confidence confirm path; **L1 conditional co-residency AND L2 swap both
proven** (STT is the swap victim under image-VLM pressure, ADR-030).

**Start here:** M6 is the first milestone that actually exercises the STT slot, so wire the three
deferred lifecycle pieces FIRST — `l2_swap_to_stt` (+ min-residency), the crash-restart ladder,
and the warm-keep policy (≥ 2 PTT / 5 min pins STT, ADR-030/Q36; `set_warm_kept` already exists
and the idle sweep honors it). The scheduler already models STT as `priority::STT` (uncancellable,
preempts pattern-VLM) — the ORCH-1 fix makes that preemption reliable.

**Open questions (M6):** final STT model (faster-whisper small vs distil-large-v3-int8); the
low-confidence confirm threshold + UX; whether the warm-keep 2-PTT/5-min heuristic holds on real
usage.

## M7 — after M6 (Docs 09, 13)

**Scope:** the Reasoning Gateway + transparency gate; **both** MCP (pre-declared primary) and CLI
transports (ADR-025); the gated `aperture_search_history` UX (ADR-037). This is the ONLY crate
allowed to open a network socket / spawn the Claude CLI (invariant ②). **Gate:** SC5 **strict**
(zero user-data bytes until Send/scoped-allow; preview bytes == wire bytes by hash); updater
traffic distinguished + loopback whitelisted; transport fallback works with each disabled in turn;
US3 end-to-end.

**Open questions (M7):** MCP payload shape/size with context; the gated-search UX decision; the
CLI's documented headless caveats.

---

## How This User Works (apply from your FIRST response)

Rajeev is **AuDHD (autistic + ADHD) — this is an accessibility requirement, not a preference.**
- **Answer first.** Conclusion in the first line; reasoning after.
- **Short chunks, bullets over paragraphs.** Bold the one load-bearing line. No walls of text.
- **One question at a time, max.** Never a list of questions.
- **No filler.** No "great question", no apology spirals, no throat-clearing.
- **Recommendation, then alternatives** — not a flat menu. End every response with a concrete next step.
- Competent peer (Rust, Python, agentic AI, MCP, RAG, llama.cpp) — don't over-explain fundamentals;
  push back directly when he's wrong. Swearing / dry humour fine. Follow tangents, then offer to refocus.

---

## Practical notes

- **Windows linker flakiness:** parallel `cargo test` relinks intermittently throw
  `LNK1104: cannot open file …exe` — that's Windows Defender holding the freshly-linked binary,
  NOT a code error. Retry (the failing set shrinks as artifacts cache) or run per-crate.
- Docs are living: `docs/16` now has an "Implementation status (2026-07-06)" section; measured
  numbers replace the seed `[VERIFY]` figures as the on-hardware gates run.
- The authoritative design is Docs 00–21 (R2). Key ADRs in play for M6/M7: ADR-030 (7.0 GB /
  co-resident / swap), ADR-032 (wake band), ADR-024 (STT model), ADR-025 (transports), ADR-037
  (gated search), ADR-036/028 (SC5 harness).
