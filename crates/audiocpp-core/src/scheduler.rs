//! Work queue (design §5.3). Jobs are created one at a time when a worker asks, from the
//! oldest active run that still has work, so edits apply to every job that hasn't started
//! and the first free server always gets the next seed.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, oneshot};

use crate::api::{Timing, TranscribeResponse};
use crate::config::ModelSpec;
use crate::error::{Error, Result};
use crate::library::SongId;
use crate::params::GenerationParams;
use crate::run::{AbcSource, JobId, RunEdit, RunId, RunName, RunSpec, RunState, RunStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ServerId(pub usize);

impl std::fmt::Display for ServerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server {}", self.0)
    }
}

/// Maximum attempts before a job is `Failed` (§5.3).
pub const MAX_ATTEMPTS: u32 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub run_id: RunId,
    pub run_name: RunName,
    pub seed: u32,
    pub start_seed: u32,
    pub index: u32,
    pub revision: u32,
    pub params: GenerationParams,
    pub model: ModelSpec,
    pub abc_source: AbcSource,
    pub attempts: u32,
    /// Regenerate: the original stem; the take is saved as `<stem>-rN` (§6.2).
    pub regenerate_of: Option<String>,
}

impl Job {
    pub fn stem(&self) -> String {
        self.run_name.stem(self.seed)
    }

    pub fn request_body(&self) -> serde_json::Value {
        self.params.to_task_body(&self.model.id, self.seed)
    }
}

pub struct TranscribeTask {
    pub wav: PathBuf,
    pub model: ModelSpec,
    pub reply: oneshot::Sender<Result<TranscribeResponse>>,
}

pub enum WorkItem {
    Generate(Job),
    Transcribe(TranscribeTask),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobState {
    Queued,
    Running {
        server: ServerId,
        started: SystemTime,
        estimate_ms: Option<u64>,
    },
    /// Running, result will be discarded (§5.3 Cancel).
    Cancelling {
        server: ServerId,
        started: SystemTime,
        estimate_ms: Option<u64>,
    },
    Encoding,
    Done(SongId),
    Failed(String),
    /// Dropped before it ran, or discarded after it finished.
    Cancelled,
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Done(_) | JobState::Failed(_) | JobState::Cancelled
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: JobId,
    pub run_id: RunId,
    pub seed: u32,
    pub revision: u32,
    pub attempts: u32,
    pub state: JobState,
    pub timing: Option<Timing>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub state: RunState,
    /// Position in the queue (0 = first).
    pub position: usize,
    pub done: u32,
    pub failed: u32,
    pub running: u32,
    pub queued_retries: u32,
}

/// Outcome a worker reports for a job.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Response received and decoded; encoding runs on the blocking pool.
    Encoding(Timing),
    Done(SongId),
    /// Transport error, crash or timeout: requeue with the same seed and revision.
    Retry(String),
    /// 4xx or out of attempts.
    Failed(String),
    /// Cancelled while running; the result was thrown away.
    Discarded,
}

pub trait SchedulerEvents: Send + Sync {
    fn job(&self, info: JobInfo);
    fn run(&self, snap: RunSnapshot);
}

struct RunEntry {
    state: RunState,
    retries: VecDeque<Job>,
    running: u32,
    done: u32,
    failed: u32,
    regenerate_of: Option<String>,
}

impl RunEntry {
    fn has_work(&self) -> bool {
        self.state.status == RunStatus::Active
            && (!self.retries.is_empty() || self.state.has_new_seeds())
    }

    fn check_done(&mut self) -> bool {
        let no_more = match self.state.status {
            RunStatus::Stopping => true,
            RunStatus::Active => !self.state.has_new_seeds() && self.retries.is_empty(),
            _ => false,
        };
        if no_more && self.running == 0 && self.state.status != RunStatus::Done {
            self.state.status = RunStatus::Done;
            return true;
        }
        false
    }
}

struct Inner {
    runs: Vec<RunEntry>,
    jobs: BTreeMap<JobId, JobInfo>,
    job_runs: BTreeMap<JobId, Job>,
    next_job: u64,
    transcribe: VecDeque<TranscribeTask>,
}

pub struct Scheduler {
    inner: Mutex<Inner>,
    notify: Notify,
    events: Box<dyn SchedulerEvents>,
}

impl Scheduler {
    pub fn new(events: Box<dyn SchedulerEvents>) -> Self {
        Scheduler {
            inner: Mutex::new(Inner {
                runs: Vec::new(),
                jobs: BTreeMap::new(),
                job_runs: BTreeMap::new(),
                next_job: 1,
                transcribe: VecDeque::new(),
            }),
            notify: Notify::new(),
            events,
        }
    }

    fn snapshot(runs: &[RunEntry], i: usize) -> RunSnapshot {
        let r = &runs[i];
        RunSnapshot {
            state: r.state.clone(),
            position: i,
            done: r.done,
            failed: r.failed,
            running: r.running,
            queued_retries: r.retries.len() as u32,
        }
    }

    fn emit_run(&self, inner: &Inner, id: RunId) {
        if let Some(i) = inner.runs.iter().position(|r| r.state.id == id) {
            self.events.run(Self::snapshot(&inner.runs, i));
        }
    }

    fn set_job(&self, inner: &mut Inner, id: JobId, f: impl FnOnce(&mut JobInfo)) {
        if let Some(info) = inner.jobs.get_mut(&id) {
            f(info);
            self.events.job(info.clone());
        }
    }

    pub fn add_run(&self, spec: RunSpec) -> RunId {
        self.add_run_with(RunId::new(), spec, None)
    }

    pub fn add_run_with(&self, id: RunId, spec: RunSpec, regenerate_of: Option<String>) -> RunId {
        let mut inner = self.inner.lock();
        inner.runs.push(RunEntry {
            state: RunState::new(id, spec),
            retries: VecDeque::new(),
            running: 0,
            done: 0,
            failed: 0,
            regenerate_of,
        });
        self.emit_run(&inner, id);
        drop(inner);
        self.notify.notify_waiters();
        id
    }

    fn with_run<T>(&self, id: RunId, f: impl FnOnce(&mut RunEntry) -> T) -> Result<T> {
        let mut inner = self.inner.lock();
        let r = inner
            .runs
            .iter_mut()
            .find(|r| r.state.id == id)
            .ok_or_else(|| Error::Other(format!("unknown run {id}")))?;
        let out = f(r);
        r.check_done();
        self.emit_run(&inner, id);
        drop(inner);
        self.notify.notify_waiters();
        Ok(out)
    }

    /// Applies an edit; affects only jobs that haven't started (§5.1.2).
    pub fn edit_run(&self, id: RunId, edit: &RunEdit) -> Result<RunState> {
        self.with_run(id, |r| {
            if r.state.status == RunStatus::Done {
                // raising the count or moving the cursor revives a finished run
                if matches!(edit, RunEdit::Count(_) | RunEdit::NextSeed(_)) {
                    r.state.status = RunStatus::Active;
                }
            }
            r.state.apply(edit);
            r.state.clone()
        })
    }

    pub fn pause_run(&self, id: RunId) -> Result<()> {
        self.with_run(id, |r| {
            if r.state.status == RunStatus::Active {
                r.state.status = RunStatus::Paused;
            }
        })
    }

    pub fn resume_run(&self, id: RunId) -> Result<()> {
        self.with_run(id, |r| {
            if r.state.status == RunStatus::Paused {
                r.state.status = RunStatus::Active;
            }
        })
    }

    /// Stops handing out seeds and drops queued retries. Running jobs finish and are kept.
    pub fn stop_run(&self, id: RunId) -> Result<()> {
        let dropped = self.with_run(id, |r| {
            if r.state.status != RunStatus::Done {
                r.state.status = RunStatus::Stopping;
            }
            r.retries.drain(..).map(|j| j.id).collect::<Vec<_>>()
        })?;
        let mut inner = self.inner.lock();
        for j in dropped {
            inner.job_runs.remove(&j);
            self.set_job(&mut inner, j, |i| i.state = JobState::Cancelled);
        }
        Ok(())
    }

    /// Moves a run to a new queue position (drag to reorder).
    pub fn move_run(&self, id: RunId, to: usize) -> Result<()> {
        let mut inner = self.inner.lock();
        let from = inner
            .runs
            .iter()
            .position(|r| r.state.id == id)
            .ok_or_else(|| Error::Other("unknown run".into()))?;
        let r = inner.runs.remove(from);
        let to = to.min(inner.runs.len());
        inner.runs.insert(to, r);
        for i in 0..inner.runs.len() {
            self.events.run(Self::snapshot(&inner.runs, i));
        }
        Ok(())
    }

    /// Queued job → dropped now. Running job → `Cancelling`; the worker keeps the request
    /// open and discards the response (§5.3).
    pub fn cancel_job(&self, job: JobId) -> Result<()> {
        let mut inner = self.inner.lock();
        let state = inner
            .jobs
            .get(&job)
            .map(|j| j.state.clone())
            .ok_or_else(|| Error::Other(format!("unknown job {job}")))?;
        match state {
            JobState::Queued => {
                let mut run_id = None;
                for r in inner.runs.iter_mut() {
                    if let Some(p) = r.retries.iter().position(|j| j.id == job) {
                        r.retries.remove(p);
                        run_id = Some(r.state.id);
                        r.check_done();
                    }
                }
                inner.job_runs.remove(&job);
                self.set_job(&mut inner, job, |i| i.state = JobState::Cancelled);
                if let Some(id) = run_id {
                    self.emit_run(&inner, id);
                }
            }
            JobState::Running {
                server,
                started,
                estimate_ms,
            } => {
                self.set_job(&mut inner, job, |i| {
                    i.state = JobState::Cancelling {
                        server,
                        started,
                        estimate_ms,
                    }
                });
            }
            _ => {}
        }
        Ok(())
    }

    pub fn is_cancelled(&self, job: JobId) -> bool {
        matches!(
            self.inner.lock().jobs.get(&job).map(|j| &j.state),
            Some(JobState::Cancelling { .. })
        )
    }

    pub fn enqueue_transcribe(&self, task: TranscribeTask) {
        self.inner.lock().transcribe.push_back(task);
        self.notify.notify_waiters();
    }

    /// Hands out the next piece of work, or `None`. Transcriptions go first, then the
    /// oldest active run with work; within a run, retries before new seeds.
    pub fn next_work(&self, server: ServerId, estimate_ms: Option<u64>) -> Option<WorkItem> {
        let mut inner = self.inner.lock();
        if let Some(t) = inner.transcribe.pop_front() {
            return Some(WorkItem::Transcribe(t));
        }
        let idx = inner.runs.iter().position(RunEntry::has_work)?;
        let job = {
            let next_id = inner.next_job;
            let r = &mut inner.runs[idx];
            match r.retries.pop_front() {
                Some(j) => j,
                None => {
                    let (seed, index) = r.state.take_seed()?;
                    Job {
                        id: JobId(next_id),
                        run_id: r.state.id,
                        run_name: r.state.spec.name.clone(),
                        seed,
                        start_seed: r.state.spec.start_seed,
                        index,
                        revision: r.state.revision,
                        params: r.state.spec.params.clone(),
                        model: r.state.spec.model.clone(),
                        abc_source: r.state.spec.abc_source.clone(),
                        attempts: 0,
                        regenerate_of: r.regenerate_of.clone(),
                    }
                }
            }
        };
        if job.id.0 == inner.next_job {
            inner.next_job += 1;
        }
        inner.runs[idx].running += 1;
        let run_id = job.run_id;
        inner.jobs.insert(
            job.id,
            JobInfo {
                id: job.id,
                run_id,
                seed: job.seed,
                revision: job.revision,
                attempts: job.attempts,
                state: JobState::Queued,
                timing: None,
            },
        );
        inner.job_runs.insert(job.id, job.clone());
        let started = SystemTime::now();
        self.set_job(&mut inner, job.id, |i| {
            i.state = JobState::Running {
                server,
                started,
                estimate_ms,
            }
        });
        self.emit_run(&inner, run_id);
        Some(WorkItem::Generate(job))
    }

    /// Waits for work. Cancel-safe: drop the future to stop waiting.
    pub async fn wait_work(
        &self,
        server: ServerId,
        estimate_ms: impl Fn() -> Option<u64>,
    ) -> WorkItem {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(w) = self.next_work(server, estimate_ms()) {
                return w;
            }
            // periodic re-check guards against any missed wakeup
            let _ = tokio::time::timeout(Duration::from_secs(5), notified).await;
        }
    }

    /// Reports what happened to a job.
    pub fn finish(&self, job: &Job, outcome: Outcome) {
        let mut inner = self.inner.lock();
        let cancelled = matches!(
            inner.jobs.get(&job.id).map(|j| &j.state),
            Some(JobState::Cancelling { .. })
        );
        let Some(ri) = inner.runs.iter().position(|r| r.state.id == job.run_id) else {
            return;
        };
        let mut release = true;
        let new_state = match outcome {
            Outcome::Encoding(timing) if !cancelled => {
                release = false; // still counts as running until encoded
                let t = timing.clone();
                self.set_job(&mut inner, job.id, |i| i.timing = Some(t));
                JobState::Encoding
            }
            Outcome::Encoding(_) | Outcome::Discarded => JobState::Cancelled,
            Outcome::Done(song) => {
                inner.runs[ri].done += 1;
                JobState::Done(song)
            }
            Outcome::Retry(reason) => {
                let attempts = job.attempts + 1;
                let stopping = inner.runs[ri].state.status == RunStatus::Stopping;
                if cancelled || stopping {
                    JobState::Cancelled
                } else if attempts >= MAX_ATTEMPTS {
                    inner.runs[ri].failed += 1;
                    tracing::warn!(job = %job.id, seed = job.seed, "failed after {attempts} attempts: {reason}");
                    JobState::Failed(format!("{reason} (after {attempts} attempts)"))
                } else {
                    let mut again = job.clone();
                    again.attempts = attempts;
                    inner.runs[ri].retries.push_front(again);
                    self.set_job(&mut inner, job.id, |i| i.attempts = attempts);
                    JobState::Queued
                }
            }
            Outcome::Failed(reason) => {
                inner.runs[ri].failed += 1;
                JobState::Failed(reason)
            }
        };
        if release {
            inner.runs[ri].running = inner.runs[ri].running.saturating_sub(1);
        }
        if new_state.is_terminal() {
            inner.job_runs.remove(&job.id);
        }
        self.set_job(&mut inner, job.id, |i| i.state = new_state);
        inner.runs[ri].check_done();
        let run_id = job.run_id;
        self.emit_run(&inner, run_id);
        drop(inner);
        self.notify.notify_waiters();
    }

    pub fn runs(&self) -> Vec<RunSnapshot> {
        let inner = self.inner.lock();
        (0..inner.runs.len())
            .map(|i| Self::snapshot(&inner.runs, i))
            .collect()
    }

    pub fn run(&self, id: RunId) -> Option<RunState> {
        self.inner
            .lock()
            .runs
            .iter()
            .find(|r| r.state.id == id)
            .map(|r| r.state.clone())
    }

    pub fn jobs(&self) -> Vec<JobInfo> {
        self.inner.lock().jobs.values().cloned().collect()
    }

    pub fn job(&self, id: JobId) -> Option<JobInfo> {
        self.inner.lock().jobs.get(&id).cloned()
    }

    /// Removes finished runs from the queue view.
    pub fn clear_done(&self) {
        self.inner
            .lock()
            .runs
            .retain(|r| r.state.status != RunStatus::Done);
    }

    /// Wakes every waiting worker (e.g. after servers change).
    pub fn wake(&self) {
        self.notify.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::RunName;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    #[derive(Default)]
    struct Rec(Mutex<Vec<JobInfo>>, Mutex<Vec<RunSnapshot>>);
    impl SchedulerEvents for Arc<Rec> {
        fn job(&self, info: JobInfo) {
            self.0.lock().push(info);
        }
        fn run(&self, snap: RunSnapshot) {
            self.1.lock().push(snap);
        }
    }

    fn spec(name: &str, start: u32, count: Option<u32>) -> RunSpec {
        RunSpec {
            name: RunName::parse(name).unwrap(),
            params: GenerationParams {
                lyrics: "l".into(),
                style: "s".into(),
                ..Default::default()
            },
            start_seed: start,
            count,
            model: ModelSpec {
                id: "yue2".into(),
                family: "yue2".into(),
                task: "gen".into(),
                mode: "offline".into(),
                path: "/p".into(),
                load_options: Default::default(),
                session_options: Default::default(),
            },
            abc_source: AbcSource::Manual,
        }
    }

    fn sched() -> (Arc<Scheduler>, Arc<Rec>) {
        let rec = Arc::new(Rec::default());
        (Arc::new(Scheduler::new(Box::new(rec.clone()))), rec)
    }

    fn job(w: Option<WorkItem>) -> Job {
        match w {
            Some(WorkItem::Generate(j)) => j,
            _ => panic!("expected a job"),
        }
    }

    const S0: ServerId = ServerId(0);
    const S1: ServerId = ServerId(1);

    #[test]
    fn seeds_unique_without_gaps_under_concurrency() {
        let (s, _) = sched();
        let run = s.add_run(spec("a", 100, Some(400)));
        let seeds = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            for w in 0..8 {
                let s = s.clone();
                let seeds = seeds.clone();
                scope.spawn(move || {
                    while let Some(WorkItem::Generate(j)) = s.next_work(ServerId(w), None) {
                        seeds.lock().push(j.seed);
                        s.finish(&j, Outcome::Done(SongId(format!("{}", j.seed))));
                    }
                });
            }
        });
        let mut v = seeds.lock().clone();
        v.sort();
        assert_eq!(v, (100..500).collect::<Vec<_>>());
        assert_eq!(s.run(run).unwrap().status, RunStatus::Done);
    }

    #[test]
    fn retry_keeps_seed_and_revision_and_goes_first() {
        let (s, _) = sched();
        let run = s.add_run(spec("a", 1, Some(3)));
        let j1 = job(s.next_work(S0, None));
        let mut p = s.run(run).unwrap().spec.params;
        p.style = "edited".into();
        s.edit_run(run, &RunEdit::Params(p)).unwrap();
        s.finish(&j1, Outcome::Retry("connection reset".into()));
        let again = job(s.next_work(S1, None));
        assert_eq!((again.seed, again.revision, again.attempts), (1, 0, 1));
        assert_eq!(
            again.params.style, "s",
            "retry keeps the revision it started with"
        );
        assert_eq!(again.id, j1.id);
        let next = job(s.next_work(S0, None));
        assert_eq!((next.seed, next.revision), (2, 1));
        assert_eq!(next.params.style, "edited");
        s.finish(&again, Outcome::Retry("again".into()));
        assert!(
            matches!(s.job(again.id).unwrap().state, JobState::Failed(_)),
            "2 attempts max"
        );
        let n3 = job(s.next_work(S0, None));
        assert_eq!(n3.seed, 3, "failed seed is recorded, not skipped silently");
    }

    #[test]
    fn edits_affect_only_unstarted_jobs() {
        let (s, _) = sched();
        let run = s.add_run(spec("a", 0, None));
        let running = job(s.next_work(S0, None));
        let mut p = running.params.clone();
        p.lyrics = "new".into();
        s.edit_run(run, &RunEdit::Params(p)).unwrap();
        s.edit_run(run, &RunEdit::NextSeed(50)).unwrap();
        let next = job(s.next_work(S1, None));
        assert_eq!(running.params.lyrics, "l");
        assert_eq!(running.revision, 0);
        assert_eq!(
            (next.params.lyrics.as_str(), next.revision, next.seed),
            ("new", 1, 50)
        );
    }

    #[test]
    fn fcfs_across_runs() {
        let (s, _) = sched();
        let a = s.add_run(spec("a", 0, Some(2)));
        let b = s.add_run(spec("b", 10, Some(2)));
        let a0 = job(s.next_work(S0, None));
        let a1 = job(s.next_work(S1, None));
        assert_eq!((a0.run_id, a1.run_id), (a, a));
        // A has handed out its last seed; the next free server starts B right away
        s.finish(&a0, Outcome::Done(SongId("x".into())));
        let b0 = job(s.next_work(S0, None));
        assert_eq!((b0.run_id, b0.seed), (b, 10));
        assert_eq!(
            s.run(a).unwrap().status,
            RunStatus::Active,
            "a1 still running"
        );
        s.finish(&a1, Outcome::Done(SongId("y".into())));
        assert_eq!(s.run(a).unwrap().status, RunStatus::Done);
    }

    #[test]
    fn paused_runs_are_skipped_and_until_stopped_blocks_later_runs() {
        let (s, _) = sched();
        let a = s.add_run(spec("a", 0, None));
        let b = s.add_run(spec("b", 0, Some(1)));
        assert_eq!(job(s.next_work(S0, None)).run_id, a);
        assert_eq!(
            job(s.next_work(S0, None)).run_id,
            a,
            "until-stopped keeps later runs waiting"
        );
        s.pause_run(a).unwrap();
        assert_eq!(job(s.next_work(S0, None)).run_id, b);
        assert!(s.next_work(S0, None).is_none());
        s.resume_run(a).unwrap();
        assert_eq!(job(s.next_work(S0, None)).run_id, a);
    }

    #[test]
    fn move_run_reorders() {
        let (s, _) = sched();
        let _a = s.add_run(spec("a", 0, Some(5)));
        let b = s.add_run(spec("b", 0, Some(5)));
        s.move_run(b, 0).unwrap();
        assert_eq!(job(s.next_work(S0, None)).run_id, b);
    }

    #[test]
    fn stop_keeps_running_drops_retries() {
        let (s, _) = sched();
        let run = s.add_run(spec("a", 0, None));
        let j0 = job(s.next_work(S0, None));
        let j1 = job(s.next_work(S1, None));
        s.finish(&j0, Outcome::Retry("x".into()));
        s.stop_run(run).unwrap();
        assert_eq!(
            s.job(j0.id).unwrap().state,
            JobState::Cancelled,
            "queued retry dropped"
        );
        assert!(s.next_work(S0, None).is_none());
        assert_eq!(s.run(run).unwrap().status, RunStatus::Stopping);
        s.finish(&j1, Outcome::Encoding(Timing::default()));
        s.finish(&j1, Outcome::Done(SongId("kept".into())));
        assert_eq!(
            s.job(j1.id).unwrap().state,
            JobState::Done(SongId("kept".into()))
        );
        assert_eq!(s.run(run).unwrap().status, RunStatus::Done);
    }

    #[test]
    fn cancel_queued_and_running() {
        let (s, _) = sched();
        let _run = s.add_run(spec("a", 0, Some(3)));
        let j0 = job(s.next_work(S0, None));
        let j1 = job(s.next_work(S1, None));
        s.finish(&j1, Outcome::Retry("x".into()));
        s.cancel_job(j1.id).unwrap();
        assert_eq!(s.job(j1.id).unwrap().state, JobState::Cancelled);
        s.cancel_job(j0.id).unwrap();
        assert!(s.is_cancelled(j0.id));
        assert!(matches!(
            s.job(j0.id).unwrap().state,
            JobState::Cancelling { .. }
        ));
        // the response arrives and is decoded anyway → still discarded
        s.finish(&j0, Outcome::Encoding(Timing::default()));
        assert_eq!(s.job(j0.id).unwrap().state, JobState::Cancelled);
        let next = job(s.next_work(S0, None));
        assert_eq!(next.seed, 2);
    }

    #[test]
    fn transcriptions_go_first() {
        let (s, _) = sched();
        s.add_run(spec("a", 0, None));
        let (tx, _rx) = oneshot::channel();
        s.enqueue_transcribe(TranscribeTask {
            wav: "x.wav".into(),
            model: spec("a", 0, None).model,
            reply: tx,
        });
        assert!(matches!(
            s.next_work(S0, None),
            Some(WorkItem::Transcribe(_))
        ));
        assert!(matches!(s.next_work(S0, None), Some(WorkItem::Generate(_))));
    }

    #[test]
    fn done_run_revives_on_count_raise() {
        let (s, _) = sched();
        let run = s.add_run(spec("a", 0, Some(1)));
        let j = job(s.next_work(S0, None));
        s.finish(&j, Outcome::Done(SongId("x".into())));
        assert_eq!(s.run(run).unwrap().status, RunStatus::Done);
        s.edit_run(run, &RunEdit::Count(Some(2))).unwrap();
        assert_eq!(job(s.next_work(S0, None)).seed, 1);
    }

    #[test]
    fn events_are_emitted() {
        let (s, rec) = sched();
        s.add_run(spec("a", 7, Some(1)));
        let j = job(s.next_work(S0, None));
        s.finish(
            &j,
            Outcome::Encoding(Timing {
                wall_ms: 5,
                ..Default::default()
            }),
        );
        s.finish(&j, Outcome::Done(SongId("id".into())));
        let states: Vec<_> = rec
            .0
            .lock()
            .iter()
            .map(|i| std::mem::discriminant(&i.state))
            .collect();
        assert!(states.len() >= 3);
        let last = rec.0.lock().last().unwrap().clone();
        assert_eq!(last.state, JobState::Done(SongId("id".into())));
        assert_eq!(last.timing.unwrap().wall_ms, 5);
        assert_eq!(rec.1.lock().last().unwrap().state.status, RunStatus::Done);
    }

    #[tokio::test]
    async fn wait_work_wakes_on_new_run() {
        let (s, _) = sched();
        let s2 = s.clone();
        let h = tokio::spawn(async move { s2.wait_work(S0, || None).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!h.is_finished());
        s.add_run(spec("a", 0, Some(1)));
        let w = tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(w, WorkItem::Generate(_)));
    }
}
