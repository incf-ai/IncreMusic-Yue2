//! GUI state machine / view-model (design §2.1, §2.3). No egui: `AppState` plus a pure
//! [`update`] that turns a [`UiAction`] or core [`Event`] into [`Command`]s.

pub mod form;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use incremusic_core::config::ModelSpec;
use incremusic_core::history::{RecordStatus, RunRecord, RunSummary};
use incremusic_core::library::{Location, Song};
use incremusic_core::media::{ExportFormat, Rating};
use incremusic_core::params::{GenerationParams, Preset};
use incremusic_core::playback::PlayState;
use incremusic_core::project::{Keypoint, Keypoints, ProjectSummary, reference_play_id};
use incremusic_core::run::{
    AbcSource, Collision, JobId, RunEdit, RunId, RunStatus, check_collision, next_free_seed,
};
use incremusic_core::scheduler::{JobInfo, JobState, RunSnapshot, ServerId};
use incremusic_core::service::{
    Command, Event, InitInfo, LogLevel, LogLine, ServerInfo, ServerState,
};
use incremusic_core::{LibraryCommand, SongId};

pub use form::{AbcChoice, Blocker, GenerateForm};

pub const MAX_LOG: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Generate,
    Queue,
    History,
    Projects,
    Inputs,
    Library,
    Review,
    Log,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerView {
    pub info: ServerInfo,
    pub state: ServerState,
    /// Model paths this server reported missing (§1.1), from its last check.
    pub path_problems: Vec<String>,
    /// The app's messages about this server and the server's own output.
    pub log: VecDeque<LogLine>,
}

/// Where a log line comes from, as the Log tab's toggles group them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LogSource {
    /// The app's own messages that aren't about a server (`core`, `library`, …).
    App,
    Server(ServerId),
}

/// Which sources the Log tab shows. It records the hidden ones, so everything (including a
/// server added later) is shown by default.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct LogFilter {
    pub hidden: BTreeSet<LogSource>,
}

impl LogFilter {
    pub fn shows(&self, src: LogSource) -> bool {
        !self.hidden.contains(&src)
    }

    pub fn shows_all(&self) -> bool {
        self.hidden.is_empty()
    }
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
    /// The song the drafts belong to.
    pub song: Option<SongId>,
    /// The song's notes as the library last reported them. A library update that
    /// changes them (an undo, say) replaces the draft; any other keeps it.
    pub notes_loaded: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PlayerView {
    pub state: Option<PlayState>,
    pub song: Option<SongId>,
    pub position: Duration,
    pub duration: Duration,
    pub peaks: Option<(SongId, Vec<f32>)>,
    /// The tab playback was started from, so the global transport can name it.
    pub tab: Option<Tab>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReviewState {
    /// Songs still to review, in creation order: captured on entry, extended as songs
    /// arrive, and shrunk as they're rated.
    pub queue: Vec<SongId>,
    pub index: usize,
    /// Songs rated here whose library update hasn't arrived yet, so they still look
    /// unreviewed and mustn't rejoin the queue.
    pub rating: BTreeSet<SongId>,
    /// The lyrics popup is open. It follows the review song and, unlike a [`Dialog`],
    /// leaves the review keys working so the song can be played and rated while reading.
    pub lyrics: bool,
    /// The keypoint last jumped to in the review song, highlighted in the pane; `None`
    /// before the first. Starts over with each song.
    pub keypoint: Option<usize>,
    /// The song [`Self::keypoint`] belongs to.
    pub keypoint_song: Option<SongId>,
    /// A keypoint's start, waiting for the review song to load before seeking there.
    pub pending_seek: Option<Duration>,
    /// A song arriving after the review ran out becomes the review song without starting;
    /// `Space` plays it. Off by default, so it plays at once.
    pub no_autoplay: bool,
    /// How long each song has actually played during this review, seeks not counted:
    /// [`ReviewKey::RateBadChecked`] asks before rating a song heard less than
    /// [`MIN_LISTEN`].
    pub listened: BTreeMap<SongId, Duration>,
}

/// Listening time below which [`ReviewKey::RateBadChecked`] asks for confirmation.
pub const MIN_LISTEN: Duration = Duration::from_secs(30);

/// The review pane's keypoints (§7.3.1): the review song's project's, edited in place and
/// saved when an edit finishes (a drag or typing ends, a button is pressed).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct KeypointEditor {
    /// `None` when the review song has no project to keep keypoints in.
    pub project: Option<String>,
    pub keypoints: Keypoints,
    /// As last saved or loaded. A project list that reports something else (the file was
    /// edited elsewhere) replaces the edits.
    pub loaded: Keypoints,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tag,
    Rename,
    Name,
    Abc,
    /// The ABC section's main button (*Load .abc…*).
    AbcSection,
}

/// See [`AppState::drop_target`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropKind {
    Abc,
    Audio,
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
    /// Rating the review song bad with `C` before it has played [`MIN_LISTEN`].
    ShortListen {
        song: SongId,
        listened: Duration,
    },
    /// Audio picked for transcription with no project name yet: asks for one, suggesting
    /// the file's base name.
    NameProject {
        audio: PathBuf,
        name: String,
    },
    /// Moving a project folder to the trash.
    DeleteProject(String),
    /// Loading a run into Generate would replace a form that has a name typed in.
    ReplaceForm(FormLoad),
    /// Clearing the Generate form back to its startup state.
    ClearForm,
    Exit,
}

/// What to fill the Generate form from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormLoad {
    /// A run in the live queue.
    Queue(RunId),
    /// The History tab's selected record, at a revision (`None` → the latest).
    Record(Option<u32>),
}

/// The History tab: every recorded run, and the selected one's full record.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct HistoryView {
    pub runs: Vec<RunSummary>,
    pub selected: Option<RunId>,
    pub record: Option<Result<RunRecord, String>>,
    pub search: String,
    pub sort: Sorting<HistorySort>,
}

impl HistoryView {
    /// Rows matching the search, unsorted; `AppState::history_rows` sorts them.
    pub fn filtered(&self) -> Vec<&RunSummary> {
        let q = self.search.trim().to_lowercase();
        self.runs
            .iter()
            .filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q))
            .collect()
    }

    /// The selected record, once loaded.
    pub fn record(&self) -> Option<&RunRecord> {
        match &self.record {
            Some(Ok(r)) if Some(r.id) == self.selected => Some(r),
            _ => None,
        }
    }
}

/// The Projects panel: the selected project and the rename draft.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProjectsView {
    pub current: Option<String>,
    pub rename: String,
    pub search: String,
    pub sort: Sorting<ProjectSort>,
    /// A rename sent to the core; the selection follows once the new name is listed.
    pub renaming: Option<String>,
}

/// A table column that can be sorted by.
pub trait SortColumn: Copy + PartialEq + 'static {
    const ALL: &'static [Self];
    fn label(self) -> &'static str;
    /// Whether the unreversed order is ascending: names run A→Z, while counts, sizes and
    /// dates go largest or newest first.
    fn ascending(self) -> bool;
}

/// Which column a table is sorted by, and whether its natural order is reversed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sorting<K> {
    pub by: K,
    pub reversed: bool,
}

impl<K: SortColumn> Sorting<K> {
    /// Sorts by `by`; the column already sorted by reverses instead.
    pub fn toggle(&mut self, by: K) {
        self.reversed = self.by == by && !self.reversed;
        self.by = by;
    }

    /// Whether the rows currently run in ascending order.
    pub fn ascending(&self) -> bool {
        self.by.ascending() != self.reversed
    }

    /// Applies the reversal to rows already in `by`'s natural order.
    pub fn finish<T>(&self, mut v: Vec<T>) -> Vec<T> {
        if self.reversed {
            v.reverse();
        }
        v
    }
}

/// The Projects panel's columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProjectSort {
    Name,
    Songs,
    Reference,
    Created,
    #[default]
    Modified,
}

impl SortColumn for ProjectSort {
    const ALL: &'static [Self] = &[
        ProjectSort::Name,
        ProjectSort::Songs,
        ProjectSort::Reference,
        ProjectSort::Created,
        ProjectSort::Modified,
    ];

    fn label(self) -> &'static str {
        match self {
            ProjectSort::Name => "Name",
            ProjectSort::Songs => "Songs",
            ProjectSort::Reference => "Reference",
            ProjectSort::Created => "Created",
            ProjectSort::Modified => "Modified",
        }
    }

    fn ascending(self) -> bool {
        matches!(self, ProjectSort::Name | ProjectSort::Reference)
    }
}

/// The History tab's columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HistorySort {
    Name,
    #[default]
    Created,
    Status,
    Seeds,
    Model,
}

impl SortColumn for HistorySort {
    const ALL: &'static [Self] = &[
        HistorySort::Name,
        HistorySort::Created,
        HistorySort::Status,
        HistorySort::Seeds,
        HistorySort::Model,
    ];

    fn label(self) -> &'static str {
        match self {
            HistorySort::Name => "Name",
            HistorySort::Created => "Created",
            HistorySort::Status => "Status",
            HistorySort::Seeds => "Seeds",
            HistorySort::Model => "Model",
        }
    }

    fn ascending(self) -> bool {
        self != HistorySort::Created
    }
}

/// The Inputs panel's columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReferenceSort {
    File,
    Format,
    Size,
    Projects,
    Transcription,
    #[default]
    Created,
}

impl SortColumn for ReferenceSort {
    const ALL: &'static [Self] = &[
        ReferenceSort::File,
        ReferenceSort::Format,
        ReferenceSort::Size,
        ReferenceSort::Projects,
        ReferenceSort::Transcription,
        ReferenceSort::Created,
    ];

    fn label(self) -> &'static str {
        match self {
            ReferenceSort::File => "File",
            ReferenceSort::Format => "Format",
            ReferenceSort::Size => "Size",
            ReferenceSort::Projects => "Projects",
            ReferenceSort::Transcription => "Transcription",
            ReferenceSort::Created => "Created",
        }
    }

    fn ascending(self) -> bool {
        matches!(
            self,
            ReferenceSort::File | ReferenceSort::Format | ReferenceSort::Projects
        )
    }
}

/// The Audio Library's columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SongSort {
    Title,
    Run,
    Seed,
    Revision,
    Rating,
    #[default]
    Created,
    Tags,
}

impl SortColumn for SongSort {
    const ALL: &'static [Self] = &[
        SongSort::Title,
        SongSort::Run,
        SongSort::Seed,
        SongSort::Revision,
        SongSort::Rating,
        SongSort::Created,
        SongSort::Tags,
    ];

    fn label(self) -> &'static str {
        match self {
            SongSort::Title => "Title",
            SongSort::Run => "Run",
            SongSort::Seed => "Seed",
            SongSort::Revision => "Revision",
            SongSort::Rating => "Rating",
            SongSort::Created => "Created",
            SongSort::Tags => "Tags",
        }
    }

    /// The library reads newest first; every other column runs ascending.
    fn ascending(self) -> bool {
        self != SongSort::Created
    }
}

/// One reference audio in the Inputs panel. Projects holding the same file (by SHA-256)
/// share a row.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceRow<'a> {
    /// The first project holding the file; it is played and re-transcribed from there.
    pub project: &'a ProjectSummary,
    pub reference: &'a incremusic_core::project::Reference,
    pub projects: Vec<&'a ProjectSummary>,
    /// A project that holds a transcription of this audio.
    pub transcribed: Option<&'a ProjectSummary>,
}

impl ReferenceRow<'_> {
    /// When the audio first came into a project (RFC 3339).
    pub fn created_at(&self) -> &str {
        self.projects
            .iter()
            .filter_map(|p| p.reference.as_ref())
            .map(|r| r.added_at.as_str())
            .min()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TranscribeView {
    pub project: Option<String>,
    pub error: Option<String>,
    pub reused: bool,
}

/// See [`AppState::song_rate`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SongRate {
    /// Average generation time of one song on one server.
    pub per_server: f64,
    /// Generating time across all servers divided by songs: halves with two busy GPUs.
    /// `None` when no finished song's start time is known.
    pub overall: Option<f64>,
    pub finished: usize,
    /// Songs still to make times `overall`, in minutes. `None` for a run that's done, runs
    /// until stopped, or has no `overall` yet.
    pub eta_minutes: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct AppState {
    pub init: Option<InitInfo>,
    pub servers: Vec<ServerView>,
    pub form: GenerateForm,
    pub runs: Vec<RunSnapshot>,
    pub jobs: BTreeMap<JobId, JobInfo>,
    /// When each job last started running; `JobState::Done` no longer carries it.
    pub job_started: BTreeMap<JobId, SystemTime>,
    pub editors: BTreeMap<RunId, RunEditor>,
    pub library: BTreeMap<SongId, Song>,
    pub filter: LibraryFilter,
    /// The Audio Library's sort.
    pub song_sort: Sorting<SongSort>,
    pub selected: BTreeSet<SongId>,
    /// The song shown in the detail view (last clicked).
    pub current: Option<SongId>,
    /// The Library's lyrics popup is open; it follows [`Self::current`].
    pub library_lyrics: bool,
    pub meta: MetaEditor,
    pub player: PlayerView,
    pub review: ReviewState,
    pub keypoints: KeypointEditor,
    pub projects: Vec<ProjectSummary>,
    pub projects_view: ProjectsView,
    /// The Inputs panel's sort.
    pub reference_sort: Sorting<ReferenceSort>,
    pub history: HistoryView,
    pub transcribe: TranscribeView,
    /// The app's messages that aren't about a server; server lines go to `ServerView::log`.
    pub log: VecDeque<LogLine>,
    pub log_filter: LogFilter,
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
    Back {
        big: bool,
    },
    Forward {
        big: bool,
    },
    Rate(Rating),
    /// Rates bad like `Rate(Bad)`, but asks first if the song played less than
    /// [`MIN_LISTEN`] in total.
    RateBadChecked,
    Next,
    Prev,
    Tag,
    Rename,
    /// Opens or closes the lyrics popup.
    Lyrics,
    /// Jumps to the keypoint after the highlighted one; nothing after the last.
    NextKeypoint,
    /// Jumps back to the keypoint before the highlighted one (or replays the first).
    PrevKeypoint,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiAction {
    SelectTab(Tab),
    // --- Generate form
    SetName(String),
    /// Enter pressed in the Name field.
    SubmitName,
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
    /// A picked lyrics file's text; replaces the form's lyrics.
    LyricsFileLoaded(String),
    /// Edits the name in [`Dialog::NameProject`].
    SetDialogName(String),
    /// Fills Generate from a queued run or a history record (asks first over a named form).
    LoadIntoForm(FormLoad),
    /// Resets the Generate form to its startup state (asks first).
    ClearForm,
    RefreshHistory,
    SelectHistoryRun(RunId),
    SetHistorySearch(String),
    /// Queues a new run continuing an interrupted one.
    ResumeInterrupted(RunId),
    /// A message for the status bar, e.g. why a dropped file was not used.
    ShowStatus(String),
    Retranscribe,
    ContinueFromSeed(u32),
    LoadProjectInputs,
    SetAdvancedOpen(bool),
    SetAbcSectionOpen(bool),
    SetLyricsOpen(bool),
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
    /// Only for runs that aren't active (paused, stopping or done).
    RemoveRun(RunId),
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
    /// Pause or resume whatever is loaded, regardless of the selected song or tab.
    PauseResume,
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
    // --- Projects
    SelectProject(String),
    SetProjectSearch(String),
    /// Sorts the Projects list by a column; the column it is already sorted by reverses.
    SortProjects(ProjectSort),
    SortHistory(HistorySort),
    /// Sorts the Inputs list by a column; the column it is already sorted by reverses.
    SortReferences(ReferenceSort),
    /// Sorts the Audio Library by a column; the column it is already sorted by reverses.
    SortSongs(SongSort),
    SetProjectRename(String),
    RenameProject,
    DeleteProject(String),
    RefreshProjects,
    /// Fills the Generate form from the project.
    OpenProject(String),
    /// Shows the project's songs in the Audio Library.
    ShowProjectSongs(String),
    // --- Inputs
    PlayReference(String),
    /// Loads a project's reference into the project named in the Generate form. A
    /// transcription of the same audio is reused.
    UseReference(String),
    RetranscribeProject(String),
    // --- Review
    EnterReview,
    ReviewKey(ReviewKey),
    /// The pane's keypoints as being edited; not saved until [`UiAction::CommitKeypoints`].
    SetKeypoints(Keypoints),
    CommitKeypoints,
    /// Adds a keypoint at the playhead, after the others.
    AddKeypoint,
    RemoveKeypoint(usize),
    /// Swaps a keypoint with the one above (`up`) or below it.
    MoveKeypoint {
        index: usize,
        up: bool,
    },
    /// Plays from a keypoint's start and highlights it.
    PlayKeypoint(usize),
    /// Closes the review lyrics popup (Escape, click outside, *Close*).
    CloseLyrics,
    /// Opens or closes the Library's lyrics popup for the current song.
    SetLibraryLyrics(bool),
    /// Whether a song arriving after the review ran out starts playing.
    SetReviewAutoplay(bool),
    FocusHandled,
    // --- App
    ToggleLogSource(LogSource),
    /// Shows every log source again.
    ShowAllLogs,
    /// Clears the lines of the shown sources.
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
    /// The lines the Log tab shows: the shown sources' buffers merged by time.
    pub fn log_lines(&self) -> Vec<&LogLine> {
        let f = &self.log_filter;
        let mut lines: Vec<&LogLine> = self
            .servers
            .iter()
            .filter(|v| f.shows(LogSource::Server(v.info.id)))
            .flat_map(|v| &v.log)
            .collect();
        if f.shows(LogSource::App) {
            lines.extend(&self.log);
        }
        // stable, so each buffer keeps its own order
        lines.sort_by_key(|l| l.time);
        lines
    }

    /// The most recent error from any source.
    pub fn last_error(&self) -> Option<&LogLine> {
        std::iter::once(&self.log)
            .chain(self.servers.iter().map(|v| &v.log))
            .filter_map(|log| log.iter().rev().find(|l| l.level == LogLevel::Error))
            .max_by_key(|l| l.time)
    }

    pub fn song(&self, id: &SongId) -> Option<&Song> {
        self.library.get(id)
    }

    pub fn current_song(&self) -> Option<&Song> {
        self.current.as_ref().and_then(|id| self.library.get(id))
    }

    pub fn seeds_for(&self, name: &str) -> BTreeSet<u32> {
        incremusic_core::library::seeds_for(self.library.values(), name.trim())
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

    pub fn current_project(&self) -> Option<&ProjectSummary> {
        let name = self.projects_view.current.as_deref()?;
        self.projects.iter().find(|p| p.name == name)
    }

    /// Projects matching the Projects panel's search, by name or reference file name,
    /// in the panel's sort order.
    pub fn filtered_projects(&self) -> Vec<&ProjectSummary> {
        let pv = &self.projects_view;
        let q = pv.search.trim().to_lowercase();
        let mut v: Vec<&ProjectSummary> = self
            .projects
            .iter()
            .filter(|p| {
                q.is_empty()
                    || p.name.to_lowercase().contains(&q)
                    || p.reference
                        .as_ref()
                        .is_some_and(|r| r.original_name.to_lowercase().contains(&q))
            })
            .collect();
        let by_name = |a: &&ProjectSummary, b: &&ProjectSummary| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.name.cmp(&b.name))
        };
        // the other columns sort stably, so ties stay in name order
        v.sort_by(by_name);
        match pv.sort.by {
            ProjectSort::Name => {}
            ProjectSort::Songs => {
                v.sort_by_cached_key(|p| std::cmp::Reverse(self.project_songs(&p.name)));
            }
            // projects without a reference go last
            ProjectSort::Reference => v.sort_by_cached_key(|p| {
                p.reference
                    .as_ref()
                    .map(|r| r.original_name.to_lowercase())
                    .map_or((true, String::new()), |n| (false, n))
            }),
            // newest first; RFC 3339 UTC strings order by time
            ProjectSort::Created => v.sort_by(|a, b| b.created_at.cmp(&a.created_at)),
            ProjectSort::Modified => v.sort_by(|a, b| b.modified_at.cmp(&a.modified_at)),
        }
        pv.sort.finish(v)
    }

    /// The History rows matching the search, in the chosen column's order.
    pub fn history_rows(&self) -> Vec<&RunSummary> {
        let h = &self.history;
        let mut v = h.filtered();
        // newest first; the other columns sort stably, so ties stay newest first
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        match h.sort.by {
            HistorySort::Created => {}
            HistorySort::Name => v.sort_by_cached_key(|r| r.name.to_lowercase()),
            HistorySort::Status => v.sort_by_key(|r| self.run_status(r).0.label()),
            HistorySort::Seeds => v.sort_by_key(|r| r.start_seed),
            HistorySort::Model => v.sort_by_cached_key(|r| r.model_id.to_lowercase()),
        }
        h.sort.finish(v)
    }

    /// A history row's status and its done and failed counts: a run still in the queue
    /// reports its live state over the list's snapshot.
    pub fn run_status(&self, r: &RunSummary) -> (RecordStatus, u32, u32) {
        match self.runs.iter().find(|q| q.state.id == r.id) {
            Some(q) => (q.state.status.into(), q.done, q.failed),
            None => (r.status, r.done, r.failed),
        }
    }

    /// Songs whose recipe names this project's run.
    pub fn project_songs(&self, name: &str) -> usize {
        self.library
            .values()
            .filter(|s| s.run_name().as_deref() == Some(name))
            .count()
    }

    /// Why a project can't be renamed or deleted right now.
    pub fn project_busy(&self, name: &str) -> Option<&'static str> {
        if self
            .runs
            .iter()
            .any(|r| r.state.status != RunStatus::Done && r.state.spec.name.as_str() == name)
        {
            return Some("A run in the queue is still writing to this project.");
        }
        if self.transcribe.project.as_deref() == Some(name) {
            return Some("A transcription is writing to this project.");
        }
        None
    }

    /// Every reference audio across projects, one row per distinct file, by original name.
    pub fn reference_rows(&self) -> Vec<ReferenceRow<'_>> {
        let mut rows: Vec<ReferenceRow<'_>> = Vec::new();
        for p in &self.projects {
            let Some(r) = &p.reference else { continue };
            match rows.iter_mut().find(|x| x.reference.sha256 == r.sha256) {
                Some(row) => {
                    row.projects.push(p);
                    if row.transcribed.is_none() && p.transcription.is_some() {
                        row.transcribed = Some(p);
                    }
                }
                None => rows.push(ReferenceRow {
                    project: p,
                    reference: r,
                    projects: vec![p],
                    transcribed: p.transcription.is_some().then_some(p),
                }),
            }
        }
        // the other columns sort stably, so ties stay in file name order
        rows.sort_by_cached_key(|r| r.reference.original_name.to_lowercase());
        let lower = |s: &str| s.to_lowercase();
        match self.reference_sort.by {
            ReferenceSort::File => {}
            ReferenceSort::Format => rows.sort_by_cached_key(|r| lower(&r.reference.format)),
            // largest first, missing files last
            ReferenceSort::Size => {
                rows.sort_by_key(|r| std::cmp::Reverse(r.project.reference_bytes))
            }
            ReferenceSort::Projects => rows.sort_by_cached_key(|r| {
                r.projects
                    .iter()
                    .map(|p| lower(&p.name))
                    .collect::<Vec<_>>()
            }),
            // newest first, untranscribed last; RFC 3339 UTC strings order by time
            ReferenceSort::Transcription => rows.sort_by_cached_key(|r| {
                std::cmp::Reverse(
                    r.transcribed
                        .and_then(|p| p.transcription.as_ref())
                        .map(|t| t.created_at.clone()),
                )
            }),
            ReferenceSort::Created => rows.sort_by(|a, b| b.created_at().cmp(a.created_at())),
        }
        self.reference_sort.finish(rows)
    }

    /// Whether the player holds this project's reference.
    pub fn playing_reference(&self, project: &str) -> bool {
        self.player.song.as_ref() == Some(&reference_play_id(project))
    }

    /// "Load project inputs" is offered when the name matches an existing project.
    pub fn offers_project(&self) -> bool {
        self.form.name_result().is_ok() && self.project(&self.form.name).is_some()
    }

    /// Why Load .abc is disabled (they need a project to save into).
    pub fn inputs_blocker(&self) -> Option<&'static str> {
        self.form
            .name_result()
            .err()
            .map(|_| "Enter a name first. Inputs are saved to inputs/<name>/.")
    }

    /// What a file dropped onto the Generate panel does (§5.2): `.abc` files go into the
    /// editor, anything else is transcribed (the core probes it with ffprobe).
    pub fn drop_target(&self, files: &[PathBuf]) -> Result<(DropKind, PathBuf), String> {
        let [path] = files else {
            return Err("Drop one file at a time.".into());
        };
        let is_abc = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("abc"));
        if is_abc {
            // audio can be named after the fact, an ABC file has nowhere to go yet
            if let Some(b) = self.inputs_blocker() {
                return Err(b.into());
            }
            return Ok((DropKind::Abc, path.clone()));
        }
        if self.transcribe.project.is_some() {
            return Err("A transcription is already running.".into());
        }
        Ok((DropKind::Audio, path.clone()))
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
            a.created_at()
                .cmp(&b.created_at())
                .then(a.modified.cmp(&b.modified))
                .then(a.stem.cmp(&b.stem))
        });
        // the other columns sort stably, so ties stay oldest first; songs without the
        // value go last
        let last = |x: Option<String>| x.map_or((true, String::new()), |x| (false, x));
        match self.song_sort.by {
            SongSort::Created => v.reverse(),
            SongSort::Title => v.sort_by_cached_key(|s| s.title().to_lowercase()),
            SongSort::Run => v.sort_by_cached_key(|s| last(s.run_name().map(|n| n.to_lowercase()))),
            SongSort::Seed => v.sort_by_key(|s| (s.seed().is_none(), s.seed())),
            SongSort::Revision => v.sort_by_key(|s| (s.revision().is_none(), s.revision())),
            // good, neutral, bad, then unreviewed
            SongSort::Rating => v.sort_by_key(|s| (s.rating().is_none(), s.rating())),
            SongSort::Tags => v.sort_by_cached_key(|s| {
                let mut tags: Vec<String> = s.meta.tags.iter().map(|t| t.to_lowercase()).collect();
                tags.sort();
                last((!tags.is_empty()).then(|| tags.join(" ")))
            }),
        }
        self.song_sort.finish(v)
    }

    pub fn all_tags(&self) -> BTreeSet<String> {
        self.library
            .values()
            .flat_map(|s| s.meta.tags.iter().cloned())
            .collect()
    }

    /// Runs not yet done, and the songs they still have to make; the flag is set when
    /// a run goes on until stopped, so the song count is only a lower bound.
    pub fn queued_totals(&self) -> (usize, u32, bool) {
        let mut runs = 0;
        let mut songs = 0;
        let mut open_ended = false;
        for r in self
            .runs
            .iter()
            .filter(|r| r.state.status != RunStatus::Done)
        {
            runs += 1;
            match songs_left(r) {
                Some(n) => songs += n,
                None => open_ended = true,
            }
        }
        (runs, songs, open_ended)
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

    /// Seconds per finished song of a run, `None` until a song has finished.
    pub fn song_rate(&self, run: RunId) -> Option<SongRate> {
        let done: Vec<(JobId, u64)> = self
            .jobs
            .values()
            .filter(|j| j.run_id == run && matches!(j.state, JobState::Done(_)))
            .filter_map(|j| j.timing.as_ref().map(|t| (j.id, t.wall_ms)))
            .collect();
        if done.is_empty() {
            return None;
        }
        let per_server =
            done.iter().map(|(_, ms)| ms).sum::<u64>() as f64 / done.len() as f64 / 1000.0;
        // each song generated over [started, started + wall_ms]; the union of those spans is
        // the time the run was generating on any server, so pauses and waits don't count
        let mut spans: Vec<(SystemTime, SystemTime)> = done
            .iter()
            .filter_map(|(id, ms)| {
                let start = *self.job_started.get(id)?;
                Some((start, start + Duration::from_millis(*ms)))
            })
            .collect();
        let overall = (!spans.is_empty()).then(|| {
            spans.sort();
            let mut busy = Duration::ZERO;
            let mut cur = spans[0];
            for &(a, b) in &spans[1..] {
                if a <= cur.1 {
                    cur.1 = cur.1.max(b);
                } else {
                    busy += cur.1.duration_since(cur.0).unwrap_or_default();
                    cur = (a, b);
                }
            }
            busy += cur.1.duration_since(cur.0).unwrap_or_default();
            busy.as_secs_f64() / spans.len() as f64
        });
        let remaining = self
            .runs
            .iter()
            .find(|r| r.state.id == run)
            .and_then(songs_left);
        let eta_minutes = overall
            .zip(remaining)
            .filter(|&(_, n)| n > 0)
            .map(|(secs, n)| secs * n as f64 / 60.0);
        Some(SongRate {
            per_server,
            overall,
            finished: done.len(),
            eta_minutes,
        })
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

    fn model(&self) -> Option<incremusic_core::config::ModelSpec> {
        self.init.as_ref().map(|i| i.models.yue2.clone())
    }

    /// Reloads the editor from the current song, keeping unsaved notes and tag drafts
    /// when it's still the same song.
    fn refresh_meta_editor(&mut self) {
        let old = std::mem::take(&mut self.meta);
        let Some(s) = self.current_song() else {
            return;
        };
        let notes = s.meta.comment.clone().unwrap_or_default();
        let same = old.song.as_ref() == Some(&s.id);
        self.meta = MetaEditor {
            title: s.title().to_string(),
            rename: s.run_name().unwrap_or_default(),
            notes: if same && notes == old.notes_loaded {
                old.notes
            } else {
                notes.clone()
            },
            new_tag: if same { old.new_tag } else { String::new() },
            song: Some(s.id.clone()),
            notes_loaded: notes,
        };
    }

    /// Drops a run and everything the queue shows for it.
    fn forget_run(&mut self, id: RunId) {
        self.runs.retain(|r| r.state.id != id);
        self.jobs.retain(|_, j| j.run_id != id);
        self.job_started.retain(|j, _| self.jobs.contains_key(j));
        self.editors.remove(&id);
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
    let mut cmds = update_inner(state, input.into());
    cmds.extend(sync_keypoints(state));
    cmds
}

fn update_inner(state: &mut AppState, input: Input) -> Vec<Command> {
    match input {
        Input::Ui(a) => {
            let (drafts, tab) = (state.meta.clone(), state.tab);
            let cmds = on_action(state, a);
            // leaving the song, the tab or the app saves the notes and tag being typed
            if state.meta.song != drafts.song || state.tab != tab || state.quit.is_some() {
                let mut saves = save_drafts(state, &drafts);
                if !saves.is_empty() {
                    saves.extend(cmds);
                    return saves;
                }
            }
            cmds
        }
        Input::Core(e) => {
            let caught_up = state.tab == Tab::Review && state.review_song().is_none();
            on_event(state, e);
            if let Some(cmds) = pending_keypoint_seek(state) {
                return cmds;
            }
            // a song arriving after the review ran out becomes the review song
            if caught_up && state.review_song().is_some() {
                if state.player.state == Some(PlayState::Playing) || state.review.no_autoplay {
                    state.current = state.review_song().cloned();
                    state.refresh_meta_editor();
                    return vec![];
                }
                return review_play_current(state);
            }
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
                    path_problems: Vec::new(),
                    log: VecDeque::new(),
                })
                .collect();
            if let Some(p) = &info.default_preset {
                apply_preset(&mut s.form, p, info.default_preset_path.clone());
            }
            s.init = Some(info);
            // a new form: the name comes first (§5.2.1), then the ABC section (§5.2)
            if s.focus.is_none() && s.form.name.is_empty() {
                s.focus = Some(Focus::Name);
            }
        }
        Event::ServerStatus(id, st) => {
            if let Some(v) = s.servers.get_mut(id.0) {
                v.state = st;
            }
        }
        Event::ModelPaths(id, problems) => {
            if let Some(v) = s.servers.get_mut(id.0) {
                v.path_problems = problems;
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
        Event::RunRemoved(id) => s.forget_run(id),
        Event::RunHistory(list) => s.history.runs = list,
        Event::RunRecord(id, r) => {
            if s.history.selected == Some(id) {
                s.history.record = Some(r);
            }
        }
        Event::JobUpdate(id, info) => {
            if let JobState::Running { started, .. } = info.state {
                s.job_started.insert(id, started);
            }
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
                extend_review_queue(s);
            }
            s.refresh_meta_editor();
        }
        Event::Projects(p) => {
            s.projects = p;
            let pv = &mut s.projects_view;
            if let Some(to) = pv.renaming.take()
                && s.projects.iter().any(|p| p.name == to)
            {
                pv.rename = to.clone();
                pv.current = Some(to);
            }
            if pv
                .current
                .as_ref()
                .is_some_and(|c| !s.projects.iter().any(|p| &p.name == c))
            {
                pv.current = None;
                pv.rename.clear();
            }
        }
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
        Event::PlaybackPosition(p) => {
            // ticks arrive every 100 ms while playing; a bigger jump is a seek
            let step = p.saturating_sub(s.player.position);
            if s.player.state == Some(PlayState::Playing)
                && p > s.player.position
                && step <= Duration::from_secs(1)
                && let Some(id) = &s.player.song
            {
                *s.review.listened.entry(id.clone()).or_default() += step;
            }
            s.player.position = p;
        }
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
            // each server has its own buffer, so a chatty one can't push out the others' lines
            match s.servers.iter_mut().find(|v| v.info.name == l.source) {
                Some(v) => push_capped(&mut v.log, l),
                None => push_capped(&mut s.log, l),
            }
        }
    }
}

fn push_capped(log: &mut VecDeque<LogLine>, l: LogLine) {
    log.push_back(l);
    while log.len() > MAX_LOG {
        log.pop_front();
    }
}

/// The form as a fresh start shows it: defaults, then the default preset, focus on Name.
fn clear_form(s: &mut AppState) {
    s.form = GenerateForm::default();
    if let Some(info) = &s.init
        && let Some(p) = &info.default_preset
    {
        apply_preset(&mut s.form, p, info.default_preset_path.clone());
    }
    s.focus = Some(Focus::Name);
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
            if t != s.tab {
                s.library_lyrics = false;
            }
            if t == Tab::Review && s.tab != Tab::Review {
                // an ongoing review picks up where it left off; `EnterReview` restarts it
                if s.review.queue.is_empty() {
                    return on_action(s, A::EnterReview);
                }
                return resume_review(s);
            }
            let entering_history = t == Tab::History && s.tab != Tab::History;
            s.tab = t;
            if entering_history {
                return on_action(s, A::RefreshHistory);
            }
            vec![]
        }
        A::RefreshHistory => {
            let mut cmds = vec![Command::ListRuns];
            if let Some(id) = s.history.selected {
                cmds.push(Command::LoadRunRecord(id));
            }
            cmds
        }
        A::SelectHistoryRun(id) => {
            if s.history.selected == Some(id) {
                return vec![];
            }
            s.history.selected = Some(id);
            s.history.record = None;
            vec![Command::LoadRunRecord(id)]
        }
        A::SetHistorySearch(q) => {
            s.history.search = q;
            vec![]
        }
        A::ResumeInterrupted(id) => {
            s.status = Some("Resuming the interrupted run; it's added to the queue".into());
            vec![Command::ResumeInterrupted(id)]
        }
        A::LoadIntoForm(what) => {
            if !s.form.name.trim().is_empty() {
                s.dialog = Some(Dialog::ReplaceForm(what));
                return vec![];
            }
            load_into_form(s, what);
            vec![]
        }
        A::ClearForm => {
            s.dialog = Some(Dialog::ClearForm);
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
            let sha256 = incremusic_core::fsutil::sha256_hex(contents.as_bytes());
            s.form.set_abc_text(contents);
            s.form.abc_source = AbcSource::File { file_name, sha256 };
            s.form.abc_choice = AbcChoice::LoadFile;
            vec![]
        }
        A::LyricsFileLoaded(text) => {
            s.form.params.lyrics = text;
            s.form.lyrics_open = true;
            vec![]
        }
        A::SubmitName => {
            if s.form.name_result().is_ok()
                && s.form.abc_text.trim().is_empty()
                && s.form.abc_choice != AbcChoice::None
            {
                s.form.abc_section_open = true;
                s.focus = Some(Focus::AbcSection);
            }
            vec![]
        }
        A::TranscribeFile(audio) => transcribe(s, audio, false),
        A::ShowStatus(why) => {
            s.status = Some(why);
            vec![]
        }
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
        A::SetLyricsOpen(b) => {
            s.form.lyrics_open = b;
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
            Some(Dialog::NameProject { audio, name }) => {
                if incremusic_core::run::RunName::parse(&name).is_err() {
                    // the view disables the button; Enter on a bad name keeps the dialog open
                    s.dialog = Some(Dialog::NameProject { audio, name });
                    return vec![];
                }
                s.form.name = name.trim().to_string();
                transcribe(s, audio, false)
            }
            Some(Dialog::ReplaceForm(what)) => {
                load_into_form(s, what);
                vec![]
            }
            Some(Dialog::ClearForm) => {
                clear_form(s);
                vec![]
            }
            Some(Dialog::DeleteProject(name)) => {
                if let Some(why) = s.project_busy(&name) {
                    s.status = Some(why.into());
                    return vec![];
                }
                vec![Command::DeleteProject(name)]
            }
            Some(Dialog::ShortListen { song, .. }) => {
                // the dialog blocks the review keys, but a review restart could intervene
                if s.review_song() == Some(&song) {
                    return review_key(s, ReviewKey::Rate(Rating::Bad));
                }
                vec![]
            }
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
        A::SetDialogName(n) => {
            if let Some(Dialog::NameProject { name, .. }) = &mut s.dialog {
                *name = n;
            }
            vec![]
        }
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
        A::RemoveRun(id) => {
            let removable = s
                .runs
                .iter()
                .any(|r| r.state.id == id && r.state.status != RunStatus::Active);
            if !removable {
                return vec![];
            }
            s.forget_run(id);
            vec![Command::RemoveRun(id)]
        }
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
            s.job_started.retain(|j, _| s.jobs.contains_key(j));
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
            s.player.tab = Some(s.tab);
            vec![Command::Play(id)]
        }
        A::TogglePlay => match (&s.player.state, &s.current) {
            (Some(PlayState::Playing | PlayState::Paused), _)
                if s.player.song == s.current || s.current.is_none() =>
            {
                vec![Command::TogglePause]
            }
            (_, Some(id)) => {
                s.player.tab = Some(s.tab);
                vec![Command::Play(id.clone())]
            }
            _ => vec![],
        },
        A::PauseResume => match s.player.state {
            Some(PlayState::Playing | PlayState::Paused) => vec![Command::TogglePause],
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
            if let Err(e) = incremusic_core::run::RunName::parse(&s.meta.rename) {
                s.status = Some(format!("Rename: {e}"));
                return vec![];
            }
            vec![Command::Library(LibraryCommand::Rename(
                song.id.clone(),
                s.meta.rename.trim().to_string(),
            ))]
        }
        A::CommitNotes => {
            let drafts = MetaEditor {
                new_tag: String::new(),
                ..s.meta.clone()
            };
            save_drafts(s, &drafts)
        }
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
        A::SelectProject(name) => {
            s.projects_view.rename = name.clone();
            s.projects_view.current = Some(name);
            vec![]
        }
        A::SetProjectSearch(q) => {
            s.projects_view.search = q;
            vec![]
        }
        A::SortProjects(sort) => {
            s.projects_view.sort.toggle(sort);
            vec![]
        }
        A::SortReferences(sort) => {
            s.reference_sort.toggle(sort);
            vec![]
        }
        A::SortHistory(sort) => {
            s.history.sort.toggle(sort);
            vec![]
        }
        A::SortSongs(sort) => {
            s.song_sort.toggle(sort);
            vec![]
        }
        A::SetProjectRename(n) => {
            s.projects_view.rename = n;
            vec![]
        }
        A::RenameProject => {
            let Some(from) = s.projects_view.current.clone() else {
                return vec![];
            };
            let to = match incremusic_core::run::RunName::parse(&s.projects_view.rename) {
                Ok(n) => n,
                Err(e) => {
                    s.status = Some(format!("Rename: {e}"));
                    return vec![];
                }
            };
            if to.as_str() == from {
                return vec![];
            }
            if s.project(to.as_str()).is_some() {
                s.status = Some(format!("Rename: a project “{to}” already exists"));
                return vec![];
            }
            if let Some(why) = s.project_busy(&from) {
                s.status = Some(why.into());
                return vec![];
            }
            s.projects_view.renaming = Some(to.to_string());
            vec![Command::RenameProject(from, to)]
        }
        A::DeleteProject(name) => {
            match s.project_busy(&name) {
                Some(why) => s.status = Some(why.into()),
                None => s.dialog = Some(Dialog::DeleteProject(name)),
            }
            vec![]
        }
        A::RefreshProjects => vec![Command::RefreshProjects],
        A::OpenProject(name) => {
            s.form.name = name.clone();
            s.tab = Tab::Generate;
            vec![Command::LoadProject(name)]
        }
        A::ShowProjectSongs(name) => {
            s.filter = LibraryFilter {
                run_name: name,
                ..Default::default()
            };
            s.tab = Tab::Library;
            vec![]
        }
        A::PlayReference(name) => {
            if s.playing_reference(&name) {
                return vec![Command::TogglePause];
            }
            s.player.tab = Some(Tab::Inputs);
            vec![Command::PlayReference(name)]
        }
        A::UseReference(from) => {
            let Some(audio) = s.project(&from).and_then(|p| p.reference_path()) else {
                return vec![];
            };
            if let Some(b) = s.inputs_blocker() {
                s.status = Some(b.into());
                s.tab = Tab::Generate;
                s.focus = Some(Focus::Name);
                return vec![];
            }
            if s.transcribe.project.is_some() {
                s.status = Some("A transcription is already running.".into());
                return vec![];
            }
            s.tab = Tab::Generate;
            s.form.abc_section_open = true;
            let sha = |name: &str| {
                s.project(name)
                    .and_then(|p| p.reference.as_ref())
                    .map(|r| r.sha256.clone())
            };
            match s.form.name_result() {
                // the same audio is already there: nothing to replace, so no dialog
                Ok(project) if sha(project.as_str()) == sha(&from) => vec![Command::Transcribe {
                    project,
                    audio,
                    force: false,
                }],
                _ => transcribe(s, audio, false),
            }
        }
        A::RetranscribeProject(name) => {
            if s.transcribe.project.is_some() {
                s.status = Some("A transcription is already running.".into());
                return vec![];
            }
            match incremusic_core::run::RunName::parse(&name) {
                Ok(n) => vec![Command::Retranscribe(n)],
                Err(e) => {
                    s.status = Some(format!("{name}: {e}"));
                    vec![]
                }
            }
        }
        A::EnterReview => {
            s.tab = Tab::Review;
            s.review = ReviewState {
                queue: s.unreviewed_in_order(),
                index: 0,
                rating: BTreeSet::new(),
                lyrics: s.review.lyrics,
                no_autoplay: s.review.no_autoplay,
                listened: std::mem::take(&mut s.review.listened),
                ..Default::default()
            };
            review_play_current(s)
        }
        A::ReviewKey(k) => review_key(s, k),
        A::SetKeypoints(k) => {
            s.keypoints.keypoints = k;
            vec![]
        }
        A::CommitKeypoints => commit_keypoints(s),
        A::AddKeypoint => {
            let here = s.review_song().is_some() && s.player.song.as_ref() == s.review_song();
            if s.keypoints.project.is_none() || !here {
                return vec![];
            }
            s.keypoints.keypoints.points.push(Keypoint {
                at_ms: s.player.position.as_millis() as u64,
                ..Default::default()
            });
            commit_keypoints(s)
        }
        A::RemoveKeypoint(i) => {
            let points = &mut s.keypoints.keypoints.points;
            if i >= points.len() {
                return vec![];
            }
            points.remove(i);
            // the one after it is still next
            s.review.keypoint = match s.review.keypoint {
                Some(c) if c == i => c.checked_sub(1),
                Some(c) if c > i => Some(c - 1),
                c => c,
            };
            commit_keypoints(s)
        }
        A::MoveKeypoint { index: i, up } => {
            let points = &mut s.keypoints.keypoints.points;
            let j = if up { i.checked_sub(1) } else { Some(i + 1) };
            let Some(j) = j.filter(|&j| j < points.len() && i < points.len()) else {
                return vec![];
            };
            points.swap(i, j);
            // the highlight follows its keypoint
            s.review.keypoint = s.review.keypoint.map(|c| match c {
                c if c == i => j,
                c if c == j => i,
                c => c,
            });
            commit_keypoints(s)
        }
        A::PlayKeypoint(i) => play_keypoint(s, i),
        A::CloseLyrics => {
            s.review.lyrics = false;
            vec![]
        }
        A::SetLibraryLyrics(open) => {
            s.library_lyrics = open;
            vec![]
        }
        A::SetReviewAutoplay(on) => {
            s.review.no_autoplay = !on;
            vec![]
        }
        A::FocusHandled => {
            s.focus = None;
            vec![]
        }
        A::ToggleLogSource(src) => {
            let hidden = &mut s.log_filter.hidden;
            if !hidden.remove(&src) {
                hidden.insert(src);
            }
            vec![]
        }
        A::ShowAllLogs => {
            s.log_filter.hidden.clear();
            vec![]
        }
        A::ClearLog => {
            let f = &s.log_filter;
            if f.shows(LogSource::App) {
                s.log.clear();
            }
            s.servers
                .iter_mut()
                .filter(|v| f.shows(LogSource::Server(v.info.id)))
                .for_each(|v| v.log.clear());
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
        let name = suggested_name(&audio);
        s.dialog = Some(Dialog::NameProject { audio, name });
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

/// A project name from an audio file's base name, with characters names can't hold
/// replaced so the suggestion is usually valid as is.
fn suggested_name(audio: &Path) -> String {
    let stem = audio
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(incremusic_core::run::MAX_NAME_LEN)
        .collect();
    name.trim().trim_end_matches('.').to_string()
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
            // keep the name; move the seed past this run so the next one
            // doesn't collide (an open-ended run has no known end)
            if let Some(n) = spec.count {
                s.form.seed = spec.start_seed.saturating_add(n).to_string();
            }
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
        Ok((params, _)) => fill_form(
            s,
            &recipe.run.name,
            &params,
            &recipe.abc_source,
            recipe.run.seed.saturating_add(1),
            None,
        ),
        Err(e) => s.status = Some(format!("Recipe: {e}")),
    }
}

/// Fills Generate from a queued run or a history record so it can be started again. The
/// seed continues after everything the run and the library already used.
fn load_into_form(s: &mut AppState, what: FormLoad) {
    let (name, params, abc, next_seed, count, model) = match what {
        FormLoad::Queue(id) => {
            let Some(r) = s.runs.iter().find(|r| r.state.id == id) else {
                s.status = Some("That run is no longer in the queue".into());
                return;
            };
            let st = &r.state;
            (
                st.spec.name.clone(),
                st.spec.params.clone(),
                st.spec.abc_source.clone(),
                st.next_seed,
                st.spec.count,
                st.spec.model.clone(),
            )
        }
        FormLoad::Record(rev) => {
            let Some(r) = s.history.record() else {
                s.status = Some("Select a run in History first".into());
                return;
            };
            let Some(v) = r.revision(rev) else {
                s.status = Some(format!("Run “{}” has no revision {rev:?}", r.name));
                return;
            };
            (
                r.name.clone(),
                v.params.clone(),
                v.abc_source.clone(),
                r.next_seed,
                r.count,
                r.model.clone(),
            )
        }
    };
    fill_form(s, name.as_str(), &params, &abc, next_seed, Some(count));
    warn_model(s, &model);
}

fn warn_model(s: &mut AppState, model: &ModelSpec) {
    if let Some(now) = s.model()
        && now.id != model.id
    {
        s.status = Some(format!(
            "That run used model “{}”; Generate uses the configured “{}”",
            model.id, now.id
        ));
    }
}

/// Puts a run's inputs into the Generate form and focuses the name. `seed_floor` is the
/// lowest seed to continue from; the library's next free seed wins if it's higher.
/// `count: Some(_)` also sets the count (`Some(None)` → until stopped).
fn fill_form(
    s: &mut AppState,
    name: &str,
    params: &GenerationParams,
    abc: &AbcSource,
    seed_floor: u32,
    count: Option<Option<u32>>,
) {
    s.form.name = name.to_string();
    s.form.load_params(params);
    s.form.abc_source = abc.clone();
    s.form.abc_choice = match abc {
        AbcSource::None => AbcChoice::None,
        AbcSource::File { .. } => AbcChoice::LoadFile,
        AbcSource::Transcribed(_) => AbcChoice::Transcribe,
        AbcSource::Manual => AbcChoice::Paste,
    };
    let seeds = s.seeds_for(name);
    let seed = next_free_seed(&seeds).map_or(seed_floor, |n| n.max(seed_floor));
    s.form.seed = seed.to_string();
    if let Some(c) = count {
        s.form.until_stopped = c.is_none();
        if let Some(c) = c {
            s.form.count = c.to_string();
        }
    }
    s.tab = Tab::Generate;
    s.focus = Some(Focus::Name);
}

/// Saves `drafts`' notes and typed tag to the song they belong to.
fn save_drafts(s: &mut AppState, drafts: &MetaEditor) -> Vec<Command> {
    let Some(song) = drafts.song.as_ref().and_then(|id| s.library.get(id)) else {
        return vec![];
    };
    let mut cmds = vec![];
    if song.meta.comment.as_deref().unwrap_or("") != drafts.notes {
        cmds.push(Command::Library(LibraryCommand::SetNotes(
            song.id.clone(),
            drafts.notes.clone(),
        )));
    }
    let tag = drafts.new_tag.trim();
    if !tag.is_empty() && !song.meta.tags.iter().any(|t| t == tag) {
        let mut tags = song.meta.tags.clone();
        tags.push(tag.to_string());
        cmds.push(Command::Library(LibraryCommand::SetTags(
            song.id.clone(),
            tags,
        )));
    }
    if s.meta.song == drafts.song && !tag.is_empty() {
        s.meta.new_tag.clear();
    }
    cmds
}

fn review_play_current(s: &mut AppState) -> Vec<Command> {
    match s.review_song().cloned() {
        Some(id) => {
            s.current = Some(id.clone());
            s.refresh_meta_editor();
            s.player.tab = Some(Tab::Review);
            vec![Command::Play(id)]
        }
        None => {
            s.current = None;
            vec![Command::Stop]
        }
    }
}

/// Songs a run has still to make: `None` when it's done or goes on until stopped.
fn songs_left(r: &RunSnapshot) -> Option<u32> {
    match r.state.status {
        RunStatus::Done => None,
        // no new seeds: only the running jobs are left
        RunStatus::Stopping => Some(r.running),
        RunStatus::Active | RunStatus::Paused => r
            .state
            .spec
            .count
            .map(|c| c.saturating_sub(r.done + r.failed)),
    }
}

/// Appends unreviewed songs missing from the review queue, leaving out ones rated here
/// whose library update is still on its way.
fn extend_review_queue(s: &mut AppState) {
    let unreviewed = s.unreviewed_in_order();
    let r = &mut s.review;
    r.rating.retain(|id| {
        s.library
            .get(id)
            .is_some_and(|song| song.location == Location::Unreviewed)
    });
    for id in unreviewed {
        if !r.queue.contains(&id) && !r.rating.contains(&id) {
            r.queue.push(id);
        }
    }
}

/// Returns to the Review tab without restarting the current song or losing the place in
/// the queue. Songs deleted or rated elsewhere meanwhile drop out (a dropped current song
/// gives way to the next), and new unreviewed ones join the end.
fn resume_review(s: &mut AppState) -> Vec<Command> {
    s.tab = Tab::Review;
    let r = &mut s.review;
    let pending = |id: &SongId| {
        s.library
            .get(id)
            .is_some_and(|song| song.location == Location::Unreviewed)
    };
    let before = r.queue[..r.index.min(r.queue.len())]
        .iter()
        .filter(|id| pending(id))
        .count();
    r.queue.retain(|id| pending(id));
    r.index = before;
    extend_review_queue(s);
    let Some(id) = s.review_song().cloned() else {
        s.current = None;
        return vec![];
    };
    // keep the review song, or a song playing from another tab, going
    if s.player.song.as_ref() == Some(&id) || s.player.state == Some(PlayState::Playing) {
        s.current = Some(id);
        s.refresh_meta_editor();
        return vec![];
    }
    review_play_current(s)
}

fn review_key(s: &mut AppState, k: ReviewKey) -> Vec<Command> {
    match k {
        // Space starts the review song if another tab's song is loaded
        ReviewKey::PlayPause => match s.review_song() {
            Some(id) if s.player.song.as_ref() != Some(id) => review_play_current(s),
            _ => vec![Command::TogglePause],
        },
        ReviewKey::Back { big } => vec![Command::SeekBy(if big { -30.0 } else { -5.0 })],
        ReviewKey::Forward { big } => vec![Command::SeekBy(if big { 30.0 } else { 5.0 })],
        ReviewKey::Rate(rating) => {
            let Some(id) = s.review_song().cloned() else {
                return vec![];
            };
            // rated songs leave the queue; the next one slides into this slot
            let r = &mut s.review;
            r.queue.remove(r.index);
            r.rating.insert(id.clone());
            r.listened.remove(&id);
            let mut cmds = vec![Command::Library(LibraryCommand::Rate(id, rating))];
            cmds.extend(review_play_current(s));
            cmds
        }
        ReviewKey::RateBadChecked => {
            let Some(id) = s.review_song().cloned() else {
                return vec![];
            };
            let listened = s.review.listened.get(&id).copied().unwrap_or_default();
            if listened < MIN_LISTEN {
                s.review.lyrics = false;
                s.dialog = Some(Dialog::ShortListen { song: id, listened });
                return vec![];
            }
            review_key(s, ReviewKey::Rate(Rating::Bad))
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
        // the fields sit behind the lyrics popup
        ReviewKey::Tag => {
            s.review.lyrics = false;
            s.focus = Some(Focus::Tag);
            vec![]
        }
        ReviewKey::Rename => {
            s.review.lyrics = false;
            s.focus = Some(Focus::Rename);
            vec![]
        }
        ReviewKey::Lyrics => {
            s.review.lyrics = !s.review.lyrics;
            vec![]
        }
        ReviewKey::NextKeypoint => {
            let i = s.review.keypoint.map_or(0, |c| c + 1);
            play_keypoint(s, i)
        }
        ReviewKey::PrevKeypoint => match s.review.keypoint {
            Some(c) => play_keypoint(s, c.saturating_sub(1)),
            None => vec![],
        },
    }
}

/// Points the keypoint editor at the review song's project, saving unsaved edits of the
/// one it leaves, and starts the highlight over when the review song changes.
fn sync_keypoints(s: &mut AppState) -> Vec<Command> {
    let song = s.review_song().cloned();
    if s.review.keypoint_song != song {
        s.review.keypoint_song = song.clone();
        s.review.keypoint = None;
        s.review.pending_seek = None;
    }
    let project = song
        .and_then(|id| s.library.get(&id))
        .and_then(Song::project)
        .and_then(|name| {
            s.projects
                .iter()
                .find(|p| p.name == name && p.error.is_none())
        })
        .map(|p| (p.name.clone(), p.keypoints.clone()));
    let ed = &mut s.keypoints;
    let mut cmds = vec![];
    match project {
        Some((name, k)) if ed.project.as_ref() == Some(&name) => {
            if k != ed.loaded {
                ed.keypoints = k.clone();
                ed.loaded = k;
            }
        }
        _ => {
            if let Some(old) = &ed.project
                && ed.keypoints != ed.loaded
            {
                cmds.push(Command::SetKeypoints(old.clone(), ed.keypoints.clone()));
                if let Some(p) = s.projects.iter_mut().find(|p| &p.name == old) {
                    p.keypoints = ed.keypoints.clone();
                }
            }
            s.keypoints = match project {
                Some((name, k)) => KeypointEditor {
                    project: Some(name),
                    keypoints: k.clone(),
                    loaded: k,
                },
                None => KeypointEditor::default(),
            };
        }
    }
    cmds
}

/// Saves the edited keypoints if they changed. The project list is updated to match, as
/// the core doesn't send it back.
fn commit_keypoints(s: &mut AppState) -> Vec<Command> {
    let ed = &mut s.keypoints;
    let Some(name) = ed.project.clone() else {
        return vec![];
    };
    if ed.keypoints == ed.loaded {
        return vec![];
    }
    ed.loaded = ed.keypoints.clone();
    if let Some(p) = s.projects.iter_mut().find(|p| p.name == name) {
        p.keypoints = ed.keypoints.clone();
    }
    vec![Command::SetKeypoints(name, ed.keypoints.clone())]
}

/// Plays the review song from keypoint `i`'s start and highlights it. A song that isn't
/// loaded yet is started, and the seek waits for it ([`pending_keypoint_seek`]).
fn play_keypoint(s: &mut AppState, i: usize) -> Vec<Command> {
    let (Some(start), Some(id)) = (s.keypoints.keypoints.start(i), s.review_song().cloned()) else {
        return vec![];
    };
    s.review.keypoint = Some(i);
    let this = s.player.song.as_ref() == Some(&id);
    match s.player.state {
        Some(PlayState::Playing) if this => {
            s.player.position = start;
            vec![Command::Seek(start)]
        }
        Some(PlayState::Paused) if this => {
            s.player.position = start;
            vec![Command::Seek(start), Command::Resume]
        }
        Some(PlayState::Loading) if this => {
            s.review.pending_seek = Some(start);
            vec![]
        }
        _ => {
            s.review.pending_seek = Some(start);
            s.current = Some(id.clone());
            s.player.tab = Some(Tab::Review);
            vec![Command::Play(id)]
        }
    }
}

/// The seek [`play_keypoint`] left waiting, once the review song has loaded.
fn pending_keypoint_seek(s: &mut AppState) -> Option<Vec<Command>> {
    s.review.pending_seek?;
    let loaded = s.player.song.is_some()
        && s.player.song.as_ref() == s.review_song()
        && matches!(s.player.state, Some(PlayState::Playing | PlayState::Paused));
    if !loaded {
        return None;
    }
    let start = s.review.pending_seek.take()?;
    s.player.position = start;
    let mut cmds = vec![Command::Seek(start)];
    if s.player.state == Some(PlayState::Paused) {
        cmds.push(Command::Resume);
    }
    Some(cmds)
}

/// A keypoint time as `m:ss.s`.
pub fn fmt_keypoint(ms: u64) -> String {
    let tenths = (ms + 50) / 100;
    format!("{}:{:02}.{}", tenths / 600, (tenths / 10) % 60, tenths % 10)
}

/// Reads `m:ss.s`, `h:mm:ss` or plain seconds (`65.5`) back into milliseconds.
pub fn parse_keypoint(text: &str) -> Option<u64> {
    let mut secs = 0.0;
    for part in text.trim().split(':') {
        let v: f64 = part.trim().parse().ok()?;
        if !(v >= 0.0 && v.is_finite()) {
            return None;
        }
        secs = secs * 60.0 + v;
    }
    Some((secs * 1000.0).round() as u64)
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
