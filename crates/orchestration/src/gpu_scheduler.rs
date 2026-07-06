//! The GPU mutex + priority queue (doc 12 §3, contract 4 / doc 15 §4).
//!
//! Invariant (1): **at most one job executes at a time** — enforced by the
//! `running` slot in [`SchedState`] (a logical single-permit mutex). Holding it
//! drives the `gpu_busy` broadcast `true`; releasing it drives it `false` (doc 11
//! §6, doc 14's degrade contract keys off it).
//!
//! Queue semantics (doc 12 §3):
//! - **Priorities (four tiers, ADR-031):** STT(voice)=100 > user-VLM=80 >
//!   enrichment-VLM=70 > pattern-VLM=50 (`aperture_contracts::gpu_job::priority`).
//! - **Admission:** the [`crate::budget_enforcer`] R1 check must pass, walking the
//!   R3 degrade ladder; the terminal rung yields `JobError::BudgetRefused` (doc 04 §6).
//! - **Preemption:** a higher-priority arrival cancels a *cancellable* lower job
//!   — pattern-VLM(50) is cancellable; user/enrichment/STT are not (doc 12 §3).
//! - **Deadlines:** interim VLM 10 s / STT 15 s — real deadlines are the M5/M6
//!   measured times (ADR-031/Q33); expiry cancels + logs, **never** loops.
//! - No hold-and-wait cycle exists by construction: one running slot + deadlines
//!   + cancellable jobs (doc 12 §7).
//!
//! Execution is behind a [`JobRunner`] seam so the admission/queue/preempt/
//! deadline machinery is testable without a GPU (real = [`SidecarRunner`] talks
//! to `vlm-host` over loopback; tests use a fake with controllable latency).

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use aperture_contracts::gpu_job::priority;
use aperture_contracts::{GpuJob, GpuJobKind, JobError, JobOutput};
use tokio::sync::{broadcast, oneshot, watch, Mutex as TokioMutex, Notify};
use tokio::time::Instant;

use crate::budget_enforcer::{Admission, BudgetEnforcer, LoadRequest};
use crate::model_lifecycle::{ModelLifecycle, SidecarKind};
use crate::telemetry::Telemetry;
use crate::vram_table::ModelId;
use crate::Loadout;

/// Default VLM context for admission projection (the enforcer shrinks it down the
/// R3 ladder if the projection fails, doc 04 R3).
const DEFAULT_VLM_CTX: u32 = 8192;

/// A job cancellable by a higher-priority arrival (doc 12 §3): pattern-VLM only —
/// it never gates a bubble, so dropping it is free. user/enrichment VLM (the user
/// is waiting/composing) and STT (voice, never) are not cancellable.
fn is_cancellable(job_priority: u8) -> bool {
    job_priority <= priority::VLM_PATTERN
}

/// Executes an admitted job on `model`. Real = [`SidecarRunner`] (loopback HTTP
/// to `vlm-host`); tests = a fake with controllable latency/output. The scheduler
/// holds the single running slot for the call's duration and drop-cancels it on
/// preemption/deadline.
#[async_trait::async_trait]
pub trait JobRunner: Send + Sync {
    async fn run(
        &self,
        job: &GpuJob,
        model: ModelId,
        lifecycle: Arc<TokioMutex<ModelLifecycle>>,
    ) -> Result<JobOutput, JobError>;
}

/// A queued job plus the scheduling metadata the [`BinaryHeap`] orders on. Higher
/// `priority` first; ties broken by earlier `seq` (FIFO within a band, doc 12 §3).
struct Waiter {
    priority: u8,
    seq: u64,
    job: GpuJob,
    result_tx: oneshot::Sender<Result<JobOutput, JobError>>,
    cancel_tx: watch::Sender<bool>,
    /// The receiver created **with** the channel (baseline = the pre-send version),
    /// carried through to `run_one`. Re-`subscribe()`ing in `run_one` would race a
    /// preemptor's `send(true)` that lands first — the fresh receiver's baseline
    /// would already be the post-send version and `changed()` would never fire,
    /// silently losing the cancel (voice starvation, doc 12 §3). This receiver's
    /// baseline predates any preemption, so it always catches the send.
    cancel_rx: watch::Receiver<bool>,
    enqueued_at: Instant,
}

// Max-heap ordering: by priority desc, then seq asc (earlier wins).
impl PartialEq for Waiter {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}
impl Eq for Waiter {}
impl Ord for Waiter {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Waiter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The currently-running job's preemption metadata.
struct RunningMeta {
    priority: u8,
    cancellable: bool,
    cancel_tx: watch::Sender<bool>,
}

#[derive(Default)]
struct SchedState {
    heap: BinaryHeap<Waiter>,
    running: Option<RunningMeta>,
}

/// The shared scheduler state, held behind an `Arc` so dispatched runner tasks
/// can re-lock it to dispatch the next job on completion.
struct Inner {
    gpu_busy_tx: broadcast::Sender<bool>,
    state: StdMutex<SchedState>,
    enforcer: StdMutex<BudgetEnforcer>,
    lifecycle: Arc<TokioMutex<ModelLifecycle>>,
    runner: Arc<dyn JobRunner>,
    telemetry: Arc<Telemetry>,
    loadout: AtomicU8, // 0 = L1, 1 = L2
    next_seq: AtomicU64,
    /// Notified whenever the running slot clears (for the OFF-path grace wait).
    idle_notify: Notify,
}

/// The single-running-slot GPU scheduler (doc 12 §3). Owns the priority queue,
/// the logical execution mutex, and the `gpu_busy` broadcast.
pub struct GpuScheduler {
    inner: Arc<Inner>,
}

impl GpuScheduler {
    /// Build a scheduler over the given enforcer, lifecycle, runner, and telemetry.
    pub fn new(
        enforcer: BudgetEnforcer,
        lifecycle: Arc<TokioMutex<ModelLifecycle>>,
        runner: Arc<dyn JobRunner>,
        telemetry: Arc<Telemetry>,
        loadout: Loadout,
    ) -> Self {
        let (gpu_busy_tx, _) = broadcast::channel(16);
        Self {
            inner: Arc::new(Inner {
                gpu_busy_tx,
                state: StdMutex::new(SchedState::default()),
                enforcer: StdMutex::new(enforcer),
                lifecycle,
                runner,
                telemetry,
                loadout: AtomicU8::new(matches!(loadout, Loadout::L2) as u8),
                next_seq: AtomicU64::new(0),
                idle_notify: Notify::new(),
            }),
        }
    }

    /// Subscribe to the `gpu_busy` signal (doc 11 §6, doc 14 §5).
    pub fn subscribe_busy(&self) -> broadcast::Receiver<bool> {
        self.inner.gpu_busy_tx.subscribe()
    }

    /// Switch the active loadout (L1<->L2). Affects model selection for the next
    /// admitted job (doc 12 §7); resident sidecars are unloaded by the toggle
    /// owner's `apply_loadout_change`.
    pub fn set_loadout(&self, loadout: Loadout) {
        self.inner
            .loadout
            .store(matches!(loadout, Loadout::L2) as u8, AtomicOrdering::SeqCst);
    }

    /// Admit + run a job (contract 4, doc 15 §4): project budget (R1) walking the
    /// R3 ladder, enqueue by priority, run under the deadline in the single slot,
    /// preempting a cancellable lower job if outranked.
    pub async fn enqueue(&self, job: GpuJob) -> Result<JobOutput, JobError> {
        self.inner.enqueue(job).await
    }

    /// Cancel every *queued* job and give a running job up to
    /// [`crate::toggle_owner::RUNNING_JOB_GRACE`] to hit its cancel point — step 3
    /// of the toggle-OFF sequence (doc 12 §6). A running STT is uncancellable, so
    /// OFF proceeds to the sidecar kill (step 4) regardless. Returns once the slot
    /// is idle or the grace elapses.
    pub async fn cancel_all_for_off(&self, grace: Duration) {
        self.inner.cancel_all_for_off(grace).await;
    }

    /// Permit count analogue for the deadlock-freedom assertion (doc 12 §7):
    /// `1` when idle, `0` while a job runs.
    #[doc(hidden)]
    pub fn available_permits(&self) -> usize {
        usize::from(self.inner.state.lock().expect("sched state").running.is_none())
    }

    /// Test/measurement hook: overwrite a VRAM row (the M5 measurement harness,
    /// doc 12 §4) or conservative-cap after an OOM (doc 12 §7).
    pub fn with_enforcer_mut<R>(&self, f: impl FnOnce(&mut BudgetEnforcer) -> R) -> R {
        f(&mut self.inner.enforcer.lock().expect("enforcer"))
    }
}

/// Contract 4 (doc 15 §4): the scheduler is directly usable as the single GPU
/// entry point, so `aperture-vision-ocr`'s Layer B can hold an
/// `Arc<dyn GpuScheduler>` without routing through the whole `OrchestratedSystem`
/// facade (doc 06 §3). The facade also impls the trait (lib.rs) for callers that
/// only have it.
#[async_trait::async_trait]
impl aperture_contracts::GpuScheduler for GpuScheduler {
    async fn enqueue(&self, job: GpuJob) -> Result<JobOutput, JobError> {
        self.inner.enqueue(job).await
    }
}

impl Inner {
    async fn enqueue(self: &Arc<Self>, job: GpuJob) -> Result<JobOutput, JobError> {
        let seq = self.next_seq.fetch_add(1, AtomicOrdering::SeqCst);
        let (result_tx, result_rx) = oneshot::channel();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let priority = job.priority;
        let waiter = Waiter {
            priority,
            seq,
            job,
            result_tx,
            cancel_tx,
            cancel_rx,
            enqueued_at: Instant::now(),
        };
        {
            let mut st = self.state.lock().expect("sched state");
            // Preempt a running cancellable lower-priority job (doc 12 §3).
            if let Some(run) = &st.running {
                if run.cancellable && priority > run.priority {
                    let _ = run.cancel_tx.send(true);
                    self.telemetry.record_preempted();
                }
            }
            st.heap.push(waiter);
            self.dispatch(&mut st);
        }
        // The runner task fulfills the oneshot; a dropped sender = Cancelled.
        result_rx.await.unwrap_or(Err(JobError::Cancelled))
    }

    /// If idle and work is queued, claim the highest-priority job and spawn its
    /// runner. Called with `state` locked.
    fn dispatch(self: &Arc<Self>, st: &mut SchedState) {
        if st.running.is_some() {
            return;
        }
        let Some(waiter) = st.heap.pop() else {
            return;
        };
        st.running = Some(RunningMeta {
            priority: waiter.priority,
            cancellable: is_cancellable(waiter.priority),
            cancel_tx: waiter.cancel_tx.clone(),
        });
        let me = Arc::clone(self);
        tokio::spawn(async move { me.run_one(waiter).await });
    }

    async fn run_one(self: Arc<Self>, waiter: Waiter) {
        self.set_busy(true);
        self.telemetry.record_queue_wait(waiter.enqueued_at.elapsed());
        // Use the receiver carried from enqueue (its baseline predates any
        // preemption send) — re-subscribing here would silently lose a cancel
        // that landed before this task was polled.
        let mut cancel_rx = waiter.cancel_rx;
        let result = self.admit_and_run(&waiter.job, &mut cancel_rx).await;
        self.set_busy(false);
        let _ = waiter.result_tx.send(result);
        // Free the slot and dispatch the next-highest-priority waiter.
        {
            let mut st = self.state.lock().expect("sched state");
            st.running = None;
            self.dispatch(&mut st);
        }
        // `notify_one` STORES a permit if the OFF-path grace waiter hasn't
        // registered yet, so the running→idle transition can never be lost in the
        // window between `cancel_all_for_off` dropping the state lock and awaiting
        // (doc 12 §6). `notify_waiters` keeps no permit and would burn the full grace.
        self.idle_notify.notify_one();
    }

    async fn admit_and_run(
        &self,
        job: &GpuJob,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> Result<JobOutput, JobError> {
        // 1. Admission (R1 + R3 ladder). Co-resident set = currently loaded.
        let loaded = self.lifecycle.lock().await.loaded_models();
        let req = self.load_request(job, &loaded);
        let admission = {
            let enforcer = self.enforcer.lock().expect("enforcer");
            enforcer.admit(req, &loaded)
        };
        let plan = match admission {
            Admission::Refused { projection_gb } => {
                self.telemetry.record_refused();
                return Err(JobError::BudgetRefused { projection_gb });
            }
            Admission::Admit {
                plan,
                unload_stt,
                projected_gb,
                ..
            } => {
                self.telemetry.record_admitted();
                self.telemetry.record_admission_projection(projected_gb);
                if unload_stt {
                    // ADR-030: STT is the swap victim under image-VLM pressure.
                    let _ = self
                        .lifecycle
                        .lock()
                        .await
                        .kill_sidecar(SidecarKind::SttHost)
                        .await;
                }
                plan
            }
        };

        // 2. Run under the deadline, cancellable by preemption. select drops the
        //    losing future — a cancelled/timed-out inference is simply abandoned
        //    (never retried in a loop, doc 12 §3).
        let run_fut = self
            .runner
            .run(job, plan.model, Arc::clone(&self.lifecycle));
        tokio::pin!(run_fut);
        tokio::select! {
            biased;
            _ = cancel_rx.changed() => Err(JobError::Cancelled),
            r = tokio::time::timeout(job.deadline, &mut run_fut) => match r {
                Ok(inner) => inner,
                Err(_elapsed) => Err(JobError::Deadline),
            }
        }
    }

    /// Derive the admission [`LoadRequest`] from the job + active loadout.
    fn load_request(&self, job: &GpuJob, _loaded: &[ModelId]) -> LoadRequest {
        let l2 = self.loadout.load(AtomicOrdering::SeqCst) == 1;
        match &job.kind {
            GpuJobKind::Vlm { .. } => LoadRequest {
                model: if l2 { ModelId::Vlm7b } else { ModelId::Vlm3b },
                ctx_tokens: DEFAULT_VLM_CTX,
                n_images: 1,
            },
            GpuJobKind::Stt { .. } => LoadRequest {
                model: ModelId::FasterWhisperSmall,
                ctx_tokens: 0,
                n_images: 0,
            },
        }
    }

    async fn cancel_all_for_off(self: &Arc<Self>, grace: Duration) {
        {
            let mut st = self.state.lock().expect("sched state");
            // Drain queued jobs -> Cancelled (doc 12 §6 step 3).
            for waiter in st.heap.drain() {
                let _ = waiter.result_tx.send(Err(JobError::Cancelled));
            }
            // Signal the running job's cancel point (uncancellable STT ignores it;
            // the sidecar kill in step 4 reaps it regardless).
            if let Some(run) = &st.running {
                let _ = run.cancel_tx.send(true);
            }
            if st.running.is_none() {
                return;
            }
        }
        // Give the running job up to the grace to exit before the kill.
        let _ = tokio::time::timeout(grace, async {
            loop {
                self.idle_notify.notified().await;
                if self.state.lock().expect("sched state").running.is_none() {
                    return;
                }
            }
        })
        .await;
    }

    fn set_busy(&self, busy: bool) {
        let _ = self.gpu_busy_tx.send(busy);
    }
}

// ---------------------------------------------------------------------------
// The production runner: loopback HTTP to the vlm-host sidecar.
// ---------------------------------------------------------------------------

/// Runs jobs by talking to the `vlm-host` sidecar over loopback (doc 06 §3, doc
/// 12 §5). Ensures the model is loaded (spawning the sidecar on demand), POSTs
/// `/infer`, and parses the structured scene JSON.
pub struct SidecarRunner {
    client: reqwest::Client,
}

impl SidecarRunner {
    pub fn new() -> Self {
        Self {
            // The deadline is enforced by the scheduler's select; keep the client
            // timeout generous so a slow-but-alive infer isn't double-cut.
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for SidecarRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Wall-clock epoch ms — the clock the sidecar lifecycle stamps `last_job_at_ms`
/// with, shared with the shell's idle sweep (doc 04 §5) so the 60 s idle window
/// is measured against one clock. Distinct from the `tokio::time::Instant` used
/// for queue-wait / deadlines (those are durations, not absolute stamps).
fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[async_trait::async_trait]
impl JobRunner for SidecarRunner {
    async fn run(
        &self,
        job: &GpuJob,
        model: ModelId,
        lifecycle: Arc<TokioMutex<ModelLifecycle>>,
    ) -> Result<JobOutput, JobError> {
        // Wall-clock epoch ms — the SAME clock the shell's idle sweep passes to
        // `idle_sweep` (doc 04 §5), so `last_job_at_ms` deltas are coherent. A
        // monotonic `Instant` can't be shared across the two separate tasks; every
        // other lifetime in the system (retention TTLs, decay, sessions) is epoch-ms.
        let now_ms = now_epoch_ms();
        let endpoint = {
            let mut life = lifecycle.lock().await;
            // TODO(M6): route a load/exec failure through `ModelLifecycle::handle_crash`
            // (exponential backoff + 3-strike cap + Degraded→OcrOnly/CpuWhisper
            // fallback, doc 12 §5) instead of a flat `SidecarDown`. As written, a
            // sidecar that crashes on load is re-cold-loaded (paying the full timeout)
            // on every job with no backoff or health surfacing. The job outcome is
            // already soft (the caller degrades to OCR-only), so this is a resilience
            // gap, not a safety one — `handle_crash` is implemented + tested, just
            // not yet wired into this production run path.
            life.ensure_loaded(model, now_ms)
                .await
                .map_err(|_| JobError::SidecarDown)?
        };
        match &job.kind {
            GpuJobKind::Vlm { image_jpeg, prompt } => {
                let body = serde_json::json!({
                    "image_jpeg": image_jpeg,
                    "prompt": prompt,
                    "schema": serde_json::Value::Null,
                });
                let resp = self
                    .client
                    .post(format!("{endpoint}/infer"))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|_| JobError::SidecarDown)?;
                let value: serde_json::Value =
                    resp.json().await.map_err(|_| JobError::SidecarDown)?;
                Ok(JobOutput::Vlm(value))
            }
            // STT execution is M6 (doc 16); admission/routing already work.
            GpuJobKind::Stt { .. } => Err(JobError::SidecarDown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vram_table::VramTable;
    use std::sync::atomic::AtomicU32;

    /// A runner with controllable latency + output; records concurrent entries so
    /// tests can assert the single-slot mutex.
    struct FakeRunner {
        latency: Duration,
        concurrent: Arc<AtomicU32>,
        max_concurrent: Arc<AtomicU32>,
    }

    impl FakeRunner {
        fn new(latency: Duration) -> (Arc<Self>, Arc<AtomicU32>) {
            let max = Arc::new(AtomicU32::new(0));
            (
                Arc::new(Self {
                    latency,
                    concurrent: Arc::new(AtomicU32::new(0)),
                    max_concurrent: Arc::clone(&max),
                }),
                max,
            )
        }
    }

    #[async_trait::async_trait]
    impl JobRunner for FakeRunner {
        async fn run(
            &self,
            _job: &GpuJob,
            model: ModelId,
            _lifecycle: Arc<TokioMutex<ModelLifecycle>>,
        ) -> Result<JobOutput, JobError> {
            let now = self.concurrent.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            self.max_concurrent.fetch_max(now, AtomicOrdering::SeqCst);
            tokio::time::sleep(self.latency).await;
            self.concurrent.fetch_sub(1, AtomicOrdering::SeqCst);
            Ok(JobOutput::Vlm(serde_json::json!({ "model": format!("{model:?}") })))
        }
    }

    /// A lifecycle whose spawner never spawns a real process (loaded set stays
    /// empty; admission co-resident = []).
    fn fake_lifecycle() -> Arc<TokioMutex<ModelLifecycle>> {
        struct NoSpawn;
        #[async_trait::async_trait]
        impl crate::model_lifecycle::Spawner for NoSpawn {
            async fn spawn(
                &self,
                _m: ModelId,
            ) -> Result<Box<dyn crate::model_lifecycle::SidecarProcess>, crate::model_lifecycle::LifecycleError>
            {
                struct P;
                #[async_trait::async_trait]
                impl crate::model_lifecycle::SidecarProcess for P {
                    fn endpoint(&self) -> &str {
                        "http://127.0.0.1:0"
                    }
                    async fn is_ready(&self) -> bool {
                        true
                    }
                    async fn kill(
                        &mut self,
                    ) -> Result<(), crate::model_lifecycle::LifecycleError> {
                        Ok(())
                    }
                }
                Ok(Box::new(P))
            }
        }
        Arc::new(TokioMutex::new(ModelLifecycle::new(Box::new(NoSpawn))))
    }

    fn scheduler(runner: Arc<dyn JobRunner>) -> GpuScheduler {
        GpuScheduler::new(
            BudgetEnforcer::new(VramTable::seeded()),
            fake_lifecycle(),
            runner,
            Arc::new(Telemetry::new()),
            Loadout::L1,
        )
    }

    fn vlm_job(priority: u8, deadline: Duration) -> GpuJob {
        GpuJob {
            kind: GpuJobKind::Vlm {
                image_jpeg: vec![0u8; 8],
                prompt: "p".into(),
            },
            priority,
            deadline,
        }
    }

    #[tokio::test]
    async fn a_single_job_runs_and_returns() {
        let (runner, _) = FakeRunner::new(Duration::from_millis(10));
        let sched = scheduler(runner);
        let out = sched
            .enqueue(vlm_job(priority::VLM_PATTERN, Duration::from_secs(5)))
            .await;
        assert!(matches!(out, Ok(JobOutput::Vlm(_))));
    }

    #[tokio::test]
    async fn only_one_job_runs_at_a_time() {
        let (runner, max_concurrent) = FakeRunner::new(Duration::from_millis(30));
        let sched = Arc::new(scheduler(runner));
        let mut handles = Vec::new();
        for _ in 0..5 {
            let s = Arc::clone(&sched);
            handles.push(tokio::spawn(async move {
                s.enqueue(vlm_job(priority::VLM_PATTERN, Duration::from_secs(60)))
                    .await
            }));
        }
        for h in handles {
            assert!(h.await.unwrap().is_ok());
        }
        assert_eq!(
            max_concurrent.load(AtomicOrdering::SeqCst),
            1,
            "the single GPU mutex must serialize execution (invariant 1)"
        );
    }

    #[tokio::test]
    async fn deadline_expiry_cancels_the_job() {
        let (runner, _) = FakeRunner::new(Duration::from_secs(30));
        let sched = scheduler(runner);
        let out = sched
            .enqueue(vlm_job(priority::VLM_PATTERN, Duration::from_millis(50)))
            .await;
        assert!(matches!(out, Err(JobError::Deadline)));
    }

    #[tokio::test]
    async fn stt_preempts_a_running_cancellable_pattern_vlm() {
        let (runner, _) = FakeRunner::new(Duration::from_secs(10));
        let sched = Arc::new(scheduler(runner));

        // Start a long pattern-VLM (cancellable).
        let s1 = Arc::clone(&sched);
        let vlm = tokio::spawn(async move {
            s1.enqueue(vlm_job(priority::VLM_PATTERN, Duration::from_secs(60)))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // An STT arrival (priority 100) preempts it.
        let s2 = Arc::clone(&sched);
        let stt = tokio::spawn(async move {
            s2.enqueue(GpuJob {
                kind: GpuJobKind::Stt { wav: vec![0u8; 4] },
                priority: priority::STT_VOICE,
                deadline: Duration::from_secs(60),
            })
            .await
        });

        // The pattern-VLM is cancelled; the STT is admitted and runs (proving the
        // higher-priority voice job is never starved — doc 12 §3). The fake runner
        // completes it; real STT execution lands at M6.
        assert!(matches!(vlm.await.unwrap(), Err(JobError::Cancelled)));
        assert!(
            stt.await.unwrap().is_ok(),
            "STT preempts + runs — voice is never starved (doc 12 §3)"
        );
    }

    #[tokio::test]
    async fn higher_priority_queued_job_runs_first() {
        let (runner, _) = FakeRunner::new(Duration::from_millis(60));
        let sched = Arc::new(scheduler(runner));
        let order = Arc::new(StdMutex::new(Vec::new()));

        // Occupy the slot with a non-cancellable user-VLM so the next two queue.
        let s0 = Arc::clone(&sched);
        let blocker = tokio::spawn(async move {
            s0.enqueue(vlm_job(priority::VLM_USER, Duration::from_secs(60)))
                .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Enqueue a low then a high; the high must run first when the slot frees.
        let (s1, o1) = (Arc::clone(&sched), Arc::clone(&order));
        let low = tokio::spawn(async move {
            let r = s1
                .enqueue(vlm_job(priority::VLM_PATTERN, Duration::from_secs(60)))
                .await;
            o1.lock().unwrap().push("low");
            r
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let (s2, o2) = (Arc::clone(&sched), Arc::clone(&order));
        let high = tokio::spawn(async move {
            let r = s2
                .enqueue(vlm_job(priority::VLM_ENRICHMENT, Duration::from_secs(60)))
                .await;
            o2.lock().unwrap().push("high");
            r
        });

        let _ = blocker.await.unwrap();
        let _ = low.await.unwrap();
        let _ = high.await.unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["high", "low"], "priority order (doc 12 §3)");
    }

    #[tokio::test]
    async fn budget_refused_when_projection_exceeds_the_ceiling() {
        // Force L2 (7B); a lone 7B + image projects > 7.0 and the ladder drops it
        // to 3B — which fits. To get a genuine refusal, shrink the table so even
        // 3B can't fit, proving admission never runs an over-budget job.
        let (runner, _) = FakeRunner::new(Duration::from_millis(10));
        let sched = scheduler(runner);
        sched.with_enforcer_mut(|e| {
            e.table_mut().framework_gb = 100.0; // nothing can fit under 7.0
        });
        let out = sched
            .enqueue(vlm_job(priority::VLM_PATTERN, Duration::from_secs(5)))
            .await;
        assert!(
            matches!(out, Err(JobError::BudgetRefused { .. })),
            "an over-ceiling projection must refuse, never run (M5 invariant)"
        );
    }
}
