//! GUI state machine / view-model (design §2.1, §2.3). No egui: `AppState` plus a pure
//! [`update`] that turns a [`UiAction`] or core [`Event`] into [`Command`]s.

pub mod form;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use audiocpp_core::library::{Location, Song};
use audiocpp_core::media::{ExportFormat, Rating};
use audiocpp_core::params::{GenerationParams, Preset};
use audiocpp_core::playback::PlayState;
use audiocpp_core::project::ProjectSummary;
use audiocpp_core::run::{
    AbcSource, Collision, JobId, RunEdit, RunId, RunStatus, check_collision, next_free_seed,
};
use audiocpp_core::scheduler::{JobInfo, JobState, RunSnapshot, ServerId};
use audiocpp_core::service::{Command, Event, InitInfo, LogLine, ServerInfo, ServerState};
use audiocpp_core::{LibraryCommand, SongId};

pub use form::{AbcChoice, Blocker, GenerateForm};

pub const MAX_LOG: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Generate,
    Queue,
    Library,
    Review,
    Log,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerView {
    pub info: ServerInfo,
    pub state: ServerState,
}

/// Inline editor for an active run in the Queue panel (§5.1.2).
#[derive(Clone, Debug, PartialEq)]
pub struct RunEditor {
    pub params: GenerationParams,
    pub abc_text: String,
    pub count: String,
    pub until_stopped: bool,
    pub next_seed: String,
    pub open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FolderFilter {
    #[default]
    All,
    Unreviewed,
    Rated(Rating),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct LibraryFilter {
    pub folder: FolderFilter,
    pub tag: String,
    pub text: String,
    pub run_name: String,
    pub seed_min: String,
    pub seed_max: String,
    pub revision: String,
    pub only_no_abc: bool,
}

/// Drafts for the selected song's metadata editor.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MetaEditor {
    pub title: String,
    pub rename: String,
    pub notes: String,
    pub new_tag: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PlayerView {
    pub state: Option<PlayState>,
    pub song: Option<SongId>,
    pub position: Duration,
    pub duration: Duration,
    pub peaks: Option<(SongId, Vec<f32>)>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReviewState {
    /// Unreviewed songs in creation order, captured on entry and extended as songs arrive.
    pub queue: Vec<SongId>,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tag,
    Rename,
    Name,
    Abc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    /// "No ABC melody: YuE2 will compose its own. Continue?" (§5.2)
    NoAbc {
        dont_ask: bool,
    },
    /// Stopping a busy server (§4.2).
    StopBusy {
        server: ServerId,
        minutes: Option<u64>,
    },
    /// A new reference for a project that already has one (§5.2.1).
    ReplaceReference {
        audio: PathBuf,
        force: bool,
    },
    ConfirmDelete(Vec<SongId>),
    Exit,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TranscribeView {
    pub project: Option<String>,
    pub error: Option<String>,
    pub reused: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AppState {
    pub init: Option<InitInfo>,
    pub servers: Vec<ServerView>,
    pub form: GenerateForm,
    pub runs: Vec<RunSnapshot>,
    pub jobs: BTreeMap<JobId, JobInfo>,
    pub editors: BTreeMap<RunId, RunEditor>,
    pub library: BTreeMap<SongId, Song>,
    pub filter: LibraryFilter,
    pub selected: BTreeSet<SongId>,
    /// The song shown in the detail view (last clicked).
    pub current: Option<SongId>,
    pub meta: MetaEditor,
    pub player: PlayerView,
    pub review: ReviewState,
    pub projects: Vec<ProjectSummary>,
    pub transcribe: TranscribeView,
    pub log: VecDeque<LogLine>,
    pub dialog: Option<Dialog>,
    pub tab: Tab,
    pub focus: Option<Focus>,
    /// Transient message shown in the status bar.
    pub status: Option<String>,
    pub export_format: Option<ExportFormat>,
    pub export_strip: bool,
    /// Set once the user confirmed exit; the view closes the window.
    pub quit: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewKey {
    PlayPause,
    Back { big: bool },
    Forward { big: bool },
    Rate(Rating),
    Next,
    Prev,
    Tag,
    Rename,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    SelectTab(Tab),
    // --- Generate form
    SetName(String),
    SetSeed(String),
    /// 🎲: the view passes a random value so `update` stays pure.
    RandomSeed(u32),
    SetCount(String),
    SetUntilStopped(bool),
    SetParams(GenerationParams),
    SetAbcText(String),
    SetAbcChoice(AbcChoice),
    AbcFileLoaded {
        file_name: String,
        contents: String,
    },
    TranscribeFile(PathBuf),
    Retranscribe,
    ContinueFromSeed(u32),
    LoadProjectInputs,
    SetAdvancedOpen(bool),
    SetAbcSectionOpen(bool),
    StartRun,
    LoadPreset(PathBuf),
    SavePreset(PathBuf),
    // --- Dialogs
    SetDontAsk(bool),
    ConfirmDialog,
    CancelDialog,
    // --- Servers
    LaunchServer(ServerId),
    StopServer(ServerId),
    RecheckServer(ServerId),
    RefreshServers,
    // --- Queue
    ToggleRunEditor(RunId),
    SetRunEditor(RunId, RunEditor),
    ApplyRunParams(RunId),
    ApplyRunCount(RunId),
    ApplyRunNextSeed(RunId),
    PauseRun(RunId),
    ResumeRun(RunId),
    StopRun(RunId),
    MoveRun(RunId, usize),
    CancelJob(JobId),
    ClearFinishedRuns,
    // --- Library
    SetFilter(LibraryFilter),
    SelectSong(SongId),
    ToggleSelect(SongId),
    SelectAllFiltered,
    ClearSelection,
    Play(SongId),
    TogglePlay,
    Seek(Duration),
    SeekBy(f64),
    StopPlayback,
    SetMeta(MetaEditor),
    CommitTitle,
    CommitRename,
    CommitNotes,
    AddTag(String),
    RemoveTag(String),
    Rate(Rating),
    Unreview,
    Delete,
    Undo,
    SetExportStrip(bool),
    Export {
        dest: PathBuf,
        format: ExportFormat,
        strip_metadata: bool,
    },
    ContinueRun(SongId),
    Regenerate(SongId),
    // --- Review
    EnterReview,
    ReviewKey(ReviewKey),
    FocusHandled,
    // --- App
    ClearLog,
    DismissStatus,
    RequestExit,
}

#[derive(Clone, Debug)]
pub enum Input {
    Ui(UiAction),
    Core(Event),
}

impl From<UiAction> for Input {
    fn from(a: UiAction) -> Self {
        Input::Ui(a)
    }
}

impl From<Event> for Input {
    fn from(e: Event) -> Self {
        Input::Core(e)
    }
}

// ---------------------------------------------------------------------------------------
// Derived views

impl AppState {
    pub fn song(&self, id: &SongId) -> Option<&Song> {
        self.library.get(id)
    }

    pub fn current_song(&self) -> Option<&Song> {
        self.current.as_ref().and_then(|id| self.library.get(id))
    }

    pub fn seeds_for(&self, name: &str) -> BTreeSet<u32> {
        audiocpp_core::library::seeds_for(self.library.values(), name.trim())
    }

    /// Seeds already used by the form's name.
    pub fn form_seeds(&self) -> BTreeSet<u32> {
        self.seeds_for(&self.form.name)
    }

    pub fn start_blocker(&self) -> Option<Blocker> {
        if self.init.is_none() {
            return Some(Blocker::NoServerModel);
        }
        self.form.blocker(&self.form_seeds())
    }

    pub fn project(&self, name: &str) -> Option<&ProjectSummary> {
        self.projects.iter().find(|p| p.name == name.trim())
    }

    /// "Load project inputs" is offered when the name matches an existing project.
    pub fn offers_project(&self) -> bool {
        self.form.name_result().is_ok() && self.project(&self.form.name).is_some()
    }

    /// Why Load .abc / Transcribe are disabled (they need a project to save into).
    pub fn inputs_blocker(&self) -> Option<&'static str> {
        self.form
            .name_result()
            .err()
            .map(|_| "Enter a name first. Inputs are saved to inputs/<name>/.")
    }

    pub fn filtered_songs(&self) -> Vec<&Song> {
        let f = &self.filter;
        let text = f.text.trim().to_lowercase();
        let tag = f.tag.trim().to_lowercase();
        let run = f.run_name.trim().to_lowercase();
        let smin = f.seed_min.trim().parse::<u32>().ok();
        let smax = f.seed_max.trim().parse::<u32>().ok();
        let rev = f.revision.trim().parse::<u32>().ok();
        let mut v: Vec<&Song> = self
            .library
            .values()
            .filter(|s| match f.folder {
                FolderFilter::All => true,
                FolderFilter::Unreviewed => s.location == Location::Unreviewed,
                FolderFilter::Rated(r) => s.location == Location::Reviewed(r),
            })
            .filter(|s| tag.is_empty() || s.meta.tags.iter().any(|t| t.to_lowercase() == tag))
            .filter(|s| {
                text.is_empty()
                    || s.title().to_lowercase().contains(&text)
                    || s.stem.to_lowercase().contains(&text)
                    || s.meta
                        .comment
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&text)
                    || s.meta.tags.iter().any(|t| t.to_lowercase().contains(&text))
            })
            .filter(|s| {
                run.is_empty()
                    || s.run_name()
                        .is_some_and(|n| n.to_lowercase().contains(&run))
            })
            .filter(|s| smin.is_none_or(|m| s.seed().is_some_and(|x| x >= m)))
            .filter(|s| smax.is_none_or(|m| s.seed().is_some_and(|x| x <= m)))
            .filter(|s| rev.is_none_or(|r| s.revision() == Some(r)))
            .filter(|s| !f.only_no_abc || s.has_abc() == Some(false))
            .collect();
        v.sort_by(|a, b| {
            b.created_at()
                .cmp(&a.created_at())
                .then(b.modified.cmp(&a.modified))
                .then(a.stem.cmp(&b.stem))
        });
        v
    }

    pub fn all_tags(&self) -> BTreeSet<String> {
        self.library
            .values()
            .flat_map(|s| s.meta.tags.iter().cloned())
            .collect()
    }

    /// Unreviewed songs, oldest first (review order, §7.3).
    pub fn unreviewed_in_order(&self) -> Vec<SongId> {
        let mut v: Vec<&Song> = self
            .library
            .values()
            .filter(|s| s.location == Location::Unreviewed)
            .collect();
        v.sort_by(|a, b| {
            a.created_at()
                .cmp(&b.created_at())
                .then(a.modified.cmp(&b.modified))
                .then(a.stem.cmp(&b.stem))
        });
        v.into_iter().map(|s| s.id.clone()).collect()
    }

    pub fn review_song(&self) -> Option<&SongId> {
        self.review.queue.get(self.review.index)
    }

    pub fn server_name(&self, id: ServerId) -> String {
        self.servers
            .get(id.0)
            .map(|s| s.info.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    pub fn jobs_of(&self, run: RunId) -> Vec<&JobInfo> {
        let mut v: Vec<&JobInfo> = self.jobs.values().filter(|j| j.run_id == run).collect();
        v.sort_by_key(|j| (j.seed, j.id));
        v
    }

    pub fn editor_collision(&self, run: RunId) -> Option<Collision> {
        let ed = self.editors.get(&run)?;
        let r = self.runs.iter().find(|r| r.state.id == run)?;
        let seed = form::parse_seed(&ed.next_seed).ok()?;
        if seed == r.state.next_seed {
            return None;
        }
        let remaining = r.state.spec.count.map(|c| c.saturating_sub(r.state.issued));
        check_collision(&self.seeds_for(r.state.spec.name.as_str()), seed, remaining)
    }

    /// Minutes until a busy server's job finishes, from its estimate and elapsed time.
    pub fn busy_minutes(&self, server: ServerId) -> Option<u64> {
        let job = match &self.servers.get(server.0)?.state {
            ServerState::Busy { job: Some(j), .. } => *j,
            _ => return None,
        };
        match &self.jobs.get(&job)?.state {
            JobState::Running {
                started,
                estimate_ms: Some(e),
                ..
            }
            | JobState::Cancelling {
                started,
                estimate_ms: Some(e),
                ..
            } => {
                let elapsed = started.elapsed().unwrap_or_default().as_millis() as u64;
                Some(e.saturating_sub(elapsed).div_ceil(60_000).max(1))
            }
            _ => None,
        }
    }

    fn model(&self) -> Option<audiocpp_core::config::ModelSpec> {
        self.init.as_ref().map(|i| i.models.yue2.clone())
    }

    fn refresh_meta_editor(&mut self) {
        self.meta = match self.current_song() {
            Some(s) => MetaEditor {
                title: s.title().to_string(),
                rename: s.run_name().unwrap_or_default(),
                notes: s.meta.comment.clone().unwrap_or_default(),
                new_tag: String::new(),
            },
            None => MetaEditor::default(),
        };
    }

    fn editor_for(r: &RunSnapshot) -> RunEditor {
        let mut params = r.state.spec.params.clone();
        let abc = params.abc.take().unwrap_or_default();
        RunEditor {
            params,
            abc_text: abc,
            count: r
                .state
                .spec
                .count
                .map(|c| c.to_string())
                .unwrap_or_else(|| "10".into()),
            until_stopped: r.state.spec.count.is_none(),
            next_seed: r.state.next_seed.to_string(),
            open: false,
        }
    }

    /// Songs an action applies to: the selection, or the current song.
    fn targets(&self) -> Vec<SongId> {
        if !self.selected.is_empty() {
            return self.selected.iter().cloned().collect();
        }
        self.current.iter().cloned().collect()
    }
}

// ---------------------------------------------------------------------------------------
// The reducer

pub fn update(state: &mut AppState, input: impl Into<Input>) -> Vec<Command> {
    match input.into() {
        Input::Ui(a) => on_action(state, a),
        Input::Core(e) => {
            on_event(state, e);
            vec![]
        }
    }
}

fn on_event(s: &mut AppState, e: Event) {
    match e {
        Event::Init(info) => {
            s.servers = info
                .servers
                .iter()
                .map(|i| ServerView {
                    info: i.clone(),
                    state: ServerState::Stopped,
                })
                .collect();
            if let Some(p) = &info.default_preset {
                apply_preset(&mut s.form, p, info.default_preset_path.clone());
            }
            s.init = Some(info);
        }
        Event::ServerStatus(id, st) => {
            if let Some(v) = s.servers.get_mut(id.0) {
                v.state = st;
            }
        }
        Event::TranscribeStarted(p) => {
            s.transcribe = TranscribeView {
                project: Some(p),
                error: None,
                reused: false,
            }
        }
        Event::TranscribeDone(r) => {
            s.transcribe.project = None;
            match r {
                Ok(score) => {
                    s.transcribe.reused = score.reused;
                    s.transcribe.error = None;
                    if score.project == s.form.name.trim() {
                        s.form.set_abc_text(score.abc);
                        s.form.abc_source = AbcSource::Transcribed(score.reference);
                        s.form.abc_choice = AbcChoice::Transcribe;
                    }
                }
                Err(e) => s.transcribe.error = Some(e),
            }
        }
        Event::RunUpdate(snap) => {
            let id = snap.state.id;
            if let std::collections::btree_map::Entry::Vacant(e) = s.editors.entry(id) {
                e.insert(AppState::editor_for(&snap));
            } else if let Some(ed) = s.editors.get_mut(&id)
                && !ed.open
            {
                let open = ed.open;
                *ed = AppState::editor_for(&snap);
                ed.open = open;
            }
            match s.runs.iter_mut().find(|r| r.state.id == id) {
                Some(r) => *r = snap,
                None => s.runs.push(snap),
            }
            s.runs.sort_by_key(|r| r.position);
        }
        Event::JobUpdate(id, info) => {
            s.jobs.insert(id, info);
        }
        Event::LibraryChanged(d) => {
            if d.full {
                s.library.clear();
            }
            for id in &d.removed {
                s.library.remove(id);
                s.selected.remove(id);
            }
            for song in d.upserted {
                s.library.insert(song.id.clone(), song);
            }
            if s.current
                .as_ref()
                .is_some_and(|c| !s.library.contains_key(c))
            {
                s.current = None;
            }
            s.selected.retain(|id| s.library.contains_key(id));
            // new songs join the end of an active review queue
            if s.tab == Tab::Review {
                for id in s.unreviewed_in_order() {
                    if !s.review.queue.contains(&id) {
                        s.review.queue.push(id);
                    }
                }
            }
            let editing = s.meta.clone();
            s.refresh_meta_editor();
            // keep drafts the user is typing if the song didn't change underneath
            if s.current_song()
                .is_some_and(|c| c.title() == editing.title || editing.title.is_empty())
            {
                s.meta.new_tag = editing.new_tag;
            }
        }
        Event::Projects(p) => s.projects = p,
        Event::ProjectInputs(r) => match r {
            Ok(inp) => {
                if inp.name == s.form.name.trim() {
                    if let Some(l) = inp.lyrics {
                        s.form.params.lyrics = l;
                    }
                    if let Some(st) = inp.style {
                        s.form.params.style = st;
                    }
                    match inp.abc {
                        Some(a) => {
                            s.form.set_abc_text(a);
                            s.form.abc_source = inp.abc_source.unwrap_or(AbcSource::Manual);
                            s.form.abc_choice = match s.form.abc_source {
                                AbcSource::File { .. } => AbcChoice::LoadFile,
                                AbcSource::Transcribed(_) => AbcChoice::Transcribe,
                                _ => AbcChoice::Paste,
                            };
                        }
                        None => {
                            s.form.set_abc_text(String::new());
                        }
                    }
                    // the collision check then offers the next free seed
                    if let Some(n) = next_free_seed(&s.form_seeds())
                        && (s.form.collision(&s.form_seeds()).is_some()
                            || s.form.seed.trim().is_empty())
                    {
                        s.form.seed = n.to_string();
                    }
                }
            }
            Err(e) => s.status = Some(format!("Loading project: {e}")),
        },
        Event::PresetLoaded(r) => match r {
            Ok((path, preset)) => {
                apply_preset(&mut s.form, &preset, Some(path.clone()));
                s.status = Some(format!("Loaded preset {}", path.display()));
            }
            Err(e) => s.status = Some(format!("Preset: {e}")),
        },
        Event::Recipe(_, _) => {}
        Event::PlaybackPosition(p) => s.player.position = p,
        Event::PlaybackState {
            state,
            song,
            duration,
        } => {
            s.player.state = Some(state);
            if song.is_some() || state == PlayState::Stopped {
                if s.player.song != song {
                    s.player.position = Duration::ZERO;
                }
                s.player.song = song;
            }
            if duration > Duration::ZERO || state == PlayState::Stopped {
                s.player.duration = duration;
            }
        }
        Event::Peaks(id, p) => s.player.peaks = Some((id, p)),
        Event::Log(l) => {
            s.log.push_back(l);
            while s.log.len() > MAX_LOG {
                s.log.pop_front();
            }
        }
    }
}

fn apply_preset(form: &mut GenerateForm, p: &Preset, path: Option<PathBuf>) {
    form.load_params(&p.params);
    form.allow_no_abc = p.allow_no_abc;
    form.preset_path = path;
    if p.params.abc.is_some() {
        form.abc_source = AbcSource::Manual;
        form.abc_choice = AbcChoice::Paste;
    }
}

fn on_action(s: &mut AppState, a: UiAction) -> Vec<Command> {
    use UiAction as A;
    match a {
        A::SelectTab(t) => {
            if t == Tab::Review && s.tab != Tab::Review {
                return on_action(s, A::EnterReview);
            }
            s.tab = t;
            vec![]
        }
        A::SetName(n) => {
            s.form.name = n;
            vec![]
        }
        A::SetSeed(v) => {
            s.form.seed = v;
            vec![]
        }
        A::RandomSeed(v) => {
            s.form.seed = v.to_string();
            vec![]
        }
        A::SetCount(c) => {
            s.form.count = c;
            vec![]
        }
        A::SetUntilStopped(b) => {
            s.form.until_stopped = b;
            vec![]
        }
        A::SetParams(p) => {
            s.form.params = GenerationParams { abc: None, ..p };
            vec![]
        }
        A::SetAbcText(t) => {
            s.form.set_abc_text(t);
            if matches!(s.form.abc_source, AbcSource::None) {
                s.form.abc_source = AbcSource::Manual;
            }
            vec![]
        }
        A::SetAbcChoice(c) => {
            s.form.abc_choice = c;
            if c == AbcChoice::Paste
                && !matches!(s.form.abc_source, AbcSource::Manual)
                && s.form.abc_text.is_empty()
            {
                s.form.abc_source = AbcSource::Manual;
            }
            vec![]
        }
        A::AbcFileLoaded {
            file_name,
            contents,
        } => {
            if s.inputs_blocker().is_some() {
                return vec![];
            }
            let sha256 = audiocpp_core::fsutil::sha256_hex(contents.as_bytes());
            s.form.set_abc_text(contents);
            s.form.abc_source = AbcSource::File { file_name, sha256 };
            s.form.abc_choice = AbcChoice::LoadFile;
            vec![]
        }
        A::TranscribeFile(audio) => transcribe(s, audio, false),
        A::Retranscribe => match s.form.name_result() {
            Ok(name) => vec![Command::Retranscribe(name)],
            Err(_) => vec![],
        },
        A::ContinueFromSeed(seed) => {
            s.form.seed = seed.to_string();
            vec![]
        }
        A::LoadProjectInputs => match s.form.name_result() {
            Ok(n) => vec![Command::LoadProject(n.to_string())],
            Err(_) => vec![],
        },
        A::SetAdvancedOpen(b) => {
            s.form.advanced_open = b;
            vec![]
        }
        A::SetAbcSectionOpen(b) => {
            s.form.abc_section_open = b;
            vec![]
        }
        A::StartRun => {
            if s.start_blocker().is_some() {
                return vec![];
            }
            if s.form.needs_no_abc_confirmation() {
                s.dialog = Some(Dialog::NoAbc { dont_ask: false });
                return vec![];
            }
            start_run(s)
        }
        A::LoadPreset(p) => vec![Command::LoadPreset(p)],
        A::SavePreset(path) => {
            let mut params = s.form.params.clone();
            params.abc = s.form.effective_abc();
            s.form.preset_path = Some(path.clone());
            vec![Command::SavePreset(
                path,
                Preset {
                    params,
                    allow_no_abc: s.form.allow_no_abc,
                    abc_file: None,
                },
            )]
        }
        A::SetDontAsk(b) => {
            if let Some(Dialog::NoAbc { dont_ask }) = &mut s.dialog {
                *dont_ask = b;
            }
            vec![]
        }
        A::ConfirmDialog => match s.dialog.take() {
            Some(Dialog::NoAbc { dont_ask }) => {
                let mut cmds = vec![];
                if dont_ask {
                    s.form.allow_no_abc = true;
                    if let Some(path) = s.form.preset_path.clone() {
                        let mut params = s.form.params.clone();
                        params.abc = None;
                        cmds.push(Command::SavePreset(
                            path,
                            Preset {
                                params,
                                allow_no_abc: true,
                                abc_file: None,
                            },
                        ));
                    }
                }
                cmds.extend(start_run(s));
                cmds
            }
            Some(Dialog::StopBusy { server, .. }) => vec![Command::StopServer(server)],
            Some(Dialog::ReplaceReference { audio, force }) => match s.form.name_result() {
                Ok(project) => vec![Command::Transcribe {
                    project,
                    audio,
                    force,
                }],
                Err(_) => vec![],
            },
            Some(Dialog::ConfirmDelete(ids)) => {
                s.selected.clear();
                ids.into_iter()
                    .map(|id| Command::Library(LibraryCommand::Delete(id)))
                    .collect()
            }
            Some(Dialog::Exit) => {
                s.quit = Some(true);
                vec![Command::Shutdown {
                    stop_launched: true,
                }]
            }
            None => vec![],
        },
        A::CancelDialog => {
            if s.dialog.take() == Some(Dialog::Exit) {
                // "leave them running" is the default answer, handled by RequestExit twice
            }
            vec![]
        }
        A::LaunchServer(id) => match s.servers.get(id.0) {
            Some(v) if v.state.can_launch() => vec![Command::LaunchServer(id)],
            _ => vec![],
        },
        A::StopServer(id) => match s.servers.get(id.0).map(|v| &v.state) {
            Some(ServerState::Busy { .. }) => {
                s.dialog = Some(Dialog::StopBusy {
                    server: id,
                    minutes: s.busy_minutes(id),
                });
                vec![]
            }
            Some(ServerState::Stopping { .. } | ServerState::Stopped) | None => vec![],
            Some(_) => vec![Command::StopServer(id)],
        },
        A::RecheckServer(id) => vec![Command::RecheckServer(id)],
        A::RefreshServers => vec![Command::RefreshServers],
        A::ToggleRunEditor(id) => {
            if let Some(e) = s.editors.get_mut(&id) {
                e.open = !e.open;
            }
            vec![]
        }
        A::SetRunEditor(id, ed) => {
            s.editors.insert(id, ed);
            vec![]
        }
        A::ApplyRunParams(id) => {
            let Some(ed) = s.editors.get(&id) else {
                return vec![];
            };
            let mut p = ed.params.clone();
            p.abc = (!ed.abc_text.trim().is_empty()).then(|| ed.abc_text.clone());
            if !p.validate().is_empty() {
                s.status = Some(format!("Not applied: {}", p.validate().join("; ")));
                return vec![];
            }
            vec![Command::EditRun(id, RunEdit::Params(p))]
        }
        A::ApplyRunCount(id) => {
            let Some(ed) = s.editors.get(&id) else {
                return vec![];
            };
            if ed.until_stopped {
                return vec![Command::EditRun(id, RunEdit::Count(None))];
            }
            match ed.count.trim().parse::<u32>() {
                Ok(n) if n > 0 => vec![Command::EditRun(id, RunEdit::Count(Some(n)))],
                _ => {
                    s.status = Some("Count must be a whole number of at least 1".into());
                    vec![]
                }
            }
        }
        A::ApplyRunNextSeed(id) => {
            let Some(ed) = s.editors.get(&id) else {
                return vec![];
            };
            match form::parse_seed(&ed.next_seed) {
                Ok(seed) => {
                    if s.editor_collision(id).is_some() {
                        s.status = Some("That seed range collides with existing songs".into());
                        return vec![];
                    }
                    vec![Command::EditRun(id, RunEdit::NextSeed(seed))]
                }
                Err(e) => {
                    s.status = Some(format!("Next seed: {e}"));
                    vec![]
                }
            }
        }
        A::PauseRun(id) => vec![Command::PauseRun(id)],
        A::ResumeRun(id) => vec![Command::ResumeRun(id)],
        A::StopRun(id) => vec![Command::StopRun(id)],
        A::MoveRun(id, to) => vec![Command::MoveRun(id, to)],
        A::CancelJob(id) => vec![Command::CancelJob(id)],
        A::ClearFinishedRuns => {
            let done: Vec<RunId> = s
                .runs
                .iter()
                .filter(|r| r.state.status == RunStatus::Done)
                .map(|r| r.state.id)
                .collect();
            s.runs.retain(|r| r.state.status != RunStatus::Done);
            s.jobs.retain(|_, j| !done.contains(&j.run_id));
            for d in done {
                s.editors.remove(&d);
            }
            vec![Command::ClearFinishedRuns]
        }
        A::SetFilter(f) => {
            s.filter = f;
            vec![]
        }
        A::SelectSong(id) => {
            s.current = Some(id);
            s.selected.clear();
            s.refresh_meta_editor();
            vec![]
        }
        A::ToggleSelect(id) => {
            if !s.selected.remove(&id) {
                s.selected.insert(id.clone());
            }
            s.current = Some(id);
            s.refresh_meta_editor();
            vec![]
        }
        A::SelectAllFiltered => {
            s.selected = s
                .filtered_songs()
                .into_iter()
                .map(|x| x.id.clone())
                .collect();
            vec![]
        }
        A::ClearSelection => {
            s.selected.clear();
            vec![]
        }
        A::Play(id) => {
            s.current = Some(id.clone());
            s.refresh_meta_editor();
            vec![Command::Play(id)]
        }
        A::TogglePlay => match (&s.player.state, &s.current) {
            (Some(PlayState::Playing | PlayState::Paused), _)
                if s.player.song == s.current || s.current.is_none() =>
            {
                vec![Command::TogglePause]
            }
            (_, Some(id)) => vec![Command::Play(id.clone())],
            _ => vec![],
        },
        A::Seek(d) => {
            s.player.position = d;
            vec![Command::Seek(d)]
        }
        A::SeekBy(d) => vec![Command::SeekBy(d)],
        A::StopPlayback => vec![Command::Stop],
        A::SetMeta(m) => {
            s.meta = m;
            vec![]
        }
        A::CommitTitle => match s.current_song() {
            Some(song) if song.title() != s.meta.title.trim() => {
                vec![Command::Library(LibraryCommand::SetTitle(
                    song.id.clone(),
                    s.meta.title.clone(),
                ))]
            }
            _ => vec![],
        },
        A::CommitRename => {
            let Some(song) = s.current_song() else {
                return vec![];
            };
            if song.run_name().as_deref() == Some(s.meta.rename.trim()) {
                return vec![];
            }
            if let Err(e) = audiocpp_core::run::RunName::parse(&s.meta.rename) {
                s.status = Some(format!("Rename: {e}"));
                return vec![];
            }
            vec![Command::Library(LibraryCommand::Rename(
                song.id.clone(),
                s.meta.rename.trim().to_string(),
            ))]
        }
        A::CommitNotes => match s.current_song() {
            Some(song) if song.meta.comment.as_deref().unwrap_or("") != s.meta.notes => {
                vec![Command::Library(LibraryCommand::SetNotes(
                    song.id.clone(),
                    s.meta.notes.clone(),
                ))]
            }
            _ => vec![],
        },
        A::AddTag(tag) => {
            let tag = tag.trim().to_string();
            s.meta.new_tag.clear();
            if tag.is_empty() {
                return vec![];
            }
            s.targets()
                .into_iter()
                .filter_map(|id| {
                    let song = s.library.get(&id)?;
                    if song.meta.tags.contains(&tag) {
                        return None;
                    }
                    let mut tags = song.meta.tags.clone();
                    tags.push(tag.clone());
                    Some(Command::Library(LibraryCommand::SetTags(id, tags)))
                })
                .collect()
        }
        A::RemoveTag(tag) => s
            .targets()
            .into_iter()
            .filter_map(|id| {
                let song = s.library.get(&id)?;
                if !song.meta.tags.contains(&tag) {
                    return None;
                }
                let tags = song
                    .meta
                    .tags
                    .iter()
                    .filter(|t| **t != tag)
                    .cloned()
                    .collect();
                Some(Command::Library(LibraryCommand::SetTags(id, tags)))
            })
            .collect(),
        A::Rate(r) => s
            .targets()
            .into_iter()
            .map(|id| Command::Library(LibraryCommand::Rate(id, r)))
            .collect(),
        A::Unreview => s
            .targets()
            .into_iter()
            .map(|id| Command::Library(LibraryCommand::Unreview(id)))
            .collect(),
        A::Delete => {
            let t = s.targets();
            if !t.is_empty() {
                s.dialog = Some(Dialog::ConfirmDelete(t));
            }
            vec![]
        }
        A::Undo => vec![Command::Library(LibraryCommand::Undo)],
        A::SetExportStrip(b) => {
            s.export_strip = b;
            vec![]
        }
        A::Export {
            dest,
            format,
            strip_metadata,
        } => {
            let ids = s.targets();
            if ids.is_empty() {
                return vec![];
            }
            s.export_format = Some(format);
            s.export_strip = strip_metadata;
            vec![Command::Library(LibraryCommand::Export {
                ids,
                dest,
                format,
                strip_metadata,
            })]
        }
        A::ContinueRun(id) => {
            continue_run(s, &id);
            vec![]
        }
        A::Regenerate(id) => vec![Command::Regenerate(id)],
        A::EnterReview => {
            s.tab = Tab::Review;
            s.review = ReviewState {
                queue: s.unreviewed_in_order(),
                index: 0,
            };
            review_play_current(s)
        }
        A::ReviewKey(k) => review_key(s, k),
        A::FocusHandled => {
            s.focus = None;
            vec![]
        }
        A::ClearLog => {
            s.log.clear();
            vec![]
        }
        A::DismissStatus => {
            s.status = None;
            vec![]
        }
        A::RequestExit => {
            let launched = s
                .servers
                .iter()
                .any(|v| v.info.launchable && !matches!(v.state, ServerState::Stopped));
            if launched && s.dialog != Some(Dialog::Exit) {
                s.dialog = Some(Dialog::Exit);
                vec![]
            } else {
                // default: leave servers running
                s.dialog = None;
                s.quit = Some(false);
                vec![Command::Shutdown {
                    stop_launched: false,
                }]
            }
        }
    }
}

fn transcribe(s: &mut AppState, audio: PathBuf, force: bool) -> Vec<Command> {
    let Ok(project) = s.form.name_result() else {
        return vec![];
    };
    if s.project(project.as_str()).is_some_and(|p| p.has_reference) {
        s.dialog = Some(Dialog::ReplaceReference { audio, force });
        return vec![];
    }
    vec![Command::Transcribe {
        project,
        audio,
        force,
    }]
}

fn start_run(s: &mut AppState) -> Vec<Command> {
    let Some(model) = s.model() else {
        return vec![];
    };
    match s.form.build_spec(model) {
        Ok(spec) => {
            s.status = Some(format!(
                "Started run “{}” from seed {}",
                spec.name, spec.start_seed
            ));
            // every new run needs a name typed on purpose
            s.form.name.clear();
            vec![Command::StartRun(spec)]
        }
        Err(e) => {
            s.status = Some(e);
            vec![]
        }
    }
}

/// Fills the form with a song's recipe, name and the next unused seed (§5.1.1).
fn continue_run(s: &mut AppState, id: &SongId) {
    let Some(song) = s.library.get(id) else {
        return;
    };
    let Some(recipe) = song.meta.recipe.clone() else {
        s.status = Some(format!("{} has no recipe", song.stem));
        return;
    };
    let req = recipe.request.get("request").cloned().unwrap_or_default();
    match GenerationParams::from_request(&req) {
        Ok((params, _)) => {
            s.form.name = recipe.run.name.clone();
            s.form.load_params(&params);
            s.form.abc_source = recipe.abc_source.clone();
            s.form.abc_choice = match recipe.abc_source {
                AbcSource::None => AbcChoice::None,
                AbcSource::File { .. } => AbcChoice::LoadFile,
                AbcSource::Transcribed(_) => AbcChoice::Transcribe,
                AbcSource::Manual => AbcChoice::Paste,
            };
            let seeds = s.seeds_for(&recipe.run.name);
            s.form.seed = next_free_seed(&seeds)
                .unwrap_or(recipe.run.seed.saturating_add(1))
                .to_string();
            s.tab = Tab::Generate;
            s.focus = Some(Focus::Name);
        }
        Err(e) => s.status = Some(format!("Recipe: {e}")),
    }
}

fn review_play_current(s: &mut AppState) -> Vec<Command> {
    match s.review_song().cloned() {
        Some(id) => {
            s.current = Some(id.clone());
            s.refresh_meta_editor();
            vec![Command::Play(id)]
        }
        None => {
            s.current = None;
            vec![Command::Stop]
        }
    }
}

fn review_key(s: &mut AppState, k: ReviewKey) -> Vec<Command> {
    match k {
        ReviewKey::PlayPause => vec![Command::TogglePause],
        ReviewKey::Back { big } => vec![Command::SeekBy(if big { -30.0 } else { -5.0 })],
        ReviewKey::Forward { big } => vec![Command::SeekBy(if big { 30.0 } else { 5.0 })],
        ReviewKey::Rate(r) => {
            let Some(id) = s.review_song().cloned() else {
                return vec![];
            };
            let mut cmds = vec![Command::Library(LibraryCommand::Rate(id, r))];
            s.review.index += 1;
            cmds.extend(review_play_current(s));
            cmds
        }
        ReviewKey::Next => {
            if s.review.index < s.review.queue.len() {
                s.review.index += 1;
            }
            review_play_current(s)
        }
        ReviewKey::Prev => {
            s.review.index = s.review.index.saturating_sub(1);
            review_play_current(s)
        }
        ReviewKey::Tag => {
            s.focus = Some(Focus::Tag);
            vec![]
        }
        ReviewKey::Rename => {
            s.focus = Some(Focus::Rename);
            vec![]
        }
    }
}

/// A short, human label for a job state (Queue panel).
pub fn job_label(state: &JobState, now: SystemTime) -> String {
    fn mins(ms: u64) -> String {
        let m = ms.div_ceil(60_000);
        format!("~{m} min")
    }
    let elapsed = |t: &SystemTime| now.duration_since(*t).unwrap_or_default();
    match state {
        JobState::Queued => "queued".into(),
        JobState::Running {
            started,
            estimate_ms,
            ..
        } => {
            let e = elapsed(started);
            match estimate_ms {
                Some(est) => format!(
                    "running {} (ETA {})",
                    fmt_duration(e),
                    mins(est.saturating_sub(e.as_millis() as u64))
                ),
                None => format!("running {}", fmt_duration(e)),
            }
        }
        JobState::Cancelling {
            started,
            estimate_ms,
            ..
        } => {
            let e = elapsed(started);
            match estimate_ms {
                Some(est) => format!(
                    "Cancelling — finishes in {}",
                    mins(est.saturating_sub(e.as_millis() as u64))
                ),
                None => "Cancelling — waits for the server to finish".into(),
            }
        }
        JobState::Encoding => "encoding".into(),
        JobState::Done(_) => "done".into(),
        JobState::Failed(r) => format!("failed: {r}"),
        JobState::Cancelled => "cancelled".into(),
    }
}

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests;
