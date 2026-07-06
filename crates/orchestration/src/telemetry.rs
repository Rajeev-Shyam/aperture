//! Local-only orchestration counters that feed the M-gates (doc 12 §2, doc 16).
//!
//! Invariant (2): these counters **never leave the machine** — they are read by
//! the M-gate harnesses (doc 04 §9, doc 16) and the local degrade/notice logic
//! only. Nothing here may touch the network; that path belongs solely to the
//! reasoning gateway (two-emitter rule, doc 13 §2). These counters (rates only,
//! never content) are also the ONLY thing the opt-in diagnostics path may carry,
//! and only via the gateway crate (ADR-036) — this crate itself never emits.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// The trailing window for the VLM wake-rate snapshot (matches the tier router).
const RATE_WINDOW_MS: i64 = 60 * 60 * 1000;

/// A point-in-time snapshot of the local counters, handed to the M-gate
/// harnesses (doc 16). Plain data; no I/O.
#[derive(Debug, Clone, Default)]
pub struct TelemetrySnapshot {
    /// VLM wakes in the trailing hour; the M5 gate asserts the adaptive 3–10/h
    /// band with its hard ceiling (ADR-032).
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
    /// exceeded 7.0 GB projected, counting co-resident weights (doc 16 M5, ADR-030).
    pub vram_peak_gb: f32,
    /// Toggle-OFF SLA breaches (release took > 3 s, doc 12 §6); surfaced once.
    pub toggle_sla_breaches: u32,
}

/// Accumulates the local counters. Cheap, in-process, lock-light; safe to update
/// from the scheduler hot path (doc 12 §1 — orchestration cost is "negligible").
#[derive(Debug, Default)]
pub struct Telemetry {
    jobs_admitted: AtomicU64,
    jobs_refused: AtomicU64,
    jobs_preempted: AtomicU64,
    toggle_sla_breaches: AtomicU32,
    /// Running mean of queue waits: (total_ns, count).
    queue_wait_ns_total: AtomicU64,
    queue_wait_count: AtomicU64,
    /// Peak admission *projection* in milli-GB (f32 has no atomic; store x1000).
    /// A conservative upper bound, not a measurement — see
    /// [`Telemetry::record_admission_projection`].
    vram_peak_mgb: AtomicU32,
    /// Wake timestamps (epoch ms) in the trailing hour.
    wake_ledger: Mutex<std::collections::VecDeque<i64>>,
}

impl Telemetry {
    /// Fresh, all-zero counters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a VLM wake at `now_ms` (doc 06 §4 logs reasons; the reason is
    /// tracked by the tier router, the rate by the snapshot here).
    pub fn record_vlm_wake(&self, now_ms: i64) {
        let mut ledger = self.wake_ledger.lock().expect("wake ledger");
        ledger.push_back(now_ms);
        while let Some(&front) = ledger.front() {
            if now_ms - front >= RATE_WINDOW_MS {
                ledger.pop_front();
            } else {
                break;
            }
        }
    }

    /// A job passed admission (R1) and ran.
    pub fn record_admitted(&self) {
        self.jobs_admitted.fetch_add(1, Ordering::Relaxed);
    }

    /// A job was refused at the terminal R3 rung (doc 04 R3).
    pub fn record_refused(&self) {
        self.jobs_refused.fetch_add(1, Ordering::Relaxed);
    }

    /// A lower-priority cancellable job was preempted (doc 12 §3).
    pub fn record_preempted(&self) {
        self.jobs_preempted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a job's queue wait once the mutex is granted.
    pub fn record_queue_wait(&self, waited: Duration) {
        self.queue_wait_ns_total
            .fetch_add(waited.as_nanos() as u64, Ordering::Relaxed);
        self.queue_wait_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record the admission-time VRAM **projection** for an admitted job, keeping
    /// the running peak (doc 16 M5). This is the enforcer's conservative upper
    /// bound — guaranteed ≤ 7.0 GB by admission — so it is NOT a measurement, and
    /// the M5 ceiling gate must not read it as "observed VRAM never exceeded 7.0"
    /// (that assertion would be vacuous). Real measured VRAM comes from the
    /// on-target nvidia-smi harness (`crates/gates/tests/m5_load_times.rs`, doc 04
    /// §9). Lock-free max via compare-and-swap.
    pub fn record_admission_projection(&self, projected_gb: f32) {
        let sample = (projected_gb * 1000.0).round().max(0.0) as u32;
        let mut cur = self.vram_peak_mgb.load(Ordering::Relaxed);
        while sample > cur {
            match self.vram_peak_mgb.compare_exchange_weak(
                cur,
                sample,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
    }

    /// Record a toggle-OFF SLA breach (release exceeded 3 s, doc 12 §6, SC6).
    pub fn record_toggle_sla_breach(&self) {
        self.toggle_sla_breaches.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the counters for an M-gate harness (doc 16). `now_ms` bounds the
    /// trailing-hour wake window.
    pub fn snapshot(&self, now_ms: i64) -> TelemetrySnapshot {
        let vlm_wakes_last_hour = {
            let mut ledger = self.wake_ledger.lock().expect("wake ledger");
            while let Some(&front) = ledger.front() {
                if now_ms - front >= RATE_WINDOW_MS {
                    ledger.pop_front();
                } else {
                    break;
                }
            }
            ledger.len() as u32
        };
        let count = self.queue_wait_count.load(Ordering::Relaxed);
        let mean_queue_wait = if count > 0 {
            Duration::from_nanos(self.queue_wait_ns_total.load(Ordering::Relaxed) / count)
        } else {
            Duration::ZERO
        };
        TelemetrySnapshot {
            vlm_wakes_last_hour,
            jobs_admitted: self.jobs_admitted.load(Ordering::Relaxed),
            jobs_refused: self.jobs_refused.load(Ordering::Relaxed),
            jobs_preempted: self.jobs_preempted.load(Ordering::Relaxed),
            mean_queue_wait,
            vram_peak_gb: self.vram_peak_mgb.load(Ordering::Relaxed) as f32 / 1000.0,
            toggle_sla_breaches: self.toggle_sla_breaches.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_snapshot() {
        let t = Telemetry::new();
        t.record_admitted();
        t.record_admitted();
        t.record_refused();
        t.record_preempted();
        t.record_queue_wait(Duration::from_millis(100));
        t.record_queue_wait(Duration::from_millis(300));
        t.record_admission_projection(5.5);
        t.record_admission_projection(6.47);
        t.record_admission_projection(4.0); // peak stays at 6.47
        t.record_toggle_sla_breach();

        let s = t.snapshot(0);
        assert_eq!(s.jobs_admitted, 2);
        assert_eq!(s.jobs_refused, 1);
        assert_eq!(s.jobs_preempted, 1);
        assert_eq!(s.mean_queue_wait, Duration::from_millis(200));
        assert!((s.vram_peak_gb - 6.47).abs() < 0.01);
        assert_eq!(s.toggle_sla_breaches, 1);
    }

    #[test]
    fn wake_rate_windows_to_the_trailing_hour() {
        let t = Telemetry::new();
        t.record_vlm_wake(0);
        t.record_vlm_wake(30 * 60_000); // +30 min
        assert_eq!(t.snapshot(59 * 60_000).vlm_wakes_last_hour, 2);
        // At +61 min the first wake (t=0) has aged out.
        assert_eq!(t.snapshot(61 * 60_000).vlm_wakes_last_hour, 1);
    }
}
