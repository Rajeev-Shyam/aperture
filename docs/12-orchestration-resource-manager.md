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
- **Telemetry** — local-only counters (wake rate, queue waits, VRAM peaks) feeding the M-gates.

## 3. GPU job queue semantics
```rust
struct GpuJob { kind: Vlm|Stt, priority: u8, payload: JobPayload,
                deadline: Duration, cancel: CancellationToken }
// priorities: STT(voice)=100 > user-requested VLM(enrichment "add screen summary")=80 > pattern-VLM=50
```
- **Admission:** BudgetEnforcer must pass (else degrade ladder, Doc 04 R3).
- **Execution:** holder of the mutex runs to completion or cancellation point; `gpu_busy=true` for the duration.
- **Preemption:** a higher-priority arrival cancels a *cancellable* lower job (pattern-VLM is always cancellable; STT never is). In **L2**, an STT arrival additionally triggers the unload(7B)→load(whisper) swap; the swap time is charged to the STT job's "thinking" UI (Doc 07 §6).
- **Deadlines:** VLM 10 s, STT 15 s [ASSUMPTION]; expiry cancels + logs, never retries in a loop.

## 4. The projection check (Doc 04 R1, operationalized)
```
projected = weights(model) + mmproj(model) + kv_est(ctx_tokens) + img_act(n_images) + framework
admit iff projected ≤ 7.2 GB        // 0.8 GB margin under the 8 GB ceiling
```
Per-model parameter table seeded from Doc 04 §2 and **overwritten by M5 measurements** — the enforcer runs on measured numbers, not estimates, after that gate.

## 5. Sidecar management (why processes — Doc 02 §2)
Spawn with pinned ports/pipes; readiness = health endpoint OK; crash ⇒ restart with exponential backoff (max 3, then mark degraded and fall back: VLM→OCR-only, STT→CPU whisper). **Kill is the unload primitive** — process death is the only guaranteed VRAM release, which is what makes SC6 and R1 enforceable rather than aspirational. [VERIFY exact server binaries/flags.]

## 6. Toggle-OFF sequence (the 3 s SLA, end-to-end)
1. ToggleOwner flips state → broadcast `capture_off`.
2. Capture subsystem executes its release steps (Doc 05 §5).
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
