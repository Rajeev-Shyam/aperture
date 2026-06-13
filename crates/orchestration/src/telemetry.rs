//! Local-only orchestration counters that feed the M-gates (doc 12 §2, doc 16).
//!
//! Invariant (2): these counters **never leave the machine** — they are read by
//! the M-gate harnesses (doc 04 §9, doc 16) and the local degrade/notice logic
//! only. Nothing here may touch the network; that path belongs solely to the
//! reasoning gateway (two-emitter rule, doc 13 §2).
//!
//! Tracked: VLM **wake rate** (target < 6 wakes/h, doc 06 §4), GPU **queue
//! waits**, and **VRAM peaks** — the numbers each M-gate asserts on (doc 16
//! M1/M5/M6).

use std::time::Duration;

/// A point-in-time snapshot of the local counters, handed to the M-gate
/// harnesses (doc 16). Plain data; no I/O.
#[derive(Debug, Clone, Default)]
pub struct TelemetrySnapshot {
    /// VLM wakes in the trailing hour; the M5 gate asserts `< 6` (doc 06 §4).
    pub vlm_wakes_last_hour: u32,
    /// Jobs admitted (passed the R1 projection) since start.
    pub jobs_admitted: u64,
    /// Jobs refused at the terminal R3 rung (doc 04 R3).
    pub jobs_refused: u64,
    /// Lower-priority jobs cancelled by preemption (doc 12 §3).
    pub jobs_preempted: u64,
    /// Mean time a job waited in the queue before the mutex was granted.
    pub mean_queue_wait: Duration,
    /// Peak observed VRAM use (GB); the M5 gate asserts no admission ever
    /// exceeded 7.2 GB projected (doc 16 M5).
    pub vram_peak_gb: f32,
    /// Toggle-OFF SLA breaches (release took > 3 s, doc 12 §6); surfaced once.
    pub toggle_sla_breaches: u32,
}

/// Accumulates the local counters. Cheap, in-process, lock-light; safe to update
/// from the scheduler hot path (doc 12 §1 — orchestration cost is "negligible").
#[derive(Debug, Default)]
pub struct Telemetry {
    // TODO(M5:) AtomicU64 counters + a small ring buffer of wake timestamps for
    // the trailing-hour rate; a histogram (or running mean) for queue waits.
}

impl Telemetry {
    /// Fresh, all-zero counters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a VLM wake with its reason (doc 06 §4 logs reasons for tuning).
    pub fn record_vlm_wake(&self, _reason: &'static str) {
        // TODO(M5:) push now() into the wake ring buffer; reason -> a per-reason tally.
        todo!("M5: record VLM wake for the < 6 wakes/h gate (doc 06 §4)")
    }

    /// Record a job's queue wait once the mutex is granted.
    pub fn record_queue_wait(&self, _waited: Duration) {
        // TODO(M5:) fold into the running mean / histogram.
        todo!("M5: record GPU queue wait (doc 12 §2)")
    }

    /// Record a sampled VRAM reading (the doc 04 §9 nvidia-smi sampler), keeping
    /// the running peak (doc 16 M5).
    pub fn record_vram_sample(&self, _used_gb: f32) {
        // TODO(M5:) update vram_peak_gb = max(peak, used_gb).
        todo!("M5: record VRAM sample; keep peak for the M5 gate (doc 04 §9)")
    }

    /// Record a toggle-OFF SLA breach (release exceeded 3 s, doc 12 §6).
    pub fn record_toggle_sla_breach(&self) {
        // TODO(M1:) increment; SC6 is a permanent CI test from M1 on (doc 16).
        todo!("M1: record toggle-OFF SLA breach (doc 12 §6, SC6)")
    }

    /// Snapshot the counters for an M-gate harness (doc 16).
    pub fn snapshot(&self) -> TelemetrySnapshot {
        // TODO(M5:) load the atomics + compute the trailing-hour wake rate.
        todo!("M5: snapshot local counters for the M-gates (doc 16)")
    }
}
