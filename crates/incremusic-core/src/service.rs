//! `CoreHandle`: the async command handler and event broadcaster that ties everything
//! together (design §2.3). The GUI only talks to the core through [`CoreBackend`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot, watch};

use crate::api::{AudioCppClient, GenerateOutcome, Generated, Timing};
use crate::config::{Config, Models, ServerConfig};
use crate::error::{Error, Result};
use crate::history::{Origin, Recorder, RunHistory, RunRecord, RunSummary};
use crate::launcher::{self, LaunchPlan, Launched, Opener};
use crate::library::{Library, LibraryCommand, LibraryDelta, SongId};
use crate::media::{
    self, Ffmpeg, Recipe, RecipeModel, RecipeOutput, RecipeRun, RecipeServer, SongMeta,
};
use crate::params::{GenerationParams, Preset};
use crate::playback::{self, PlayState, Playback};
use crate::project::{Keypoints, ProjectInputs, ProjectStore, ProjectSummary};
use crate::run::{AbcSource, JobId, RunEdit, RunId, RunName, RunSpec};
use crate::scheduler::{
    Job, JobInfo, Outcome, RunSnapshot, Scheduler, SchedulerEvents, ServerId, TranscribeTask,
    WorkItem,
};
use crate::transcribe::{self, AbcScore};
use crate::{fsutil, models};

// ---------------------------------------------------------------------------------------
// Boundary types

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    LaunchServer(ServerId),
    StopServer(ServerId),
    /// Clears a sticky `Down` (e.g. after a timeout) once `/health` answers.
    RecheckServer(ServerId),
    RefreshServers,
    Transcribe {
        project: RunName,
        audio: PathBuf,
        force: bool,
    },
    /// Transcribes the project's existing reference again (forces a server call).
    Retranscribe(RunName),
    StartRun(RunSpec),
    EditRun(RunId, RunEdit),
    PauseRun(RunId),
    ResumeRun(RunId),
    StopRun(RunId),
    /// Deletes a paused, stopping or finished run; its unfinished jobs are cancelled.
    RemoveRun(RunId),
    MoveRun(RunId, usize),
    CancelJob(JobId),
    ClearFinishedRuns,
    /// Same request as the song's recipe, saved as a new take `<stem>-rN` (§6.2).
    Regenerate(SongId),
    /// Re-reads the run history and sends [`Event::RunHistory`].
    ListRuns,
    /// Sends [`Event::RunRecord`] with one run's full record.
    LoadRunRecord(RunId),
    /// Queues a new run that continues an `Interrupted` record where it stopped.
    ResumeInterrupted(RunId),
    Library(LibraryCommand),
    LoadProject(String),
    /// Re-reads `inputs/` and sends [`Event::Projects`].
    RefreshProjects,
    /// Renames a project folder (§5.2.1). Songs keep the old run name in their recipes.
    RenameProject(String, RunName),
    /// Moves a project folder to the system trash.
    DeleteProject(String),
    /// Saves a project's review keypoints. The GUI already shows them, so nothing is sent
    /// back unless saving fails.
    SetKeypoints(String, Keypoints),
    /// Plays a project's original reference audio.
    PlayReference(String),
    LoadPreset(PathBuf),
    SavePreset(PathBuf, Preset),
    Play(SongId),
    Seek(Duration),
    SeekBy(f64),
    Pause,
    Resume,
    TogglePause,
    Stop,
    Shutdown {
        stop_launched: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerState {
    Stopped,
    Starting {
        since: SystemTime,
    },
    Ready {
        backend: Option<String>,
        loaded_models: Vec<String>,
    },
    /// `job: None` → transcribing.
    Busy {
        job: Option<JobId>,
        since: SystemTime,
    },
    Stopping {
        since: SystemTime,
    },
    Down(String),
}

impl ServerState {
    pub fn can_launch(&self) -> bool {
        matches!(self, ServerState::Stopped | ServerState::Down(_))
    }

    pub fn is_up(&self) -> bool {
        matches!(self, ServerState::Ready { .. } | ServerState::Busy { .. })
    }

    pub fn label(&self) -> String {
        match self {
            ServerState::Stopped => "stopped".into(),
            ServerState::Starting { .. } => "starting".into(),
            ServerState::Ready { .. } => "ready".into(),
            ServerState::Busy { job: Some(j), .. } => format!("busy ({j})"),
            ServerState::Busy { job: None, .. } => "busy (transcribing)".into(),
            ServerState::Stopping { .. } => "stopping — waiting for the GPU job to finish".into(),
            ServerState::Down(r) => format!("down: {r}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub id: ServerId,
    pub name: String,
    pub port: u16,
    pub launchable: bool,
    pub backend: Option<String>,
    pub device: Option<u32>,
    /// Where the launcher script saves the server's output (Unix, launchable servers).
    #[serde(default)]
    pub log_file: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InitInfo {
    pub servers: Vec<ServerInfo>,
    pub library_root: PathBuf,
    pub models: Models,
    pub default_preset: Option<Preset>,
    pub default_preset_path: Option<PathBuf>,
    /// Native terminal opener in use, or why there is none.
    pub opener: Result<String, String>,
    pub ffmpeg: Result<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    pub time: SystemTime,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
    /// A line the server itself printed, rather than a message from the app.
    #[serde(default)]
    pub output: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Init(InitInfo),
    ServerStatus(ServerId, ServerState),
    /// Configured model paths this server reports missing or of the wrong kind (§1.1),
    /// checked each time it becomes ready. Empty: all present.
    ModelPaths(ServerId, Vec<String>),
    TranscribeStarted(String),
    TranscribeDone(Result<AbcScore, String>),
    RunUpdate(RunSnapshot),
    /// The run was deleted from the queue, along with its jobs.
    RunRemoved(RunId),
    JobUpdate(JobId, JobInfo),
    LibraryChanged(LibraryDelta),
    Projects(Vec<ProjectSummary>),
    ProjectInputs(Result<ProjectInputs, String>),
    PresetLoaded(Result<(PathBuf, Preset), String>),
    /// Regenerate: the recipe as editable params (for "open as base for a new batch").
    Recipe(
        SongId,
        Result<(RunName, GenerationParams, u32, AbcSource), String>,
    ),
    PlaybackPosition(Duration),
    PlaybackState {
        state: PlayState,
        song: Option<SongId>,
        duration: Duration,
    },
    Peaks(SongId, Vec<f32>),
    /// Every recorded run, newest first.
    RunHistory(Vec<RunSummary>),
    RunRecord(RunId, Result<RunRecord, String>),
    Log(LogLine),
}

/// What the GUI needs from the core. Tests replace it with a fake that records commands.
pub trait CoreBackend: Send + Sync {
    fn send(&self, cmd: Command);
    fn try_recv(&self) -> Option<Event>;
    /// The sample source for the GUI's audio output device.
    fn playback(&self) -> Option<Arc<Playback>>;
}

// ---------------------------------------------------------------------------------------
// Event plumbing

#[derive(Clone)]
pub struct EventTx {
    tx: std::sync::mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl EventTx {
    pub fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        (self.wake)();
    }

    pub fn log(&self, level: LogLevel, source: &str, message: impl Into<String>) {
        let message = message.into();
        match level {
            LogLevel::Info => tracing::info!(source, "{message}"),
            LogLevel::Warn => tracing::warn!(source, "{message}"),
            LogLevel::Error => tracing::error!(source, "{message}"),
        }
        self.send(Event::Log(LogLine {
            time: SystemTime::now(),
            level,
            source: source.into(),
            message,
            output: false,
        }));
    }
}

impl SchedulerEvents for EventTx {
    fn job(&self, info: JobInfo) {
        self.send(Event::JobUpdate(info.id, info));
    }
    fn run(&self, snap: RunSnapshot) {
        self.send(Event::RunUpdate(snap));
    }
    fn run_removed(&self, id: RunId) {
        self.send(Event::RunRemoved(id));
    }
}

// ---------------------------------------------------------------------------------------
// CoreHandle

pub struct CoreHandle {
    cmd: mpsc::UnboundedSender<Command>,
    events: Mutex<std::sync::mpsc::Receiver<Event>>,
    playback: Arc<Playback>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Clone, Default)]
pub struct CoreOptions {
    /// Called after every event (the GUI passes `ctx.request_repaint`).
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Overrides the scripts/pidfile directory (tests).
    pub run_dir: Option<PathBuf>,
    /// Skip autostart (tests).
    pub no_autostart: bool,
    /// Health poll interval; default 2 s (500 ms while starting/stopping).
    pub poll: Option<Duration>,
}

impl CoreHandle {
    /// Starts the core on a tokio multi-thread runtime in a background thread.
    pub fn start(cfg: Config, opts: CoreOptions) -> Result<CoreHandle> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let wake = opts.wake.clone().unwrap_or_else(|| Arc::new(|| {}));
        let events = EventTx { tx: ev_tx, wake };
        let playback = Playback::new();
        let library = Library::open(&cfg.library.root)?.with_ffmpeg(Ffmpeg::from_config(&cfg));
        let pb = playback.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let thread = std::thread::Builder::new()
            .name("incremusic-core".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("core")
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(Error::Other(e.to_string())));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                rt.block_on(Service::run(cfg, opts, events, library, pb, cmd_rx));
                rt.shutdown_timeout(Duration::from_secs(2));
            })
            .map_err(|e| Error::Other(e.to_string()))?;
        ready_rx.recv().map_err(|e| Error::Other(e.to_string()))??;
        Ok(CoreHandle {
            cmd: cmd_tx,
            events: Mutex::new(ev_rx),
            playback,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub fn recv_timeout(&self, d: Duration) -> Option<Event> {
        self.events.lock().recv_timeout(d).ok()
    }

    /// Waits for the first event matching `f` (tests).
    pub fn wait_for<T>(
        &self,
        timeout: Duration,
        mut f: impl FnMut(&Event) -> Option<T>,
    ) -> Option<T> {
        let end = Instant::now() + timeout;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Some(e) = self.recv_timeout(left)
                && let Some(t) = f(&e)
            {
                return Some(t);
            }
        }
        None
    }

    /// Sends `Shutdown` and joins the core thread.
    pub fn shutdown(&self, stop_launched: bool) {
        let _ = self.cmd.send(Command::Shutdown { stop_launched });
        if let Some(t) = self.thread.lock().take() {
            let _ = t.join();
        }
    }
}

impl CoreBackend for CoreHandle {
    fn send(&self, cmd: Command) {
        let _ = self.cmd.send(cmd);
    }
    fn try_recv(&self) -> Option<Event> {
        self.events.lock().try_recv().ok()
    }
    fn playback(&self) -> Option<Arc<Playback>> {
        Some(self.playback.clone())
    }
}

impl Drop for CoreHandle {
    fn drop(&mut self) {
        let _ = self.cmd.send(Command::Shutdown {
            stop_launched: false,
        });
    }
}

// ---------------------------------------------------------------------------------------
// Server output

/// Reads what was appended to a log file since the last call, as whole lines.
#[derive(Default)]
struct LogTail {
    pos: u64,
    pending: Vec<u8>,
    /// Starting mid-file: the first line is probably cut off.
    skip_first: bool,
}

impl LogTail {
    /// A line longer than this without a newline is shown anyway.
    const MAX_LINE: usize = 16 * 1024;

    fn read(&mut self, path: &Path) -> Vec<String> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(path) else {
            return vec![];
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.pos {
            // truncated or replaced (relaunch)
            self.pos = 0;
            self.pending.clear();
            self.skip_first = false;
        }
        if len == self.pos || f.seek(SeekFrom::Start(self.pos)).is_err() {
            return vec![];
        }
        let mut buf = Vec::new();
        if f.take(len - self.pos).read_to_end(&mut buf).is_err() {
            return vec![];
        }
        self.pos += buf.len() as u64;
        self.pending.extend_from_slice(&buf);
        let mut lines = Vec::new();
        while let Some(i) = self.pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.pending.drain(..=i).collect();
            lines.push(String::from_utf8_lossy(&raw[..i]).into_owned());
        }
        if self.pending.len() > Self::MAX_LINE {
            lines.extend(self.flush());
        }
        if self.skip_first && !lines.is_empty() {
            self.skip_first = false;
            lines.remove(0);
        }
        lines
            .iter()
            .map(|l| clean_output_line(l))
            .filter(|l| !l.trim().is_empty())
            .collect()
    }

    /// Whatever is left without a newline.
    fn flush(&mut self) -> Vec<String> {
        let raw = std::mem::take(&mut self.pending);
        let l = clean_output_line(&String::from_utf8_lossy(&raw));
        if l.trim().is_empty() { vec![] } else { vec![l] }
    }
}

/// What a terminal would show for one line of output: ANSI escape sequences removed, and
/// only the text after the last carriage return (progress bars redraw with `\r`).
pub fn clean_output_line(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    let line = line.rsplit('\r').next().unwrap_or(line);
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a final byte in @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC (e.g. window title): up to BEL or ESC \
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Highlights server output that looks like a warning or an error.
pub fn output_level(line: &str) -> LogLevel {
    let l = line.to_ascii_lowercase();
    if ["error", "fatal", "panic", "failed", "exception"]
        .iter()
        .any(|w| l.contains(w))
    {
        LogLevel::Error
    } else if l.contains("warn") {
        LogLevel::Warn
    } else {
        LogLevel::Info
    }
}

// ---------------------------------------------------------------------------------------
// Service internals

struct SlotState {
    state: ServerState,
    /// Sticky `Down` (timeout): no auto-reattach until Recheck or restart.
    hold: bool,
    worker_stop: Option<watch::Sender<bool>>,
    /// Launched by this app (or we found its pidfile).
    launched: bool,
    headless_exited: Option<Arc<AtomicBool>>,
    /// Stops the task that follows the server's log file.
    log_tail: Option<watch::Sender<bool>>,
    stop_sent_at: Option<Instant>,
    killed: bool,
    health_failures: u32,
    backend: Option<String>,
}

struct Slot {
    id: ServerId,
    cfg: ServerConfig,
    client: AudioCppClient,
    st: Mutex<SlotState>,
}

struct Ctx {
    cfg: Config,
    ev: EventTx,
    sched: Arc<Scheduler>,
    library: Arc<Mutex<Library>>,
    projects: ProjectStore,
    /// `None` if `runs/` couldn't be opened; runs still work, unrecorded.
    runs: Option<Arc<RunHistory>>,
    ffmpeg: Ffmpeg,
    playback: Arc<Playback>,
    slots: Vec<Slot>,
    run_dir: PathBuf,
    opener: Option<Opener>,
    history: Mutex<Vec<(ServerId, Timing)>>,
    poll: Duration,
    shutting_down: AtomicBool,
}

struct Service;

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(v[v.len() / 2])
}

/// ETA for the next job on `server` (§1.4): median song length × this server's median
/// RTF once it has history; before that, the median `wall_ms` of all jobs.
pub fn estimate_ms(history: &[(ServerId, Timing)], server: ServerId) -> Option<u64> {
    let rtfs: Vec<f64> = history
        .iter()
        .filter(|(s, _)| *s == server)
        .filter_map(|(_, t)| t.rtf)
        .collect();
    let lens: Vec<f64> = history
        .iter()
        .filter_map(|(_, t)| t.audio_duration_ms.map(|d| d as f64))
        .collect();
    if let (Some(r), Some(l)) = (median(rtfs), median(lens)) {
        return Some((r * l) as u64);
    }
    median(history.iter().map(|(_, t)| t.wall_ms as f64).collect()).map(|m| m as u64)
}

impl Ctx {
    fn slot(&self, id: ServerId) -> Option<&Slot> {
        self.slots.get(id.0)
    }

    fn set_state(&self, slot: &Slot, st: &mut SlotState, new: ServerState) {
        if st.state != new {
            st.state = new.clone();
            self.ev.send(Event::ServerStatus(slot.id, new));
        }
    }

    fn estimate(&self, server: ServerId) -> Option<u64> {
        estimate_ms(&self.history.lock(), server)
    }

    fn send_library(&self, d: LibraryDelta) {
        self.ev.send(Event::LibraryChanged(d));
    }

    fn log(&self, level: LogLevel, source: &str, msg: impl Into<String>) {
        self.ev.log(level, source, msg);
    }
}

impl Service {
    async fn run(
        cfg: Config,
        opts: CoreOptions,
        ev: EventTx,
        library: Library,
        playback: Arc<Playback>,
        mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    ) {
        let timeout = Duration::from_secs(cfg.request_timeout_secs);
        let slots = cfg
            .servers
            .iter()
            .enumerate()
            .map(|(i, s)| Slot {
                id: ServerId(i),
                cfg: s.clone(),
                client: AudioCppClient::local(s.port, timeout),
                st: Mutex::new(SlotState {
                    state: ServerState::Stopped,
                    hold: false,
                    worker_stop: None,
                    launched: false,
                    headless_exited: None,
                    log_tail: None,
                    stop_sent_at: None,
                    killed: false,
                    health_failures: 0,
                    backend: None,
                }),
            })
            .collect();
        let opener = match cfg.terminal {
            crate::config::TerminalMode::Native => launcher::detect_opener(&cfg),
            _ => None,
        };
        let ffmpeg = Ffmpeg::from_config(&cfg);
        let projects = ProjectStore::new(library.inputs_dir());
        let runs_dir = library.root().join(crate::library::RUNS);
        let (runs, interrupted) = match RunHistory::open(&runs_dir) {
            Ok((h, n)) => (Some(h), n),
            Err(e) => {
                ev.log(
                    LogLevel::Error,
                    "history",
                    format!("run history off: {}: {e}", runs_dir.display()),
                );
                (None, 0)
            }
        };
        let sched = match &runs {
            Some(h) => Scheduler::new(Box::new(Recorder {
                inner: ev.clone(),
                history: h.clone(),
            })),
            None => Scheduler::new(Box::new(ev.clone())),
        };
        let ctx = Arc::new(Ctx {
            sched: Arc::new(sched),
            runs,
            ev: ev.clone(),
            library: Arc::new(Mutex::new(library)),
            projects,
            ffmpeg,
            playback,
            slots,
            run_dir: opts.run_dir.clone().unwrap_or_else(launcher::runtime_dir),
            opener,
            history: Mutex::new(Vec::new()),
            poll: opts.poll.unwrap_or(Duration::from_secs(2)),
            shutting_down: AtomicBool::new(false),
            cfg,
        });

        Self::startup(&ctx, &opts).await;
        if interrupted > 0 {
            ctx.log(
                LogLevel::Info,
                "history",
                format!("{interrupted} run(s) from the last session were interrupted; resume them from History"),
            );
        }
        Self::send_history(&ctx).await;

        let mut monitors = Vec::new();
        for i in 0..ctx.slots.len() {
            monitors.push(tokio::spawn(Self::monitor(ctx.clone(), ServerId(i))));
        }
        let watcher = Self::spawn_watcher(ctx.clone());
        let ticker = tokio::spawn(Self::playback_ticker(ctx.clone()));

        while let Some(cmd) = cmd_rx.recv().await {
            if let Command::Shutdown { stop_launched } = cmd {
                ctx.shutting_down.store(true, Ordering::SeqCst);
                if stop_launched {
                    for s in &ctx.slots {
                        if s.st.lock().launched {
                            Self::stop_server(&ctx, s.id);
                        }
                    }
                }
                break;
            }
            // commands that wait on servers or disk run as their own tasks so that
            // playback, queue edits etc. are never stuck behind them
            let slow = matches!(
                cmd,
                Command::LaunchServer(_)
                    | Command::RecheckServer(_)
                    | Command::RefreshServers
                    | Command::Retranscribe(_)
                    | Command::Regenerate(_)
                    | Command::ListRuns
                    | Command::LoadRunRecord(_)
                    | Command::ResumeInterrupted(_)
                    | Command::LoadProject(_)
                    | Command::RefreshProjects
                    | Command::RenameProject(..)
                    | Command::DeleteProject(_)
                    | Command::LoadPreset(_)
                    | Command::SavePreset(..)
                    | Command::Play(_)
            );
            if slow {
                let ctx = ctx.clone();
                tokio::spawn(async move { Self::handle(&ctx, cmd).await });
            } else {
                Self::handle(&ctx, cmd).await;
            }
        }
        for m in monitors {
            m.abort();
        }
        ticker.abort();
        drop(watcher);
        for s in &ctx.slots {
            if let Some(w) = s.st.lock().worker_stop.take() {
                let _ = w.send(true);
            }
        }
        if let Some(h) = &ctx.runs {
            h.close();
        }
    }

    async fn startup(ctx: &Arc<Ctx>, opts: &CoreOptions) {
        let ffmpeg = {
            let f = ctx.ffmpeg.clone();
            tokio::task::spawn_blocking(move || f.check())
                .await
                .unwrap_or_else(|e| Err(Error::Other(e.to_string())))
        };
        match &ffmpeg {
            Ok(v) => ctx.log(LogLevel::Info, "core", format!("ffmpeg found: {v}")),
            Err(e) => ctx.log(
                LogLevel::Error,
                "core",
                format!("ffmpeg missing — generation and transcription need it: {e}"),
            ),
        }
        let opener = match (&ctx.cfg.terminal, &ctx.opener) {
            (crate::config::TerminalMode::Native, Some(o)) => Ok(o.describe()),
            (crate::config::TerminalMode::Native, None) => Err(
                "no terminal opener found; set `terminal: Command([...])` in the config"
                    .to_string(),
            ),
            (crate::config::TerminalMode::Command(c), _) => Ok(format!("command: {}", c.join(" "))),
            (crate::config::TerminalMode::Headless, _) => Ok("headless (no terminal)".into()),
        };
        match &opener {
            Ok(o) => ctx.log(LogLevel::Info, "launcher", format!("terminal opener: {o}")),
            Err(e) => ctx.log(LogLevel::Warn, "launcher", e.clone()),
        }
        let (default_preset, default_preset_path) = match ctx.cfg.load_default_preset() {
            Ok(p) => (p, ctx.cfg.defaults.clone()),
            Err(e) => {
                ctx.log(LogLevel::Warn, "config", format!("default preset: {e}"));
                (None, ctx.cfg.defaults.clone())
            }
        };
        ctx.ev.send(Event::Init(InitInfo {
            servers: ctx
                .slots
                .iter()
                .map(|s| ServerInfo {
                    id: s.id,
                    name: s.cfg.name.clone(),
                    port: s.cfg.port,
                    launchable: s.cfg.launch.is_some(),
                    backend: s.cfg.launch.as_ref().map(|l| l.backend.clone()),
                    device: s.cfg.launch.as_ref().and_then(|l| l.device),
                    log_file: (cfg!(unix) && s.cfg.launch.is_some())
                        .then(|| launcher::log_path(&ctx.run_dir, &s.cfg.name)),
                })
                .collect(),
            library_root: ctx.cfg.library.root.clone(),
            models: ctx.cfg.models.clone(),
            default_preset,
            default_preset_path,
            opener,
            ffmpeg: ffmpeg.map_err(|e| e.to_string()),
        }));
        for s in &ctx.slots {
            ctx.ev.send(Event::ServerStatus(s.id, ServerState::Stopped));
        }
        // library: clean staging leftovers, scan
        let lib = ctx.library.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut l = lib.lock();
            let n = l.cleanup_partials()?;
            let d = l.scan()?;
            Ok::<_, Error>((n, d))
        })
        .await;
        match res {
            Ok(Ok((n, d))) => {
                if n > 0 {
                    ctx.log(
                        LogLevel::Info,
                        "library",
                        format!("removed {n} unfinished .part folder(s)"),
                    );
                }
                ctx.send_library(d);
            }
            Ok(Err(e)) => ctx.log(LogLevel::Error, "library", format!("scan failed: {e}")),
            Err(e) => ctx.log(LogLevel::Error, "library", e.to_string()),
        }
        ctx.ev.send(Event::Projects(ctx.projects.summaries()));
        // attach to anything already running; autostart the rest
        for s in &ctx.slots {
            if let Ok(h) = s.client.health().await
                && h.is_ok()
            {
                Self::became_ready(ctx, s.id, h.backend).await;
                continue;
            }
            if !opts.no_autostart && s.cfg.launch.as_ref().is_some_and(|l| l.autostart) {
                Self::launch_server(ctx, s.id).await;
            }
        }
    }

    async fn handle(ctx: &Arc<Ctx>, cmd: Command) {
        match cmd {
            Command::LaunchServer(id) => Self::launch_server(ctx, id).await,
            Command::StopServer(id) => Self::stop_server(ctx, id),
            Command::RecheckServer(id) => {
                if let Some(s) = ctx.slot(id) {
                    s.st.lock().hold = false;
                    match s.client.health().await {
                        Ok(h) if h.is_ok() => Self::became_ready(ctx, id, h.backend).await,
                        Ok(_) | Err(_) => ctx.log(
                            LogLevel::Warn,
                            &s.cfg.name,
                            "recheck: /health does not answer",
                        ),
                    }
                }
            }
            Command::RefreshServers => {
                for s in &ctx.slots {
                    if s.st.lock().hold {
                        continue;
                    }
                    if let Ok(h) = s.client.health().await
                        && h.is_ok()
                        && s.st.lock().state.can_launch()
                    {
                        Self::became_ready(ctx, s.id, h.backend).await;
                    }
                }
            }
            Command::Transcribe {
                project,
                audio,
                force,
            } => Self::start_transcription(ctx, project, audio, force),
            Command::Retranscribe(project) => {
                let p = ctx.projects.clone();
                let n = project.to_string();
                let r = tokio::task::spawn_blocking(move || p.reference_path(&n))
                    .await
                    .ok()
                    .flatten();
                match r {
                    Some(audio) => Self::start_transcription(ctx, project, audio, true),
                    None => ctx.log(
                        LogLevel::Warn,
                        "transcribe",
                        format!("{project}: no reference audio yet"),
                    ),
                }
            }
            Command::StartRun(spec) => Self::start_run(ctx, spec, Origin::default()).await,
            Command::EditRun(id, edit) => match ctx.sched.edit_run(id, &edit) {
                Ok(state) => {
                    if let RunEdit::Params(_) | RunEdit::AbcSource(_) = edit {
                        let (projects, spec) = (ctx.projects.clone(), state.spec.clone());
                        let r = tokio::task::spawn_blocking(move || {
                            projects.write_run_inputs(
                                &spec.name,
                                &spec.params,
                                &spec.abc_source,
                                None,
                            )
                        })
                        .await;
                        if let Ok(Err(e)) = r {
                            ctx.log(LogLevel::Warn, "project", format!("rewriting inputs: {e}"));
                        }
                    }
                }
                Err(e) => ctx.log(LogLevel::Error, "queue", e.to_string()),
            },
            Command::PauseRun(id) => Self::log_err(ctx, "queue", ctx.sched.pause_run(id)),
            Command::ResumeRun(id) => Self::log_err(ctx, "queue", ctx.sched.resume_run(id)),
            Command::StopRun(id) => Self::log_err(ctx, "queue", ctx.sched.stop_run(id)),
            Command::RemoveRun(id) => Self::log_err(ctx, "queue", ctx.sched.remove_run(id)),
            Command::MoveRun(id, to) => Self::log_err(ctx, "queue", ctx.sched.move_run(id, to)),
            Command::CancelJob(id) => Self::log_err(ctx, "queue", ctx.sched.cancel_job(id)),
            Command::ClearFinishedRuns => ctx.sched.clear_done(),
            Command::Regenerate(song) => Self::regenerate(ctx, song).await,
            Command::ListRuns => Self::send_history(ctx).await,
            Command::LoadRunRecord(id) => {
                let r = Self::load_record(ctx, id).await;
                ctx.ev.send(Event::RunRecord(id, r));
            }
            Command::ResumeInterrupted(id) => Self::resume_interrupted(ctx, id).await,
            Command::Library(lc) => Self::library_cmd(ctx, lc),
            Command::LoadProject(name) => {
                let p = ctx.projects.clone();
                let r = tokio::task::spawn_blocking(move || p.inputs(&name)).await;
                let r = r
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()));
                ctx.ev.send(Event::ProjectInputs(r));
            }
            Command::RefreshProjects => Self::send_projects(ctx).await,
            Command::RenameProject(from, to) => {
                let p = ctx.projects.clone();
                let (f2, t2) = (from.clone(), to.clone());
                match tokio::task::spawn_blocking(move || p.rename(&f2, &t2)).await {
                    Ok(Ok(())) => {
                        ctx.log(LogLevel::Info, "project", format!("renamed {from} → {to}"))
                    }
                    Ok(Err(e)) => {
                        ctx.log(LogLevel::Error, "project", format!("rename {from}: {e}"))
                    }
                    Err(e) => ctx.log(LogLevel::Error, "project", e.to_string()),
                }
                Self::send_projects(ctx).await;
            }
            Command::DeleteProject(name) => {
                let p = ctx.projects.clone();
                let n2 = name.clone();
                match tokio::task::spawn_blocking(move || p.trash(&n2)).await {
                    Ok(Ok(())) => ctx.log(
                        LogLevel::Info,
                        "project",
                        format!("moved {name} to the trash"),
                    ),
                    Ok(Err(e)) => {
                        ctx.log(LogLevel::Error, "project", format!("delete {name}: {e}"))
                    }
                    Err(e) => ctx.log(LogLevel::Error, "project", e.to_string()),
                }
                Self::send_projects(ctx).await;
            }
            Command::SetKeypoints(name, k) => {
                if let Err(e) = ctx.projects.set_keypoints(&name, k) {
                    ctx.log(
                        LogLevel::Error,
                        "project",
                        format!("keypoints of {name}: {e}"),
                    );
                    Self::send_projects(ctx).await;
                }
            }
            Command::PlayReference(name) => Self::play_reference(ctx, name),
            Command::LoadPreset(path) => {
                let r = tokio::task::spawn_blocking(move || Preset::load(&path).map(|p| (path, p)))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()));
                ctx.ev.send(Event::PresetLoaded(r));
            }
            Command::SavePreset(path, preset) => {
                let p2 = path.clone();
                match tokio::task::spawn_blocking(move || preset.save(&p2)).await {
                    Ok(Ok(())) => ctx.log(
                        LogLevel::Info,
                        "preset",
                        format!("saved {}", path.display()),
                    ),
                    Ok(Err(e)) => ctx.log(LogLevel::Error, "preset", e.to_string()),
                    Err(e) => ctx.log(LogLevel::Error, "preset", e.to_string()),
                }
            }
            Command::Play(song) => Self::play(ctx, song),
            Command::Seek(d) => {
                ctx.playback.seek(d);
                ctx.ev
                    .send(Event::PlaybackPosition(ctx.playback.position()));
            }
            Command::SeekBy(d) => {
                ctx.playback.seek_by(d);
                ctx.ev
                    .send(Event::PlaybackPosition(ctx.playback.position()));
            }
            Command::Pause => ctx.playback.pause(),
            Command::Resume => ctx.playback.play(),
            Command::TogglePause => ctx.playback.toggle(),
            Command::Stop => ctx.playback.stop(),
            Command::Shutdown { .. } => {}
        }
    }

    fn log_err(ctx: &Ctx, source: &str, r: Result<()>) {
        if let Err(e) = r {
            ctx.log(LogLevel::Error, source, e.to_string());
        }
    }

    // -----------------------------------------------------------------------------------
    // Servers (§4)

    async fn launch_server(ctx: &Arc<Ctx>, id: ServerId) {
        let Some(s) = ctx.slot(id) else { return };
        {
            let st = s.st.lock();
            if !st.state.can_launch() {
                ctx.log(
                    LogLevel::Warn,
                    &s.cfg.name,
                    format!("cannot launch while {}", st.state.label()),
                );
                return;
            }
        }
        // already answering → attach, don't launch
        if let Ok(h) = s.client.health().await
            && h.is_ok()
        {
            s.st.lock().hold = false;
            ctx.log(LogLevel::Info, &s.cfg.name, "already running; attaching");
            Self::became_ready(ctx, id, h.backend).await;
            return;
        }
        let plan = match LaunchPlan::new(&ctx.cfg, &s.cfg, &ctx.run_dir) {
            Ok(p) => p,
            Err(e) => {
                ctx.log(LogLevel::Error, &s.cfg.name, e.to_string());
                return;
            }
        };
        match launcher::launch(&plan, &ctx.cfg.terminal, ctx.opener.as_ref()) {
            Ok(launched) => {
                let mut st = s.st.lock();
                st.launched = true;
                st.hold = false;
                st.stop_sent_at = None;
                st.killed = false;
                st.health_failures = 0;
                match launched {
                    Launched::Headless(child) => {
                        st.headless_exited =
                            Some(Self::watch_headless(ctx.clone(), s.cfg.name.clone(), child));
                    }
                    Launched::Detached { opener } => {
                        st.headless_exited = None;
                        tokio::spawn(Self::watch_opener(ctx.clone(), id, opener));
                    }
                }
                if cfg!(unix) {
                    st.log_tail = Some(Self::tail_log(
                        ctx.clone(),
                        s.cfg.name.clone(),
                        plan.log_path(),
                        false,
                    ));
                }
                ctx.set_state(
                    s,
                    &mut st,
                    ServerState::Starting {
                        since: SystemTime::now(),
                    },
                );
                drop(st);
                ctx.log(
                    LogLevel::Info,
                    &s.cfg.name,
                    format!("launched: {}", plan.argv.join(" ")),
                );
            }
            Err(e) => {
                let mut st = s.st.lock();
                ctx.set_state(s, &mut st, ServerState::Down(e.to_string()));
                drop(st);
                ctx.log(LogLevel::Error, &s.cfg.name, e.to_string());
            }
        }
    }

    /// Pipes a headless server's stdout/stderr into the log and notes when it exits.
    fn watch_headless(
        ctx: Arc<Ctx>,
        name: String,
        mut child: tokio::process::Child,
    ) -> Arc<AtomicBool> {
        let exited = Arc::new(AtomicBool::new(false));
        for (stream, level) in [
            (
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
                LogLevel::Info,
            ),
            (
                child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
                LogLevel::Warn,
            ),
        ] {
            if let Some(stream) = stream {
                let (ctx, name) = (ctx.clone(), name.clone());
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(stream).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        ctx.ev.send(Event::Log(LogLine {
                            time: SystemTime::now(),
                            level: output_level(&line).max(level),
                            source: name.clone(),
                            message: clean_output_line(&line),
                            output: true,
                        }));
                    }
                });
            }
        }
        let flag = exited.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            flag.store(true, Ordering::SeqCst);
            tracing::info!("{name} exited: {status:?}");
        });
        exited
    }

    /// Reaps the command that opened the terminal. If it fails, the server never started,
    /// and its output (e.g. gio's "Unable to find terminal") is the only explanation.
    async fn watch_opener(ctx: Arc<Ctx>, id: ServerId, opener: tokio::process::Child) {
        let Some(s) = ctx.slot(id) else { return };
        let Ok(out) = opener.wait_with_output().await else {
            return;
        };
        let text = [&out.stdout[..], &out.stderr[..]]
            .map(String::from_utf8_lossy)
            .join("\n");
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if out.status.success() {
            for l in lines {
                ctx.log(LogLevel::Info, &s.cfg.name, format!("terminal opener: {l}"));
            }
            return;
        }
        let why = if lines.is_empty() {
            format!("terminal opener failed ({})", out.status)
        } else {
            format!(
                "terminal opener failed ({}): {}",
                out.status,
                lines.join(" / ")
            )
        };
        ctx.log(LogLevel::Error, &s.cfg.name, why.clone());
        let started = launcher::read_pid(&Self::pidfile(&ctx, s)).is_some();
        if !started && matches!(s.st.lock().state, ServerState::Starting { .. }) {
            if matches!(ctx.cfg.terminal, crate::config::TerminalMode::Native) {
                ctx.log(
                    LogLevel::Error,
                    &s.cfg.name,
                    "no terminal could be opened; install a terminal emulator, or set `terminal: Headless` or `terminal: Command([...])` in the config",
                );
            }
            Self::mark_down(&ctx, id, &why, false);
        }
    }

    /// Follows the log file the launcher script writes (everything the server prints) into
    /// the log, until the returned sender is dropped or set. `from_end`: start with only the
    /// last few KB (attaching to a server launched earlier).
    fn tail_log(ctx: Arc<Ctx>, name: String, path: PathBuf, from_end: bool) -> watch::Sender<bool> {
        const BACKLOG: u64 = 16 * 1024;
        let (tx, mut stop) = watch::channel(false);
        tokio::spawn(async move {
            let mut tail = LogTail::default();
            if from_end {
                let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                tail.pos = len.saturating_sub(BACKLOG);
                tail.skip_first = tail.pos > 0;
            }
            loop {
                let stopping = tokio::select! {
                    r = stop.changed() => r.is_err() || *stop.borrow(),
                    _ = tokio::time::sleep(Duration::from_millis(250)) => false,
                };
                let stopping = stopping || ctx.shutting_down.load(Ordering::SeqCst);
                let mut lines = tail.read(&path);
                if stopping {
                    lines.extend(tail.flush());
                }
                for line in lines {
                    ctx.ev.send(Event::Log(LogLine {
                        time: SystemTime::now(),
                        level: output_level(&line),
                        source: name.clone(),
                        message: line,
                        output: true,
                    }));
                }
                if stopping {
                    break;
                }
            }
        });
        tx
    }

    fn pidfile(ctx: &Ctx, s: &Slot) -> PathBuf {
        ctx.run_dir.join(format!("{}.pid", s.cfg.name))
    }

    fn stop_server(ctx: &Arc<Ctx>, id: ServerId) {
        let Some(s) = ctx.slot(id) else { return };
        let pidfile = Self::pidfile(ctx, s);
        let Some(pid) = launcher::read_pid(&pidfile) else {
            ctx.log(
                LogLevel::Warn,
                &s.cfg.name,
                "not launched by this app (no pidfile); stop it where it runs",
            );
            return;
        };
        let mut st = s.st.lock();
        if matches!(
            st.state,
            ServerState::Stopping { .. } | ServerState::Stopped
        ) {
            return;
        }
        if let Err(e) = launcher::terminate(pid, false) {
            drop(st);
            ctx.log(LogLevel::Error, &s.cfg.name, e.to_string());
            return;
        }
        st.launched = true;
        st.stop_sent_at = Some(Instant::now());
        st.killed = false;
        if let Some(w) = st.worker_stop.take() {
            let _ = w.send(true);
        }
        ctx.set_state(
            s,
            &mut st,
            ServerState::Stopping {
                since: SystemTime::now(),
            },
        );
        drop(st);
        ctx.log(LogLevel::Info, &s.cfg.name, format!("stopping (pid {pid})"));
    }

    /// Marks a server `Ready` and starts its worker.
    async fn became_ready(ctx: &Arc<Ctx>, id: ServerId, backend: Option<String>) {
        let Some(s) = ctx.slot(id) else { return };
        let loaded = s
            .client
            .list_models()
            .await
            .map(|m| m.into_iter().filter(|m| m.loaded).map(|m| m.id).collect())
            .unwrap_or_default();
        let mut st = s.st.lock();
        if matches!(st.state, ServerState::Stopping { .. }) {
            return;
        }
        st.health_failures = 0;
        if backend.is_some() {
            st.backend = backend.clone();
        }
        if matches!(st.state, ServerState::Busy { .. }) {
            return;
        }
        ctx.set_state(
            s,
            &mut st,
            ServerState::Ready {
                backend,
                loaded_models: loaded,
            },
        );
        let log = launcher::log_path(&ctx.run_dir, &s.cfg.name);
        if st.log_tail.is_none() && log.is_file() {
            // launched earlier (e.g. by a previous session of the app): show its output
            st.log_tail = Some(Self::tail_log(ctx.clone(), s.cfg.name.clone(), log, true));
        }
        if st.worker_stop.is_none() {
            let (tx, rx) = watch::channel(false);
            st.worker_stop = Some(tx);
            tokio::spawn(Self::worker(ctx.clone(), id, rx));
        }
        drop(st);
        ctx.sched.wake();
        tokio::spawn(Self::check_model_paths(ctx.clone(), id));
    }

    /// Asks the server whether the configured model files exist (`/v1/ui/path-status`).
    /// Only warns: the server stays usable, and a load would fail with the server's error.
    async fn check_model_paths(ctx: Arc<Ctx>, id: ServerId) {
        let Some(s) = ctx.slot(id) else { return };
        let m = &ctx.cfg.models;
        match models::check_paths(&s.client, &[&m.yue2, &m.sheetsage2]).await {
            Ok(problems) => {
                for p in &problems {
                    ctx.log(LogLevel::Warn, &s.cfg.name, format!("model path: {p}"));
                }
                ctx.ev.send(Event::ModelPaths(id, problems));
            }
            Err(e) => ctx.log(
                LogLevel::Info,
                &s.cfg.name,
                format!("can't check model paths (needs --ui-management): {e}"),
            ),
        }
    }

    fn mark_down(ctx: &Ctx, id: ServerId, reason: &str, hold: bool) {
        let Some(s) = ctx.slot(id) else { return };
        let mut st = s.st.lock();
        if matches!(st.state, ServerState::Stopping { .. }) {
            return;
        }
        st.hold = hold;
        if let Some(w) = st.worker_stop.take() {
            let _ = w.send(true);
        }
        ctx.set_state(s, &mut st, ServerState::Down(reason.to_string()));
        drop(st);
        ctx.log(LogLevel::Warn, &s.cfg.name, format!("down: {reason}"));
    }

    /// Health/PID monitor for one server (§4.2).
    async fn monitor(ctx: Arc<Ctx>, id: ServerId) {
        let s = &ctx.slots[id.0];
        let fast = Duration::from_millis(500).min(ctx.poll);
        loop {
            let state = s.st.lock().state.clone();
            let interval = match state {
                ServerState::Starting { .. } | ServerState::Stopping { .. } => fast,
                _ => ctx.poll,
            };
            tokio::time::sleep(interval).await;
            if ctx.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            let pid = launcher::read_pid(&Self::pidfile(&ctx, s));
            let headless_exited =
                s.st.lock()
                    .headless_exited
                    .as_ref()
                    .is_some_and(|f| f.load(Ordering::SeqCst));
            let pid_gone = headless_exited || pid.is_some_and(|p| !launcher::pid_alive(p));
            match state {
                ServerState::Stopping { .. } => {
                    let port = s.cfg.port;
                    let port_open = tokio::task::spawn_blocking(move || launcher::port_open(port))
                        .await
                        .unwrap_or(false);
                    let alive = !pid_gone && pid.is_some();
                    let mut st = s.st.lock();
                    if !alive && !port_open {
                        st.launched = false;
                        st.headless_exited = None;
                        // the tail task reads the last output, then ends
                        st.log_tail = None;
                        let _ = std::fs::remove_file(Self::pidfile(&ctx, s));
                        ctx.set_state(s, &mut st, ServerState::Stopped);
                        drop(st);
                        ctx.log(LogLevel::Info, &s.cfg.name, "stopped");
                    } else if alive
                        && !st.killed
                        && st
                            .stop_sent_at
                            .is_some_and(|t| t.elapsed() >= Duration::from_secs(10))
                    {
                        st.killed = true;
                        drop(st);
                        if let Some(p) = pid {
                            let _ = launcher::terminate(p, true);
                            ctx.log(LogLevel::Info, &s.cfg.name, "still running after 10 s; sent kill (it exits when its GPU job finishes)");
                        }
                    }
                }
                ServerState::Starting { since } => match s.client.health().await {
                    Ok(h) if h.is_ok() => {
                        ctx.log(LogLevel::Info, &s.cfg.name, "healthy");
                        Self::became_ready(&ctx, id, h.backend).await;
                    }
                    _ => {
                        if pid_gone {
                            Self::mark_down(&ctx, id, "server exited during startup", false);
                        } else if since.elapsed().unwrap_or_default() > Duration::from_secs(60) {
                            let why = if pid.is_none() {
                                "no healthy /health within 60 s; the launcher script never ran (no pidfile)"
                            } else {
                                "no healthy /health within 60 s"
                            };
                            Self::mark_down(&ctx, id, why, false);
                        }
                    }
                },
                ServerState::Ready { .. } | ServerState::Busy { .. } => {
                    let health = s.client.health().await;
                    let ok = matches!(&health, Ok(h) if h.is_ok());
                    if ok && !pid_gone {
                        s.st.lock().health_failures = 0;
                        continue;
                    }
                    let failures = {
                        let mut st = s.st.lock();
                        st.health_failures += 1;
                        st.health_failures
                    };
                    if pid_gone {
                        Self::mark_down(&ctx, id, "server process exited", false);
                    } else if failures >= 2 {
                        let why = match health {
                            Err(e) => e.to_string(),
                            Ok(h) => format!("status {}", h.status),
                        };
                        Self::mark_down(&ctx, id, &format!("health check failed: {why}"), false);
                    }
                }
                ServerState::Stopped | ServerState::Down(_) => {
                    if s.st.lock().hold {
                        continue;
                    }
                    if let Ok(h) = s.client.health().await
                        && h.is_ok()
                    {
                        ctx.log(
                            LogLevel::Info,
                            &s.cfg.name,
                            "answering on its port; attaching",
                        );
                        Self::became_ready(&ctx, id, h.backend).await;
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // Workers (§5.3)

    async fn worker(ctx: Arc<Ctx>, id: ServerId, mut stop: watch::Receiver<bool>) {
        let s = &ctx.slots[id.0];
        loop {
            if *stop.borrow() {
                break;
            }
            let item = tokio::select! {
                biased;
                _ = stop.changed() => break,
                w = ctx.sched.wait_work(id, || ctx.estimate(id)) => w,
            };
            let keep_going = match item {
                WorkItem::Transcribe(t) => Self::do_transcribe(&ctx, s, t).await,
                WorkItem::Generate(job) => Self::do_generate(&ctx, s, job).await,
            };
            if !keep_going {
                break;
            }
        }
        let mut st = s.st.lock();
        st.worker_stop = None;
        drop(st);
        tracing::debug!("worker for {} exited", s.cfg.name);
    }

    fn set_busy(ctx: &Ctx, s: &Slot, job: Option<JobId>) {
        let mut st = s.st.lock();
        if st.state.is_up() {
            ctx.set_state(
                s,
                &mut st,
                ServerState::Busy {
                    job,
                    since: SystemTime::now(),
                },
            );
        }
    }

    fn set_idle(ctx: &Ctx, s: &Slot, loaded: Vec<String>) {
        let mut st = s.st.lock();
        if matches!(st.state, ServerState::Busy { .. }) {
            let backend = st.backend.clone();
            ctx.set_state(
                s,
                &mut st,
                ServerState::Ready {
                    backend,
                    loaded_models: loaded,
                },
            );
        }
    }

    fn still_ready(s: &Slot) -> bool {
        s.st.lock().state.is_up()
    }

    async fn do_transcribe(ctx: &Arc<Ctx>, s: &Slot, t: TranscribeTask) -> bool {
        Self::set_busy(ctx, s, None);
        ctx.log(LogLevel::Info, &s.cfg.name, "transcribing with SheetSage2");
        let res = transcribe::run_on_server(&s.client, &t.model, &t.wav).await;
        let transport = matches!(&res, Err(Error::Transport(_) | Error::Timeout));
        let _ = t.reply.send(res);
        Self::set_idle(ctx, s, vec![]);
        if transport && let Err(_) | Ok(false) = s.client.health().await.map(|h| h.is_ok()) {
            Self::mark_down(
                ctx,
                s.id,
                "server stopped answering during transcription",
                false,
            );
            return false;
        }
        Self::still_ready(s)
    }

    /// Runs one job end to end. Returns whether the worker should continue.
    async fn do_generate(ctx: &Arc<Ctx>, s: &Slot, job: Job) -> bool {
        Self::set_busy(ctx, s, Some(job.id));
        let backend = s.client.health().await.ok().and_then(|h| h.backend);
        // stage: reserve a collision-free stem and its .part folder
        let reserved = {
            let mut lib = ctx.library.lock();
            match &job.regenerate_of {
                Some(orig) => lib.reserve_regen(orig),
                None => lib.reserve(&job.stem()),
            }
        };
        let (stem, part) = match reserved {
            Ok(r) => r,
            Err(e) => {
                ctx.sched
                    .finish(&job, Outcome::Failed(format!("staging: {e}")));
                Self::set_idle(ctx, s, vec![]);
                return Self::still_ready(s);
            }
        };
        let wav = part.join(format!("{stem}.wav"));
        let body = job.request_body();
        let result = async {
            models::ensure_loaded(&s.client, &job.model).await?;
            let sched = ctx.sched.clone();
            let jid = job.id;
            s.client
                .generate(&body, &wav, move || sched.is_cancelled(jid))
                .await
        }
        .await;
        let loaded = vec![job.model.id.clone()];
        match result {
            Ok(GenerateOutcome::Discarded) => {
                ctx.library.lock().abandon(&stem, &part);
                ctx.sched.finish(&job, Outcome::Discarded);
                ctx.log(
                    LogLevel::Info,
                    &s.cfg.name,
                    format!("{} seed {} discarded (cancelled)", job.run_name, job.seed),
                );
                Self::set_idle(ctx, s, loaded);
                Self::still_ready(s)
            }
            Ok(GenerateOutcome::Done(g)) => {
                if ctx.sched.is_cancelled(job.id) {
                    ctx.library.lock().abandon(&stem, &part);
                    ctx.sched.finish(&job, Outcome::Discarded);
                    Self::set_idle(ctx, s, loaded);
                    return Self::still_ready(s);
                }
                if let Some(m) = &g.header_mismatch {
                    ctx.log(LogLevel::Warn, &s.cfg.name, m.clone());
                }
                ctx.history.lock().push((s.id, g.timing.clone()));
                ctx.sched.finish(&job, Outcome::Encoding(g.timing.clone()));
                Self::set_idle(ctx, s, loaded);
                let server = RecipeServer {
                    name: s.cfg.name.clone(),
                    port: s.cfg.port,
                    backend,
                    device: s.cfg.launch.as_ref().and_then(|l| l.device),
                };
                Self::spawn_encode(ctx.clone(), job, stem, part, g, body, server);
                Self::still_ready(s)
            }
            Err(e) => {
                ctx.library.lock().abandon(&stem, &part);
                Self::job_error(ctx, s, &job, e).await
            }
        }
    }

    async fn job_error(ctx: &Arc<Ctx>, s: &Slot, job: &Job, e: Error) -> bool {
        let what = format!("{} seed {}", job.run_name, job.seed);
        match e {
            e if e.is_client_error() => {
                ctx.log(LogLevel::Error, &s.cfg.name, format!("{what} failed: {e}"));
                ctx.sched.finish(job, Outcome::Failed(e.to_string()));
                Self::set_idle(ctx, s, vec![]);
                Self::still_ready(s)
            }
            Error::Timeout => {
                // the server probably keeps working (§5.3): requeue elsewhere, park this one
                ctx.sched
                    .finish(job, Outcome::Retry("request timed out".into()));
                Self::mark_down(ctx, s.id, "request timed out; may still be busy", true);
                false
            }
            e => {
                let healthy = matches!(s.client.health().await, Ok(h) if h.is_ok());
                ctx.log(
                    LogLevel::Warn,
                    &s.cfg.name,
                    format!("{what}: {e}; requeueing"),
                );
                ctx.sched.finish(job, Outcome::Retry(e.to_string()));
                if healthy {
                    Self::set_idle(ctx, s, vec![]);
                    Self::still_ready(s)
                } else {
                    Self::mark_down(ctx, s.id, &format!("crashed during a job: {e}"), false);
                    false
                }
            }
        }
    }

    /// Encoding runs on the blocking pool so the GPU never waits for it (§5.3).
    fn spawn_encode(
        ctx: Arc<Ctx>,
        job: Job,
        stem: String,
        part: PathBuf,
        g: Generated,
        body: serde_json::Value,
        server: RecipeServer,
    ) {
        tokio::spawn(async move {
            let c = ctx.clone();
            let (job2, stem2, part2) = (job.clone(), stem.clone(), part.clone());
            let res = tokio::task::spawn_blocking(move || {
                Self::finalize(&c, &job2, &stem2, &part2, &g, body, server)
            })
            .await;
            let res = res.map_err(|e| Error::Other(e.to_string())).and_then(|r| r);
            match res {
                Ok(delta) => {
                    let id = delta
                        .upserted
                        .first()
                        .map(|s| s.id.clone())
                        .unwrap_or(SongId(String::new()));
                    ctx.send_library(delta);
                    ctx.sched.finish(&job, Outcome::Done(id));
                }
                Err(e) => {
                    ctx.library.lock().abandon(&stem, &part);
                    ctx.log(LogLevel::Error, "encoder", format!("{stem}: {e}"));
                    ctx.sched
                        .finish(&job, Outcome::Failed(format!("encoding: {e}")));
                }
            }
        });
    }

    fn finalize(
        ctx: &Ctx,
        job: &Job,
        stem: &str,
        part: &Path,
        g: &Generated,
        body: serde_json::Value,
        server: RecipeServer,
    ) -> Result<LibraryDelta> {
        let wav = part.join(format!("{stem}.wav"));
        let mp3 = part.join(format!("{stem}.mp3"));
        ctx.ffmpeg.encode(&wav, &mp3, ctx.cfg.library.encoder)?;
        let song_id = ulid::Ulid::generate().to_string();
        let recipe = Recipe {
            schema: media::RECIPE_SCHEMA,
            app_version: crate::APP_VERSION.into(),
            song_id,
            run: RecipeRun {
                id: job.run_id,
                name: job.run_name.to_string(),
                seed: job.seed,
                start_seed: job.start_seed,
                index: job.index,
                revision: job.revision,
            },
            created_at: fsutil::now_rfc3339(),
            server,
            model: RecipeModel {
                spec: job.model.clone(),
                file_hashes: BTreeMap::new(),
            },
            request: body,
            project: job.run_name.to_string(),
            abc_source: job.abc_source.clone(),
            timing: g.timing.clone(),
            output: RecipeOutput {
                sample_rate: g.sample_rate,
                channels: g.channels,
                wav_sha256: g.wav_sha256.clone(),
                encoder: ctx.cfg.library.encoder.describe(),
            },
        };
        let meta = SongMeta {
            title: Some(stem.to_string()),
            recipe: Some(recipe),
            ..Default::default()
        };
        media::write_meta(&mp3, &meta)?;
        ctx.library.lock().commit(stem, part)
    }

    // -----------------------------------------------------------------------------------
    // Runs, transcription, library, playback

    async fn start_run(ctx: &Arc<Ctx>, spec: RunSpec, origin: Origin) {
        let problems = spec.params.validate();
        if !problems.is_empty() {
            ctx.log(
                LogLevel::Error,
                "queue",
                format!("run `{}` not started: {}", spec.name, problems.join("; ")),
            );
            return;
        }
        let id = RunId::new();
        let (projects, s2) = (ctx.projects.clone(), spec.clone());
        let r = tokio::task::spawn_blocking(move || {
            projects.write_run_inputs(&s2.name, &s2.params, &s2.abc_source, Some(id))
        })
        .await;
        if let Ok(Err(e)) = r {
            ctx.log(
                LogLevel::Warn,
                "project",
                format!("writing inputs for `{}`: {e}", spec.name),
            );
        }
        let count = spec
            .count
            .map(|c| c.to_string())
            .unwrap_or_else(|| "until stopped".into());
        ctx.log(
            LogLevel::Info,
            "queue",
            format!(
                "run `{}` from seed {} ({count})",
                spec.name, spec.start_seed
            ),
        );
        if let Some(h) = &ctx.runs {
            h.note_origin(id, origin.clone());
        }
        ctx.sched.add_run_with(id, spec, origin.regenerate_of);
        ctx.ev.send(Event::Projects(ctx.projects.summaries()));
    }

    async fn regenerate(ctx: &Arc<Ctx>, song: SongId) {
        let found = ctx.library.lock().get(&song).cloned();
        let Some(s) = found else { return };
        let Some(recipe) = s.meta.recipe.clone() else {
            ctx.log(
                LogLevel::Error,
                "library",
                format!("{}: no recipe to regenerate from", s.stem),
            );
            return;
        };
        let parsed = recipe
            .request
            .get("request")
            .ok_or_else(|| Error::BadResponse("recipe has no request".into()))
            .and_then(GenerationParams::from_request)
            .and_then(|(p, seed)| Ok((RunName::parse(&recipe.run.name)?, p, seed)));
        match parsed {
            Ok((name, params, seed)) => {
                ctx.ev.send(Event::Recipe(
                    song.clone(),
                    Ok((
                        name.clone(),
                        params.clone(),
                        seed,
                        recipe.abc_source.clone(),
                    )),
                ));
                let spec = RunSpec {
                    name,
                    params,
                    start_seed: seed,
                    count: Some(1),
                    model: recipe.model.spec.clone(),
                    abc_source: recipe.abc_source.clone(),
                };
                let base = recipe.run.name.clone() + "-" + &recipe.run.seed.to_string();
                let origin = Origin {
                    regenerate_of: Some(base),
                    resumed_from: None,
                };
                Self::start_run(ctx, spec, origin).await;
            }
            Err(e) => ctx.log(LogLevel::Error, "library", format!("{}: {e}", s.stem)),
        }
    }

    fn start_transcription(ctx: &Arc<Ctx>, project: RunName, audio: PathBuf, force: bool) {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            ctx.ev.send(Event::TranscribeStarted(project.to_string()));
            let sched = ctx.sched.clone();
            let model = ctx.cfg.models.sheetsage2.clone();
            let res = transcribe::transcribe(
                &ctx.projects,
                &ctx.ffmpeg,
                &project,
                &audio,
                force,
                move |wav| async move {
                    let (tx, rx) = oneshot::channel();
                    sched.enqueue_transcribe(TranscribeTask {
                        wav,
                        model,
                        reply: tx,
                    });
                    rx.await
                        .map_err(|_| Error::Other("transcription dropped".into()))?
                },
            )
            .await;
            match &res {
                Ok(s) if s.reused => ctx.log(
                    LogLevel::Info,
                    "transcribe",
                    format!("{}: reused an existing transcription", project),
                ),
                Ok(s) => ctx.log(
                    LogLevel::Info,
                    "transcribe",
                    format!("{}: done in {} ms", project, s.wall_ms.unwrap_or(0)),
                ),
                Err(e) => ctx.log(LogLevel::Error, "transcribe", format!("{project}: {e}")),
            }
            ctx.ev
                .send(Event::TranscribeDone(res.map_err(|e| e.to_string())));
            ctx.ev.send(Event::Projects(ctx.projects.summaries()));
        });
    }

    fn library_cmd(ctx: &Arc<Ctx>, cmd: LibraryCommand) {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let lib = ctx.library.clone();
            let projects = ctx.projects.clone();
            let c2 = cmd.clone();
            let res = tokio::task::spawn_blocking(move || match &c2 {
                // encode without holding the library lock
                LibraryCommand::Export {
                    ids,
                    dest,
                    format,
                    strip_metadata,
                } => {
                    let (songs, ff) = {
                        let l = lib.lock();
                        (l.export_plan(ids)?, l.ffmpeg().cloned())
                    };
                    crate::library::export_songs(
                        &songs,
                        ff.as_ref(),
                        dest,
                        *format,
                        *strip_metadata,
                        &|song| Some(projects.load(&song.project()?).ok()?.keypoints),
                    )?;
                    Ok(LibraryDelta::default())
                }
                _ => lib.lock().apply(&c2),
            })
            .await;
            match res {
                Ok(Ok(delta)) => {
                    if let LibraryCommand::Export { ids, dest, .. } = &cmd {
                        ctx.log(
                            LogLevel::Info,
                            "library",
                            format!("exported {} song(s) to {}", ids.len(), dest.display()),
                        );
                    }
                    if delta != LibraryDelta::default() {
                        ctx.send_library(delta);
                    }
                }
                Ok(Err(e)) => ctx.log(LogLevel::Error, "library", e.to_string()),
                Err(e) => ctx.log(LogLevel::Error, "library", e.to_string()),
            }
        });
    }

    fn play(ctx: &Arc<Ctx>, song: SongId) {
        let Some(s) = ctx.library.lock().get(&song).cloned() else {
            return;
        };
        ctx.playback.set_loading(song.clone());
        ctx.ev.send(Event::PlaybackState {
            state: PlayState::Loading,
            song: Some(song.clone()),
            duration: Duration::ZERO,
        });
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let path = s.mp3.clone();
            let res = tokio::task::spawn_blocking(move || playback::decode_file(&path)).await;
            match res {
                Ok(Ok(decoded)) => {
                    let peaks = media::peaks(&decoded.samples, decoded.channels, 800);
                    let dur = decoded.duration();
                    if ctx.playback.load(song.clone(), Arc::new(decoded), true) {
                        ctx.ev.send(Event::Peaks(song.clone(), peaks));
                        ctx.ev.send(Event::PlaybackState {
                            state: PlayState::Playing,
                            song: Some(song),
                            duration: dur,
                        });
                    }
                }
                Ok(Err(e)) => ctx.log(LogLevel::Error, "playback", format!("{}: {e}", s.stem)),
                Err(e) => ctx.log(LogLevel::Error, "playback", e.to_string()),
            }
        });
    }

    fn play_reference(ctx: &Arc<Ctx>, name: String) {
        let Some(path) = ctx.projects.reference_path(&name) else {
            ctx.log(
                LogLevel::Warn,
                "playback",
                format!("{name}: no reference audio"),
            );
            return;
        };
        let id = crate::project::reference_play_id(&name);
        ctx.playback.set_loading(id.clone());
        ctx.ev.send(Event::PlaybackState {
            state: PlayState::Loading,
            song: Some(id.clone()),
            duration: Duration::ZERO,
        });
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let res = tokio::task::spawn_blocking(move || playback::decode_file(&path)).await;
            match res {
                Ok(Ok(decoded)) => {
                    let peaks = media::peaks(&decoded.samples, decoded.channels, 800);
                    let dur = decoded.duration();
                    if ctx.playback.load(id.clone(), Arc::new(decoded), true) {
                        ctx.ev.send(Event::Peaks(id.clone(), peaks));
                        ctx.ev.send(Event::PlaybackState {
                            state: PlayState::Playing,
                            song: Some(id),
                            duration: dur,
                        });
                    }
                }
                Ok(Err(e)) => ctx.log(
                    LogLevel::Error,
                    "playback",
                    format!("{name} reference: {e}"),
                ),
                Err(e) => ctx.log(LogLevel::Error, "playback", e.to_string()),
            }
        });
    }

    async fn send_history(ctx: &Arc<Ctx>) {
        let Some(h) = ctx.runs.clone() else { return };
        let Ok((list, errors)) = tokio::task::spawn_blocking(move || h.summaries()).await else {
            return;
        };
        for e in errors {
            ctx.log(LogLevel::Warn, "history", format!("skipped {e}"));
        }
        ctx.ev.send(Event::RunHistory(list));
    }

    async fn load_record(ctx: &Arc<Ctx>, id: RunId) -> Result<RunRecord, String> {
        let h = ctx.runs.clone().ok_or("run history is off")?;
        tokio::task::spawn_blocking(move || h.load(id))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())
    }

    async fn resume_interrupted(ctx: &Arc<Ctx>, id: RunId) {
        let r = match Self::load_record(ctx, id).await {
            Ok(r) => r,
            Err(e) => {
                ctx.log(LogLevel::Error, "history", format!("run {id}: {e}"));
                return;
            }
        };
        if r.status != crate::history::RecordStatus::Interrupted {
            ctx.log(
                LogLevel::Warn,
                "history",
                format!(
                    "`{}` wasn't interrupted; load it into Generate instead",
                    r.name
                ),
            );
            return;
        }
        if r.remaining() == Some(0) && r.in_flight() == 0 {
            ctx.log(
                LogLevel::Info,
                "history",
                format!("`{}` has no seeds left to run", r.name),
            );
            return;
        }
        let Some(spec) = r.resume_spec() else { return };
        let origin = Origin {
            regenerate_of: r.regenerate_of.clone(),
            resumed_from: Some(id),
        };
        Self::start_run(ctx, spec, origin).await;
        Self::send_history(ctx).await;
    }

    async fn send_projects(ctx: &Arc<Ctx>) {
        let p = ctx.projects.clone();
        if let Ok(list) = tokio::task::spawn_blocking(move || p.summaries()).await {
            ctx.ev.send(Event::Projects(list));
        }
    }

    async fn playback_ticker(ctx: Arc<Ctx>) {
        let mut last = (PlayState::Stopped, None::<SongId>);
        let mut iv = tokio::time::interval(Duration::from_millis(100));
        loop {
            iv.tick().await;
            let state = ctx.playback.state();
            let song = ctx.playback.song();
            if state == PlayState::Playing {
                ctx.ev
                    .send(Event::PlaybackPosition(ctx.playback.position()));
            }
            if (state, song.clone()) != last && state != PlayState::Loading {
                ctx.ev.send(Event::PlaybackState {
                    state,
                    song: song.clone(),
                    duration: ctx.playback.duration(),
                });
                if state != PlayState::Playing {
                    ctx.ev
                        .send(Event::PlaybackPosition(ctx.playback.position()));
                }
            }
            last = (state, song);
        }
    }

    /// Picks up songs moved or deleted outside the app (§7.1).
    fn spawn_watcher(ctx: Arc<Ctx>) -> Option<notify::RecommendedWatcher> {
        use notify::{RecursiveMode, Watcher};
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let root = ctx.cfg.library.root.clone();
        let skip = [
            root.join(crate::library::CACHE),
            root.join(crate::library::INPUTS),
            root.join(crate::library::EXPORTS),
        ];
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let relevant = ev.paths.iter().any(|p| {
                    !skip.iter().any(|s| p.starts_with(s))
                        && !p.components().any(|c| {
                            c.as_os_str()
                                .to_string_lossy()
                                .ends_with(crate::library::PART_SUFFIX)
                        })
                        && !p
                            .file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                });
                if relevant && !matches!(ev.kind, notify::EventKind::Access(_)) {
                    let _ = tx.send(());
                }
            }
        })
        .ok()?;
        for d in [crate::library::UNREVIEWED, crate::library::REVIEWED] {
            if let Err(e) = watcher.watch(&root.join(d), RecursiveMode::Recursive) {
                ctx.log(LogLevel::Warn, "library", format!("file watcher: {e}"));
            }
        }
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // debounce bursts
                tokio::time::sleep(Duration::from_millis(400)).await;
                while rx.try_recv().is_ok() {}
                let lib = ctx.library.clone();
                if let Ok(Ok(d)) = tokio::task::spawn_blocking(move || lib.lock().scan()).await {
                    ctx.send_library(d);
                }
            }
        });
        Some(watcher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(wall: u64, dur: Option<u64>, rtf: Option<f64>) -> Timing {
        Timing {
            wall_ms: wall,
            audio_duration_ms: dur,
            rtf,
        }
    }

    #[test]
    fn eta_estimates() {
        let a = ServerId(0);
        let b = ServerId(1);
        assert_eq!(estimate_ms(&[], a), None);
        let h = vec![(b, t(100_000, None, None)), (b, t(300_000, None, None))];
        assert_eq!(
            estimate_ms(&h, a),
            Some(300_000),
            "median wall_ms before rtf history"
        );
        let h = vec![
            (b, t(150_000, Some(300_000), Some(0.5))),
            (a, t(200_000, Some(200_000), Some(1.0))),
        ];
        // a's rtf 1.0 × median length (300 s of [200, 300])
        assert_eq!(estimate_ms(&h, a), Some(300_000));
        assert_eq!(estimate_ms(&h, b), Some(150_000));
    }

    #[test]
    fn server_state_labels() {
        assert!(ServerState::Down("x".into()).can_launch());
        assert!(
            !ServerState::Stopping {
                since: SystemTime::now()
            }
            .can_launch()
        );
        assert!(
            ServerState::Busy {
                job: None,
                since: SystemTime::now()
            }
            .is_up()
        );
    }

    #[test]
    fn output_lines_are_cleaned() {
        assert_eq!(
            clean_output_line("\u{1b}[1;31merror\u{1b}[0m: x"),
            "error: x"
        );
        assert_eq!(clean_output_line("\u{1b}]0;title\u{7}hello"), "hello");
        assert_eq!(clean_output_line("\u{1b}]0;title\u{1b}\\hello"), "hello");
        assert_eq!(clean_output_line("10%\r50%\r100%\r"), "100%");
        assert_eq!(
            output_level("ggml_vulkan: Failed to allocate"),
            LogLevel::Error
        );
        assert_eq!(output_level("WARNING: slow"), LogLevel::Warn);
        assert_eq!(output_level("listening on 9123"), LogLevel::Info);
    }

    #[test]
    fn log_tail_reads_whole_lines_and_follows_truncation() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("gpu1.log");
        let mut t = LogTail::default();
        assert!(t.read(&p).is_empty(), "no file yet");
        let mut f = std::fs::File::create(&p).unwrap();
        write!(f, "one\ntw").unwrap();
        assert_eq!(t.read(&p), vec!["one"]);
        write!(f, "o\n\nthree").unwrap();
        assert_eq!(t.read(&p), vec!["two"]);
        assert_eq!(t.flush(), vec!["three"]);
        // relaunch: the file starts over
        std::fs::write(&p, "new\n").unwrap();
        assert_eq!(t.read(&p), vec!["new"]);
        // attaching mid-file skips the cut-off first line
        let mut t = LogTail {
            pos: 2,
            skip_first: true,
            ..Default::default()
        };
        std::fs::write(&p, "abcd\nefgh\n").unwrap();
        assert_eq!(t.read(&p), vec!["efgh"]);
    }
}
