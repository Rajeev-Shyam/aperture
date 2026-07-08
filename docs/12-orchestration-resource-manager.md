# Doc 12 — Orchestration & Resource Manager

## 1. Role & interface
The "brain": owns the capture toggle, the GPU mutex + job queue, sidecar lifecycles, the VRAM projection check, and tier routing. **No component touches the GPU or a sidecar directly; no component but the gateway touches the network.**
| | |
|---|---|
| **Inputs** | GPU job requests (Docs 06/07), toggle commands (tray/UI), settings (loadout L1/L2, timers), sidecar health |
| **Outputs** | Job results to callers; `gpu_busy` signal (Doc 14/11); toggle broadcasts + indicator state; `capture_toggle` audit events |
| **Resource cost** | Negligible itself — it exists to keep everything else under the ceiling |

## 2. Subcomponents
- **ToggleOwner** — single writer of capture state (Doc 02 §7); executes the OFF sequence (§6).
- **GpuScheduler** — priority queue + the single-permit mutex.
- **ModelLifecycle** — spawns/kills `vlm-host` / `stt-host` sidecars; health pings; idle-unload timers; warm-keep policy (Doc 04 §5).
- **BudgetEnforcer** — the R1 projection check before every load/job (§4).
- **TierRouter** — applies Doc 06's wake gate; routes explicit reasoning to the gateway (never the proactive loop).
- **Telemetry** — local-only counters (wake rate, queue waits, VRAM peaks, click-through rates) feeding the M-gates. These counters are the **only** thing the opt-in, off-by-default **diagnostics** path may send, and only **via the gateway crate** (aggregate-only, never content, audited like `cloud_send`) — ADR-036/Q89.

## 3. GPU job queue semantics
```rust
struct GpuJob { kind: Vlm|Stt, priority: u8, payload: JobPayload,
                deadline: Duration, cancel: CancellationToken }
// four-tier priorities (ADR-031):
//   STT(voice)=100 > user-VLM(waiting on the answer now)=80
//   > enrichment-VLM("add screen summary" while composing a payload)=70 > pattern-VLM=50
```
- **Admission:** BudgetEnforcer must pass (else degrade ladder, Doc 04 R3).
- **Execution:** holder of the mutex runs to completion or cancellation point; `gpu_busy=true` for the duration.
- **Preemption:** a higher-priority arrival cancels a *cancellable* lower job (pattern-VLM is always cancellable; STT never is). In **L2**, an STT arrival additionally triggers the unload(7B)→load(whisper) swap; the swap time is charged to the STT job's "thinking" UI (Doc 07 §6).
- **Deadlines:** VLM 10 s, STT 15 s are **interim** figures — the real deadlines are **set by the M5/M6 measured cold-load + inference times** (ADR-031/Q33); expiry cancels + logs, never retries in a loop.

## 4. The projection check (Doc 04 R1, operationalized)
```
projected = active(weights + mmproj + kv_est(ctx_tokens) + img_act(n_images))
          + framework + co_resident_weights            // ADR-030: co-resident weights ARE counted
admit iff projected ≤ 7.0 GB        // 1.0 GB margin under the 8 GB ceiling
```
Per-model parameter table seeded from Doc 04 §2 and **overwritten by M5 measurements** — the enforcer runs on measured numbers, not estimates, after that gate.

**Co-residency & swap rules (ADR-030):** under image-VLM memory pressure the enforcer **unloads faster-whisper (the swap victim)** before admitting the job; it reloads on the next PTT. A **warm-kept STT** (Doc 04 §5) is **protected from pattern-VLM** (prio 50 — which degrades to OCR-text-only rather than evict STT) **but yields to a user/enrichment image-VLM** (prio 80/70). True co-residency during a VLM image job is the exception, not the rule (Doc 04 FIX 4.1); the ceiling holds by construction.

## 5. Sidecar management (why processes — Doc 02 §2)
Spawn with pinned ports/pipes; readiness = health endpoint OK; crash ⇒ restart with exponential backoff (max 3, then mark degraded and fall back: VLM→OCR-only, STT→CPU whisper.cpp). On **persistent** failure, surface a **one-time "reduced mode" notice** and expose sidecar health in the **Activity & Privacy view** (ADR-040/Q93). **Kill is the unload primitive** — process death is the only guaranteed VRAM release, which is what makes SC6 and R1 enforceable rather than aspirational. [VERIFY exact server binaries/flags.]

## 6. Toggle-OFF sequence (the 3 s SLA, end-to-end)
1. ToggleOwner flips state → broadcast `capture_off`.
2. Capture subsystem executes its release steps (Doc 05 §5) — including **signalling the native-messaging host to stop forwarding browser-extension data** (ADR-027 / FIX 2.1), so OFF halts *all* capture sources, not just WGC/UIA.
3. GpuScheduler cancels queued jobs; running job gets 1 s to cancel, else proceed to 4.
4. ModelLifecycle **kills both sidecars** (no graceful-drain on OFF — the SLA wins).
5. Indicator flips (tray + overlay dot); `capture_toggle{off}` audit event written.
6. Watchdog samples `nvidia-smi`-equivalent; SLA breach is logged and surfaced once.
ON reverses lazily: hooks + sampler immediately; sidecars stay down until first demanded.

## 7. Failure modes
| Failure | Behavior |
|---|---|
| Deadlock risk | Single mutex + deadlines + cancellable jobs ⇒ no hold-and-wait cycle exists by construction |
| Load thrash (alternating VLM/STT demand in L2) | Debounce swaps (min residency 20 s [ASSUMPTION]); recommend L1 in a notice if thrash persists |
| Sidecar zombie (kill fails) | Escalate to `TerminateProcess`; refuse new loads until VRAM telemetry confirms release |
| Projection table wrong (driver/runtime change) | A real OOM from a sidecar ⇒ mark that model's row "unmeasured", re-run the M5 measurement harness, conservative-cap meanwhile |
| Settings flip L1→L2 mid-session | Treated as: unload all → admit next job under the new loadout's rules |

---
## Implementation status (2026-07-08) — M6 orchestration wiring

- **L2 STT swap wired end-to-end (§3).** `ModelLifecycle::l2_swap_to_stt` evicts the exclusive VLM and returns `SwapOutcome::Swapped { thrash_risk }` (flags an eviction inside the 20 s min-residency). The **scheduler now invokes it**: `admit_and_run`, on an STT job refused because the 7B is resident, evicts + re-admits so **voice is never starved**. Proven by `m6_l2_swap` (enforcer isolation) **and** a scheduler-level test (eviction + admission) — the latter was added after review found the swap was implemented but orphaned.
- **Crash-restart ladder wired (§5).** `gpu_scheduler::acquire_endpoint` routes a cold-load failure through `handle_crash` (backoff + 3-strike + Degraded→OcrOnly/CpuWhisper); one respawn per job, the 3-strike counter persists across jobs.
- **Warm-keep (§7 / ADR-030).** `warm_keep::PttWarmKeep` (≥2 PTT/5 min) computes the pin; the idle sweep honors `set_warm_kept`. Driving it from the PTT path is composition-root wiring (pending).

Full session detail: `docs/handoff/session-bridge-2026-07-08-m6-m8.md`.

---
> **R2 amendments applied** (see docs/19–21): ADR-031 (four-tier priorities: STT 100 > user-VLM 80 > enrichment-VLM 70 > pattern-VLM 50; deadlines measured at M5/M6), ADR-030 (7.0 GB cap + co_resident_weights in the projection; STT swap victim; warm-kept-STT protection rule), ADR-036/Q89 (diagnostics via the gateway only, opt-in/aggregate/audited), ADR-040/Q93 (reduced-mode notice + health in the Activity & Privacy view), ADR-027/FIX 2.1 (toggle-OFF halts extension forwarding). Min-residency 20 s unchanged (Q37).
