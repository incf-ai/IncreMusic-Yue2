//! Run history: one RON record per run under `<library_root>/runs/<run-id>.ron`, written as
//! the run progresses, so every queued run can be browsed and reloaded later — including
//! runs that never produced a song (docs/JOB_HISTORY_PLAN.md).
//!
//! The scheduler reports every change through [`SchedulerEvents`]; [`Recorder`] folds those
//! into records and hands the text to a writer thread, so no disk I/O happens while the
//! scheduler holds its lock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::config::{ModelSpec, from_ron, to_ron};
use crate::error::Result;
use crate::fsutil;
use crate::params::GenerationParams;
use crate::run::{AbcSource, RunId, RunName, RunSpec, RunState, RunStatus};
use crate::scheduler::{JobInfo, JobState, RunSnapshot, SchedulerEvents};

pub const SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordStatus {
    Active,
    Paused,
    Stopping,
    Done,
    /// Deleted from the queue before it finished.
    Removed,
    /// Still queued or running when the app quit.
    Interrupted,
}

impl RecordStatus {
    /// Still in the live queue: the record may change.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Paused | Self::Stopping)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Stopping => "stopping",
            Self::Done => "done",
            Self::Removed => "removed",
            Self::Interrupted => "interrupted",
        }
    }
}

impl From<RunStatus> for RecordStatus {
    fn from(s: RunStatus) -> Self {
        match s {
            RunStatus::Active => Self::Active,
            RunStatus::Paused => Self::Paused,
            RunStatus::Stopping => Self::Stopping,
            RunStatus::Done => Self::Done,
        }
    }
}

/// The params in force from `first_seed` on. `revision` matches `RunState::revision` and
/// each job's revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub revision: u32,
    pub at: String,
    pub first_seed: u32,
    pub params: GenerationParams,
    pub abc_source: AbcSource,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SeedOutcome {
    /// The song's id (its library path).
    Song(String),
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeedEntry {
    pub seed: u32,
    pub revision: u32,
    pub outcome: SeedOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema: u32,
    pub app_version: String,
    pub id: RunId,
    pub name: RunName,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub status: RecordStatus,
    pub start_seed: u32,
    /// `None` → until stopped.
    pub count: Option<u32>,
    pub next_seed: u32,
    /// New seeds handed out so far.
    pub issued: u32,
    pub model: ModelSpec,
    /// Regenerate: the original stem.
    pub regenerate_of: Option<String>,
    /// Resume: the interrupted run this one continues.
    pub resumed_from: Option<RunId>,
    pub revisions: Vec<Revision>,
    pub seeds: Vec<SeedEntry>,
}

impl RunRecord {
    fn new(state: &RunState, origin: Origin) -> Self {
        let spec = &state.spec;
        RunRecord {
            schema: SCHEMA,
            app_version: crate::APP_VERSION.into(),
            id: state.id,
            name: spec.name.clone(),
            created_at: fsutil::now_rfc3339(),
            finished_at: None,
            status: state.status.into(),
            start_seed: spec.start_seed,
            count: spec.count,
            next_seed: state.next_seed,
            issued: state.issued,
            model: spec.model.clone(),
            regenerate_of: origin.regenerate_of,
            resumed_from: origin.resumed_from,
            revisions: vec![Revision {
                revision: state.revision,
                at: fsutil::now_rfc3339(),
                first_seed: spec.start_seed,
                params: spec.params.clone(),
                abc_source: spec.abc_source.clone(),
            }],
            seeds: Vec::new(),
        }
    }

    /// The latest revision, or revision `n`.
    pub fn revision(&self, n: Option<u32>) -> Option<&Revision> {
        match n {
            None => self.revisions.last(),
            Some(n) => self.revisions.iter().find(|r| r.revision == n),
        }
    }

    pub fn done(&self) -> u32 {
        self.count_outcomes(|o| matches!(o, SeedOutcome::Song(_)))
    }

    pub fn failed(&self) -> u32 {
        self.count_outcomes(|o| matches!(o, SeedOutcome::Failed(_)))
    }

    fn count_outcomes(&self, f: impl Fn(&SeedOutcome) -> bool) -> u32 {
        self.seeds.iter().filter(|s| f(&s.outcome)).count() as u32
    }

    /// Seeds still to hand out; `None` → until stopped.
    pub fn remaining(&self) -> Option<u32> {
        self.count.map(|c| c.saturating_sub(self.issued))
    }

    /// Seeds handed out that never reported back (running when the app quit): the ones
    /// just below `next_seed` without an outcome, at most `issued - outcomes` of them.
    pub fn in_flight(&self) -> u32 {
        let lost = self.issued.saturating_sub(self.seeds.len() as u32);
        let mut n = 0;
        while n < lost {
            let Some(seed) = self.next_seed.checked_sub(n + 1) else {
                break;
            };
            if seed < self.start_seed || self.seeds.iter().any(|e| e.seed == seed) {
                break;
            }
            n += 1;
        }
        n
    }

    /// A spec that continues this run with the latest params: the seeds that were
    /// running when it stopped, then the ones it didn't get to.
    pub fn resume_spec(&self) -> Option<RunSpec> {
        let rev = self.revision(None)?;
        let redo = self.in_flight();
        Some(RunSpec {
            name: self.name.clone(),
            params: rev.params.clone(),
            start_seed: self.next_seed - redo,
            count: self.remaining().map(|r| r + redo),
            model: self.model.clone(),
            abc_source: rev.abc_source.clone(),
        })
    }

    pub fn summary(&self) -> RunSummary {
        RunSummary {
            id: self.id,
            name: self.name.to_string(),
            created_at: self.created_at.clone(),
            status: self.status,
            start_seed: self.start_seed,
            count: self.count,
            done: self.done(),
            failed: self.failed(),
            revisions: self.revisions.len() as u32,
            model_id: self.model.id.clone(),
            regenerate_of: self.regenerate_of.clone(),
        }
    }

    /// Folds a snapshot in; returns whether anything changed.
    fn apply_snapshot(&mut self, s: &RunState) -> bool {
        let before = self.clone();
        self.status = s.status.into();
        self.count = s.spec.count;
        self.next_seed = s.next_seed;
        self.issued = s.issued;
        let last = self.revisions.last_mut();
        match last {
            Some(r) if r.revision == s.revision => {
                // an ABC-source edit doesn't bump the revision; the audio is the same
                r.abc_source = s.spec.abc_source.clone();
            }
            _ => self.revisions.push(Revision {
                revision: s.revision,
                at: fsutil::now_rfc3339(),
                first_seed: s.next_seed,
                params: s.spec.params.clone(),
                abc_source: s.spec.abc_source.clone(),
            }),
        }
        if self.status == RecordStatus::Done {
            self.finished_at.get_or_insert_with(fsutil::now_rfc3339);
        } else {
            // a raised count revives a finished run
            self.finished_at = None;
        }
        *self != before
    }

    fn close(&mut self, status: RecordStatus) {
        self.status = status;
        self.finished_at.get_or_insert_with(fsutil::now_rfc3339);
    }
}

/// One row of the History list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: RunId,
    pub name: String,
    pub created_at: String,
    pub status: RecordStatus,
    pub start_seed: u32,
    pub count: Option<u32>,
    pub done: u32,
    pub failed: u32,
    pub revisions: u32,
    pub model_id: String,
    pub regenerate_of: Option<String>,
}

/// How a run came to be, noted before it's queued.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Origin {
    pub regenerate_of: Option<String>,
    pub resumed_from: Option<RunId>,
}

enum Write {
    File(PathBuf, String),
    Flush(mpsc::Sender<()>),
}

/// The store. Records of runs in this session stay in memory; older ones are read from
/// disk on demand.
pub struct RunHistory {
    dir: PathBuf,
    live: Mutex<BTreeMap<RunId, RunRecord>>,
    origins: Mutex<BTreeMap<RunId, Origin>>,
    tx: Mutex<Option<mpsc::Sender<Write>>>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl RunHistory {
    /// Opens `dir` and marks records left open by an earlier session `Interrupted`.
    /// Returns the store and how many records were marked.
    pub fn open(dir: impl Into<PathBuf>) -> Result<(Arc<RunHistory>, usize)> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| crate::Error::io(&dir, e))?;
        let mut interrupted = 0;
        for path in record_files(&dir) {
            if let Ok(mut r) = read_record(&path)
                && r.status.is_open()
            {
                r.close(RecordStatus::Interrupted);
                fsutil::write_atomic(&path, to_ron(&r)?.as_bytes())?;
                interrupted += 1;
            }
        }
        let (tx, rx) = mpsc::channel::<Write>();
        let writer = std::thread::Builder::new()
            .name("run-history".into())
            .spawn(move || {
                for w in rx {
                    match w {
                        Write::File(path, text) => {
                            if let Err(e) = fsutil::write_atomic(&path, text.as_bytes()) {
                                tracing::warn!(
                                    source = "history",
                                    "writing {}: {e}",
                                    path.display()
                                );
                            }
                        }
                        Write::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .map_err(|e| crate::Error::Other(e.to_string()))?;
        Ok((
            Arc::new(RunHistory {
                dir,
                live: Mutex::new(BTreeMap::new()),
                origins: Mutex::new(BTreeMap::new()),
                tx: Mutex::new(Some(tx)),
                writer: Mutex::new(Some(writer)),
            }),
            interrupted,
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, id: RunId) -> PathBuf {
        self.dir.join(format!("{id}.ron"))
    }

    /// Notes where a run came from; call before the scheduler sees it.
    pub fn note_origin(&self, id: RunId, origin: Origin) {
        self.origins.lock().insert(id, origin);
    }

    /// All records, newest first. Unreadable files are skipped and reported.
    pub fn summaries(&self) -> (Vec<RunSummary>, Vec<String>) {
        let live = self.live.lock().clone();
        let mut out: BTreeMap<RunId, RunSummary> =
            live.values().map(|r| (r.id, r.summary())).collect();
        let mut errors = Vec::new();
        for path in record_files(&self.dir) {
            match read_record(&path) {
                Ok(r) => {
                    out.entry(r.id).or_insert_with(|| r.summary());
                }
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        (out.into_values().rev().collect(), errors)
    }

    pub fn load(&self, id: RunId) -> Result<RunRecord> {
        if let Some(r) = self.live.lock().get(&id) {
            return Ok(r.clone());
        }
        read_record(&self.path(id))
    }

    /// Waits until everything sent so far is on disk.
    pub fn flush(&self) {
        let (done, wait) = mpsc::channel();
        if let Some(tx) = &*self.tx.lock()
            && tx.send(Write::Flush(done)).is_ok()
        {
            let _ = wait.recv();
        }
    }

    /// Writes what's pending and stops the writer thread.
    pub fn close(&self) {
        self.tx.lock().take();
        if let Some(w) = self.writer.lock().take() {
            let _ = w.join();
        }
    }

    fn save(&self, r: &RunRecord) {
        let text = match to_ron(r) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(source = "history", "run {}: {e}", r.id);
                return;
            }
        };
        if let Some(tx) = &*self.tx.lock() {
            let _ = tx.send(Write::File(self.path(r.id), text));
        }
    }

    fn on_snapshot(&self, snap: &RunSnapshot) {
        let s = &snap.state;
        let mut live = self.live.lock();
        let changed = match live.get_mut(&s.id) {
            Some(r) => r.apply_snapshot(s),
            None => {
                let origin = self.origins.lock().remove(&s.id).unwrap_or_default();
                let mut r = RunRecord::new(s, origin);
                r.apply_snapshot(s);
                live.insert(s.id, r);
                true
            }
        };
        if changed {
            self.save(&live[&s.id]);
        }
    }

    fn on_job(&self, info: &JobInfo) {
        let outcome = match &info.state {
            JobState::Done(song) => SeedOutcome::Song(song.0.clone()),
            JobState::Failed(why) => SeedOutcome::Failed(why.clone()),
            JobState::Cancelled => SeedOutcome::Cancelled,
            _ => return,
        };
        let mut live = self.live.lock();
        let Some(r) = live.get_mut(&info.run_id) else {
            return;
        };
        let entry = SeedEntry {
            seed: info.seed,
            revision: info.revision,
            outcome,
        };
        // a seed that was cancelled and later redone keeps its latest outcome
        match r.seeds.iter_mut().find(|e| e.seed == entry.seed) {
            Some(e) if *e == entry => return,
            Some(e) => *e = entry,
            None => r.seeds.push(entry),
        }
        self.save(r);
    }

    fn on_removed(&self, id: RunId) {
        let mut live = self.live.lock();
        if let Some(r) = live.get_mut(&id) {
            if r.status.is_open() {
                r.close(RecordStatus::Removed);
            }
            self.save(r);
        }
    }
}

impl Drop for RunHistory {
    fn drop(&mut self) {
        self.close();
    }
}

fn record_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "ron")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    v.sort();
    v
}

fn read_record(path: &Path) -> Result<RunRecord> {
    let text = std::fs::read_to_string(path).map_err(|e| crate::Error::io(path, e))?;
    from_ron(&text)
}

/// Forwards scheduler events and records them in the history.
pub struct Recorder<E> {
    pub inner: E,
    pub history: Arc<RunHistory>,
}

impl<E: SchedulerEvents> SchedulerEvents for Recorder<E> {
    fn job(&self, info: JobInfo) {
        self.history.on_job(&info);
        self.inner.job(info);
    }
    fn run(&self, snap: RunSnapshot) {
        self.history.on_snapshot(&snap);
        self.inner.run(snap);
    }
    fn run_removed(&self, id: RunId) {
        self.history.on_removed(id);
        self.inner.run_removed(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::RunEdit;
    use crate::scheduler::ServerId;
    use crate::scheduler::{Outcome, Scheduler, WorkItem};

    struct Nop;
    impl SchedulerEvents for Nop {
        fn job(&self, _: JobInfo) {}
        fn run(&self, _: RunSnapshot) {}
        fn run_removed(&self, _: RunId) {}
    }

    fn spec(name: &str, start: u32, count: Option<u32>) -> RunSpec {
        RunSpec {
            name: RunName::parse(name).unwrap(),
            params: GenerationParams {
                abc: Some("X:1\nK:C\nC".into()),
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

    fn setup() -> (tempfile::TempDir, Arc<RunHistory>, Scheduler) {
        let dir = tempfile::tempdir().unwrap();
        let (h, _) = RunHistory::open(dir.path().join("runs")).unwrap();
        let sched = Scheduler::new(Box::new(Recorder {
            inner: Nop,
            history: h.clone(),
        }));
        (dir, h, sched)
    }

    fn take(sched: &Scheduler) -> crate::scheduler::Job {
        match sched.next_work(ServerId(0), None) {
            Some(WorkItem::Generate(j)) => j,
            _ => panic!("no job"),
        }
    }

    #[test]
    fn records_revisions_seeds_and_status() {
        let (_d, h, sched) = setup();
        let id = RunId::new();
        h.note_origin(
            id,
            Origin {
                regenerate_of: Some("x-1".into()),
                resumed_from: None,
            },
        );
        sched.add_run_with(id, spec("sunny", 10, Some(3)), None);
        let j1 = take(&sched);
        sched.finish(
            &j1,
            Outcome::Done(crate::library::SongId("unreviewed/sunny-10".into())),
        );
        let mut p = spec("sunny", 0, None).params;
        p.abc = Some("X:1\nK:D\nD".into());
        sched.edit_run(id, &RunEdit::Params(p.clone())).unwrap();
        let j2 = take(&sched);
        sched.finish(&j2, Outcome::Failed("400 bad".into()));
        let j3 = take(&sched);
        sched.finish(
            &j3,
            Outcome::Done(crate::library::SongId("unreviewed/sunny-12".into())),
        );
        h.flush();

        let r = read_record(&h.path(id)).unwrap();
        assert_eq!(r.status, RecordStatus::Done);
        assert!(r.finished_at.is_some());
        assert_eq!(r.regenerate_of.as_deref(), Some("x-1"));
        assert_eq!(r.revisions.len(), 2);
        assert_eq!(r.revisions[1].first_seed, 11);
        assert_eq!(r.revisions[1].params, p);
        assert_eq!(
            r.seeds,
            vec![
                SeedEntry {
                    seed: 10,
                    revision: 0,
                    outcome: SeedOutcome::Song("unreviewed/sunny-10".into())
                },
                SeedEntry {
                    seed: 11,
                    revision: 1,
                    outcome: SeedOutcome::Failed("400 bad".into())
                },
                SeedEntry {
                    seed: 12,
                    revision: 1,
                    outcome: SeedOutcome::Song("unreviewed/sunny-12".into())
                },
            ]
        );
        assert_eq!((r.done(), r.failed()), (2, 1));
        assert_eq!(h.load(id).unwrap(), r, "memory and disk agree");
    }

    #[test]
    fn a_run_stopped_before_any_song_is_kept_and_removal_is_recorded() {
        let (_d, h, sched) = setup();
        let id = sched.add_run(spec("quiet", 5, Some(4)));
        sched.stop_run(id).unwrap();
        sched.remove_run(id).unwrap();
        h.flush();
        let r = read_record(&h.path(id)).unwrap();
        assert_eq!(
            r.status,
            RecordStatus::Done,
            "stopped with nothing running → done"
        );
        let id2 = sched.add_run(spec("paused", 5, Some(4)));
        sched.pause_run(id2).unwrap();
        sched.remove_run(id2).unwrap();
        h.flush();
        assert_eq!(
            read_record(&h.path(id2)).unwrap().status,
            RecordStatus::Removed
        );
    }

    #[test]
    fn open_records_become_interrupted_and_resume_continues() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let id = {
            let (h, _) = RunHistory::open(&runs).unwrap();
            let sched = Scheduler::new(Box::new(Recorder {
                inner: Nop,
                history: h.clone(),
            }));
            let id = sched.add_run(spec("long", 100, Some(5)));
            let j = take(&sched);
            sched.finish(
                &j,
                Outcome::Done(crate::library::SongId("unreviewed/long-100".into())),
            );
            take(&sched); // running when the app quits
            h.close();
            id
        };
        std::fs::write(runs.join("garbage.ron"), "not ron").unwrap();
        let (h, n) = RunHistory::open(&runs).unwrap();
        assert_eq!(n, 1);
        let r = h.load(id).unwrap();
        assert_eq!(r.status, RecordStatus::Interrupted);
        assert_eq!(r.in_flight(), 1, "seed 101 was running");
        let s = r.resume_spec().unwrap();
        assert_eq!((s.start_seed, s.count), (101, Some(4)));
        let (list, errors) = h.summaries();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].done, 1);
        assert_eq!(errors.len(), 1, "a corrupt file is reported, not fatal");
    }
}
