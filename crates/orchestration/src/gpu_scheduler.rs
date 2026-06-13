//! The GPU mutex + priority queue (doc 12 §3, contract 4 / doc 15 §4).
//!
//! Invariant (1): a **single-permit** [`tokio::sync::Semaphore`] guards *execution*
//! on the GPU — the resource manager is its only issuer (doc 04 §4). Holding the
//! permit drives the `gpu_busy` broadcast `true`; releasing it drives it `false`
//! (doc 11 §6, doc 14's degrade contract keys off it).
//!
//! Queue semantics (doc 12 §3):
//! - **Priorities:** STT(voice)=100 > user-VLM=80 > pattern-VLM=50
//!   (`aperture_contracts::gpu_job::priority`).
//! - **Admission:** the [`crate::budget_enforcer`] R1 check must pass, else the
//!   R3 degrade ladder (doc 04 §6).
//! - **Preemption:** a higher-priority arrival cancels a *cancellable* lower job
//!   at its cancel point — pattern-VLM(50) is always cancellable; **STT(100) is
//!   never cancellable** (doc 12 §3).
//! - **Deadlines:** VLM 10 s, STT 15 s [ASSUMPTION]; expiry cancels + logs,
//!   **never** retries in a loop (doc 12 §3).
//! - No hold-and-wait cycle exists by construction: single mutex + deadlines +
//!   cancellable jobs (doc 12 §7).

use std::cmp::Ordering;

use aperture_contracts::{GpuJob, JobError, JobOutput};
use tokio::sync::broadcast;

/// A queued job plus the scheduling metadata the [`std::collections::BinaryHeap`]
/// orders on. Higher `priority` first; ties broken by earlier `seq` (FIFO within
/// a priority band, doc 12 §3).
pub struct QueuedJob {
    pub job: GpuJob,
    /// Monotonic admission sequence — the FIFO tiebreaker within a priority.
    pub seq: u64,
    // cancel: tokio::sync::watch::Sender<bool>, // the job's cancel point (doc 12 §3)
    // respond_to: tokio::sync::oneshot::Sender<Result<JobOutput, JobError>>,
}

// --- Max-heap ordering: by priority desc, then seq asc (earlier wins) (doc 12 §3) ---
impl PartialEq for QueuedJob {
    fn eq(&self, other: &Self) -> bool {
        self.job.priority == other.job.priority && self.seq == other.seq
    }
}
impl Eq for QueuedJob {}
impl Ord for QueuedJob {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority is "greater" (pops first). Equal priority: smaller seq
        // is "greater" so the earlier-admitted job pops first (FIFO).
        self.job
            .priority
            .cmp(&other.job.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for QueuedJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The single-permit GPU scheduler (doc 12 §3). Owns the execution mutex, the
/// priority queue, and the `gpu_busy` broadcast. Constructed and driven by
/// [`crate::OrchestratedSystem`]; `enqueue` is exposed through the
/// `aperture_contracts::GpuScheduler` impl on that facade.
pub struct GpuScheduler {
    /// Single permit == the GPU execution mutex (doc 04 §4, invariant 1).
    permit: tokio::sync::Semaphore,
    /// `true` while the permit is held (doc 11 §6, doc 14). Latched on a
    /// broadcast so late subscribers can read the current value.
    gpu_busy_tx: broadcast::Sender<bool>,
    // queue: Mutex<BinaryHeap<QueuedJob>>,
    // running: Mutex<Option<RunningJob>>, // for preemption (doc 12 §3)
    // next_seq: AtomicU64,
}

impl GpuScheduler {
    /// Build a fresh scheduler with the single execution permit released and
    /// `gpu_busy=false`.
    pub fn new() -> Self {
        let (gpu_busy_tx, _) = broadcast::channel(16);
        Self {
            permit: tokio::sync::Semaphore::new(1), // exactly one (invariant 1)
            gpu_busy_tx,
        }
    }

    /// Subscribe to the `gpu_busy` signal (doc 11 §6, doc 14 §5). Re-exported by
    /// [`crate::OrchestratedSystem::gpu_busy`].
    pub fn subscribe_busy(&self) -> broadcast::Receiver<bool> {
        self.gpu_busy_tx.subscribe()
    }

    /// Admit + run a job: project budget (R1), enqueue by priority, acquire the
    /// permit (preempting a cancellable lower job if this one outranks it), run
    /// under the deadline, and return the result. This is the body of the
    /// `GpuScheduler::enqueue` contract method (doc 15 §4).
    pub async fn enqueue(&self, _job: GpuJob) -> Result<JobOutput, JobError> {
        // TODO(M5:) end-to-end:
        //   1. BudgetEnforcer::project; on fail walk R3 -> JobError::BudgetRefused{projection_gb}.
        //   2. push QueuedJob{seq} onto the BinaryHeap.
        //   3. if running job outranks-by < this.priority AND running is cancellable
        //      (pattern-VLM only; STT never) -> signal its cancel point (doc 12 §3).
        //   4. acquire the single permit -> set_busy(true); ensure the model is loaded
        //      (ModelLifecycle::spawn_sidecar / L2 swap), then run.
        //   5. race the run against tokio::time::timeout(job.deadline):
        //        Vlm 10 s / Stt 15 s; expiry -> cancel + log -> JobError::Deadline (never loop).
        //   6. on permit drop -> set_busy(false); telemetry.record_queue_wait(..).
        //   M6 wires the STT path + L2 swap (doc 16 M6).
        todo!("M5: admit/queue/preempt/deadline/run; broadcast gpu_busy (doc 12 §3)")
    }

    /// Cancel every *queued* job and give the running job 1 s to hit its cancel
    /// point — step 3 of the toggle-OFF sequence (doc 12 §6). A running STT is
    /// uncancellable, so OFF proceeds to the sidecar kill (step 4) regardless.
    pub async fn cancel_all_for_off(&self) {
        // TODO(M1/M5:) drain the heap (each respond_to -> JobError::Cancelled);
        //   signal the running job's cancel point with a 1 s grace, then return so
        //   ModelLifecycle::kill_all_sidecars runs (doc 12 §6). SLA wins.
        todo!("M1: cancel queued + 1 s grace for running, then proceed to kill (doc 12 §6)")
    }

    /// Latch and broadcast the busy state (held permit == busy, doc 11 §6).
    fn set_busy(&self, busy: bool) {
        // Ignore send errors: no subscribers yet is fine (latched on next subscribe).
        let _ = self.gpu_busy_tx.send(busy);
    }

    /// Permit count, for the deadlock-freedom assertion in tests (doc 12 §7).
    #[doc(hidden)]
    pub fn available_permits(&self) -> usize {
        self.permit.available_permits()
    }
}

impl Default for GpuScheduler {
    fn default() -> Self {
        Self::new()
    }
}
