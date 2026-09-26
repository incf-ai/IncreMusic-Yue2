//! Headless GUI tests (design §9.3): the real egui app driven through the AccessKit tree,
//! with the core replaced by a fake that records `Command`s and replays scripted `Event`s.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use audiocpp_core::api::Timing;
use audiocpp_core::config::{ModelSpec, Models};
use audiocpp_core::library::{LibraryDelta, Location, Song};
use audiocpp_core::media::{
    Rating, Recipe, RecipeModel, RecipeOutput, RecipeRun, RecipeServer, SongMeta,
};
use audiocpp_core::params::Preset;
use audiocpp_core::playback::Playback;
use audiocpp_core::run::{AbcSource, JobId, RunEdit, RunId, RunState};
use audiocpp_core::scheduler::{JobInfo, JobState, RunSnapshot, ServerId};
use audiocpp_core::service::{Command, CoreBackend, Event, InitInfo, ServerInfo, ServerState};
use audiocpp_core::{LibraryCommand, SongId};
use audiocpp_gui::{Effect, EffectRunner, GuiApp};
use audiocpp_gui_core::UiAction;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

#[derive(Default)]
struct FakeCore {
    commands: Mutex<Vec<Command>>,
    events: Mutex<VecDeque<Event>>,
}

impl FakeCore {
    fn push(&self, e: Event) {
        self.events.lock().unwrap().push_back(e);
    }
    fn take(&self) -> Vec<Command> {
        std::mem::take(&mut *self.commands.lock().unwrap())
    }
}

impl CoreBackend for FakeCore {
    fn send(&self, cmd: Command) {
        self.commands.lock().unwrap().push(cmd);
    }
    fn try_recv(&self) -> Option<Event> {
        self.events.lock().unwrap().pop_front()
    }
    fn playback(&self) -> Option<Arc<Playback>> {
        None
    }
}

struct NoEffects;
impl EffectRunner for NoEffects {
    fn run(&self, _: Effect, _: std::sync::mpsc::Sender<UiAction>, _: egui::Context) {}
}

fn model(id: &str) -> ModelSpec {
    ModelSpec {
        id: id.into(),
        family: id.into(),
        task: "gen".into(),
        mode: "offline".into(),
        path: "/m".into(),
        load_options: Default::default(),
        session_options: Default::default(),
    }
}

fn init(preset: Option<Preset>) -> Event {
    Event::Init(InitInfo {
        servers: vec![ServerInfo {
            id: ServerId(0),
            name: "gpu1".into(),
            port: 9123,
            launchable: true,
            backend: None,
            device: None,
            log_file: None,
        }],
        library_root: "/lib".into(),
        models: Models {
            yue2: model("yue2"),
            sheetsage2: model("sheetsage2"),
        },
        default_preset: preset,
        default_preset_path: None,
        opener: Ok("gio launch".into()),
        ffmpeg: Ok("ffmpeg".into()),
    })
}

fn preset() -> Preset {
    let mut p = Preset::default();
    p.params.lyrics = "la la".into();
    p.params.style = "pop".into();
    p.params.abc = Some("X:1\nK:C\n".into());
    p
}

fn harness(core: Arc<FakeCore>) -> Harness<'static, GuiApp> {
    let app = GuiApp::without_audio(core, Box::new(NoEffects));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 1000.0))
        .build_ui_state(|ui, app: &mut GuiApp| app.show(ui), app);
    audiocpp_gui::install_fonts(&h.ctx);
    h.run_steps(4);
    h
}

/// Replaces the field's contents, like selecting all and typing.
fn type_into(h: &mut Harness<'_, GuiApp>, label: &str, text: &str) {
    h.get_by_label(label).focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get_by_label(label).type_text(text);
    h.run_steps(4);
}

/// True if some node shows `text` (it may legitimately appear more than once).
fn has_text(h: &Harness<'_, GuiApp>, text: &str) -> bool {
    h.query_all_by_label_contains(text).next().is_some()
}

fn disabled(h: &Harness<'_, GuiApp>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

fn song(name: &str, seed: u32, created: &str) -> Song {
    let stem = format!("{name}-{seed}");
    Song {
        id: SongId(format!("{stem}-id")),
        stem: stem.clone(),
        dir: Some(format!("/lib/unreviewed/{stem}").into()),
        mp3: format!("/lib/unreviewed/{stem}/{stem}.mp3").into(),
        wav: Some(format!("/lib/unreviewed/{stem}/{stem}.wav").into()),
        location: Location::Unreviewed,
        meta: SongMeta {
            title: Some(stem),
            recipe: Some(Recipe {
                schema: 1,
                app_version: "0".into(),
                song_id: "x".into(),
                run: RecipeRun {
                    id: RunId::new(),
                    name: name.into(),
                    seed,
                    start_seed: seed,
                    index: 0,
                    revision: 0,
                },
                created_at: created.into(),
                server: RecipeServer {
                    name: "gpu1".into(),
                    port: 1,
                    backend: None,
                    device: None,
                },
                model: RecipeModel {
                    spec: model("yue2"),
                    file_hashes: Default::default(),
                },
                request: serde_json::json!({"request":{"lyrics":"l","seed":seed,"options":{"style":"s","abc":"X:1"}}}),
                project: name.into(),
                abc_source: AbcSource::Manual,
                timing: Timing::default(),
                output: RecipeOutput {
                    sample_rate: 48000,
                    channels: 2,
                    wav_sha256: String::new(),
                    encoder: "mp3 V0".into(),
                },
            }),
            ..Default::default()
        },
        wav_ok: Some(true),
        modified: SystemTime::UNIX_EPOCH,
        size: 1,
    }
}

#[test]
fn start_run_needs_name_and_seed_then_sends_spec_and_clears_name() {
    let core = Arc::new(FakeCore::default());
    core.push(init(Some(preset())));
    let mut h = harness(core.clone());
    assert!(disabled(&h, "Start run"), "empty name");
    // Load .abc needs a name first; Transcribe asks for one after the file is picked
    assert!(disabled(&h, "Load .abc…"));
    assert!(!disabled(&h, "Transcribe audio…"));

    type_into(&mut h, "Name", "bad/name");
    type_into(&mut h, "Starting seed", "1233");
    assert!(disabled(&h, "Start run"), "invalid name");
    assert!(
        has_text(&h, "must not contain `/`"),
        "reason shown next to the field"
    );

    h.state_mut().dispatch(UiAction::SetName(String::new()));
    type_into(&mut h, "Name", "sunny-hook");
    assert!(!disabled(&h, "Load .abc…"));
    h.state_mut().dispatch(UiAction::SetSeed(String::new()));
    h.run_steps(4);
    assert!(disabled(&h, "Start run"), "seed missing");
    type_into(&mut h, "Starting seed", "1233");
    assert!(!disabled(&h, "Start run"));

    h.get_by_label("Start run").click();
    h.run_steps(4);
    let cmds = core.take();
    let [Command::StartRun(spec)] = cmds.as_slice() else {
        panic!("{cmds:?}")
    };
    assert_eq!(spec.name.as_str(), "sunny-hook");
    assert_eq!((spec.start_seed, spec.count), (1233, Some(10)));
    assert_eq!(spec.params.lyrics, "la la");
    assert_eq!(spec.params.abc.as_deref(), Some("X:1\nK:C\n"));
    assert_eq!(
        h.get_by_label("Name").value().unwrap_or_default(),
        "sunny-hook",
        "name kept"
    );
    assert_eq!(
        h.get_by_label("Starting seed").value().unwrap_or_default(),
        "1243",
        "seed advanced by the run's count"
    );
}

#[test]
fn colliding_name_offers_continue_from_next_seed() {
    let core = Arc::new(FakeCore::default());
    core.push(init(Some(preset())));
    core.push(Event::LibraryChanged(LibraryDelta {
        full: true,
        upserted: vec![song("dup", 5, "a"), song("dup", 6, "b")],
        removed: vec![],
    }));
    let mut h = harness(core.clone());
    type_into(&mut h, "Name", "dup");
    type_into(&mut h, "Starting seed", "1");
    assert!(disabled(&h, "Start run"));
    assert!(has_text(&h, "already exist for this name"));
    h.get_by_label("Continue from seed 7").click();
    h.run_steps(4);
    assert_eq!(
        h.get_by_label("Starting seed").value().unwrap_or_default(),
        "7"
    );
    assert!(!disabled(&h, "Start run"));
}

fn running_run(core: &FakeCore) -> RunId {
    let spec = audiocpp_core::run::RunSpec {
        name: audiocpp_core::run::RunName::parse("live").unwrap(),
        params: preset().params,
        start_seed: 40,
        count: Some(3),
        model: model("yue2"),
        abc_source: AbcSource::Manual,
    };
    let id = RunId::new();
    let mut state = RunState::new(id, spec);
    state.next_seed = 41;
    state.issued = 1;
    core.push(Event::RunUpdate(RunSnapshot {
        state,
        position: 0,
        done: 0,
        failed: 0,
        running: 1,
        queued_retries: 0,
    }));
    core.push(Event::JobUpdate(
        JobId(1),
        JobInfo {
            id: JobId(1),
            run_id: id,
            seed: 40,
            revision: 0,
            attempts: 0,
            state: JobState::Running {
                server: ServerId(0),
                started: SystemTime::now(),
                estimate_ms: Some(200_000),
            },
            timing: None,
        },
    ));
    id
}

#[test]
fn queue_rows_load_into_generate() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let mut h = harness(core.clone());
    running_run(&core);
    h.run_steps(2);
    h.get_by_label_contains("Queue").click();
    h.run_steps(2);
    h.get_by_label("Load live into Generate").click();
    h.run_steps(3);
    let st = &h.state().state;
    assert_eq!(st.tab, audiocpp_gui_core::Tab::Generate);
    assert_eq!(st.form.name, "live");
    assert_eq!(st.form.seed, "41", "continues after the run's issued seeds");
}

#[test]
fn history_tab_shows_a_record_and_resumes_it() {
    use audiocpp_core::history::{RecordStatus, Revision, RunRecord, SeedEntry, SeedOutcome};
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let mut h = harness(core.clone());
    let id = RunId::new();
    let record = RunRecord {
        schema: 1,
        app_version: "0".into(),
        id,
        name: audiocpp_core::run::RunName::parse("night-drive").unwrap(),
        created_at: "2026-09-25T10:02:11Z".into(),
        finished_at: Some("2026-09-25T10:40:00Z".into()),
        status: RecordStatus::Interrupted,
        start_seed: 1,
        count: Some(4),
        next_seed: 3,
        issued: 2,
        model: model("yue2"),
        regenerate_of: None,
        resumed_from: None,
        revisions: vec![Revision {
            revision: 0,
            at: "2026-09-25T10:02:11Z".into(),
            first_seed: 1,
            params: preset().params,
            abc_source: AbcSource::Manual,
        }],
        seeds: vec![SeedEntry {
            seed: 1,
            revision: 0,
            outcome: SeedOutcome::Failed("503 busy".into()),
        }],
    };
    h.get_by_label("History").click();
    h.run_steps(2);
    assert!(core.take().contains(&Command::ListRuns));
    core.push(Event::RunHistory(vec![record.summary()]));
    h.run_steps(2);
    h.get_by_label_contains("night-drive").click();
    h.run_steps(2);
    assert_eq!(core.take(), vec![Command::LoadRunRecord(id)]);
    core.push(Event::RunRecord(id, Ok(record)));
    h.run_steps(3);
    assert!(has_text(&h, "failed: 503 busy"));
    h.get_by_label("Resume run").click();
    h.run_steps(2);
    assert_eq!(core.take(), vec![Command::ResumeInterrupted(id)]);
    h.get_by_label("Load into Generate").click();
    h.run_steps(3);
    assert_eq!(h.state().state.form.name, "night-drive");
}

#[test]
fn editing_a_running_run_sends_edit_run_and_job_updates_show_in_the_queue() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let run = running_run(&core);
    let mut h = harness(core.clone());
    h.get_by_label_contains("Queue").click();
    h.run_steps(4);
    let row = h
        .get_by_label_contains("live seed 40 ·")
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(row.contains("running") && row.contains("on gpu1"), "{row}");

    h.get_by_label("Edit live").click();
    h.run_steps(4);
    h.get_by_label("live style").focus();
    h.run_steps(1);
    h.get_by_label("live style").type_text(" rock");
    h.run_steps(4);
    h.get_by_label("Apply params to live").click();
    h.run_steps(4);
    let cmds = core.take();
    let edit = cmds.iter().find_map(|c| match c {
        Command::EditRun(id, RunEdit::Params(p)) if *id == run => Some(p.clone()),
        _ => None,
    });
    assert!(edit.expect("EditRun sent").style.ends_with("rock"));

    // a JobUpdate event updates the row
    core.push(Event::JobUpdate(
        JobId(1),
        JobInfo {
            id: JobId(1),
            run_id: run,
            seed: 40,
            revision: 0,
            attempts: 0,
            state: JobState::Done(SongId("s".into())),
            timing: None,
        },
    ));
    h.run_steps(4);
    let row = h
        .get_by_label_contains("live seed 40 ·")
        .accesskit_node()
        .value()
        .unwrap_or_default();
    assert!(row.ends_with(": done"), "{row}");

    h.get_by_label("Pause live").click();
    h.run_steps(4);
    assert_eq!(core.take(), vec![Command::PauseRun(run)]);
}

#[test]
fn no_abc_asks_for_confirmation_unless_none_or_preset_allows() {
    let core = Arc::new(FakeCore::default());
    let mut p = preset();
    p.params.abc = None;
    core.push(init(Some(p.clone())));
    let mut h = harness(core.clone());
    type_into(&mut h, "Name", "noabc");
    type_into(&mut h, "Starting seed", "1");
    h.get_by_label("Start run").click();
    h.run_steps(4);
    assert!(core.take().is_empty());
    assert!(has_text(&h, "YuE2 will compose its own"));
    h.get_by_label("Continue without ABC").click();
    h.run_steps(4);
    let cmds = core.take();
    let [Command::StartRun(spec)] = cmds.as_slice() else {
        panic!("{cmds:?}")
    };
    assert!(
        spec.params.to_request(1)["options"].get("abc").is_none(),
        "no abc field at all"
    );

    // choosing None on purpose skips the dialog
    type_into(&mut h, "Name", "none-on-purpose");
    h.get_by_label("None: let YuE2 compose").click();
    h.run_steps(4);
    h.get_by_label("Start run").click();
    h.run_steps(4);
    assert!(matches!(core.take().as_slice(), [Command::StartRun(_)]));
    assert!(!has_text(&h, "YuE2 will compose its own"));

    // a preset with allow_no_abc skips it too
    let core = Arc::new(FakeCore::default());
    p.allow_no_abc = true;
    core.push(init(Some(p)));
    let mut h = harness(core.clone());
    type_into(&mut h, "Name", "allowed");
    type_into(&mut h, "Starting seed", "1");
    h.get_by_label("Start run").click();
    h.run_steps(4);
    assert!(matches!(core.take().as_slice(), [Command::StartRun(_)]));
}

#[test]
fn review_mode_key_1_rates_good_and_plays_next() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let (a, b) = (song("r", 1, "2026-01-01"), song("r", 2, "2026-01-02"));
    let (ia, ib) = (a.id.clone(), b.id.clone());
    core.push(Event::LibraryChanged(LibraryDelta {
        full: true,
        upserted: vec![b, a],
        removed: vec![],
    }));
    let mut h = harness(core.clone());
    h.get_by_label_contains("Review (2)").click();
    h.run_steps(4);
    assert_eq!(core.take(), vec![Command::Play(ia.clone())]);
    h.key_press(egui::Key::Num1);
    h.run_steps(4);
    assert_eq!(
        core.take(),
        vec![
            Command::Library(LibraryCommand::Rate(ia, Rating::Good)),
            Command::Play(ib)
        ]
    );
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(1);
    assert_eq!(core.take(), vec![Command::SeekBy(5.0)]);
    h.key_press(egui::Key::T);
    h.run_steps(3);
    assert!(
        h.get_by_label("Add tag").is_focused(),
        "T focuses the tag field"
    );
}

#[test]
fn review_lyrics_popup_opens_with_l_and_closes_with_escape() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let mut a = song("r", 1, "2026-01-01");
    a.meta.recipe.as_mut().unwrap().request =
        serde_json::json!({"request":{"lyrics":"[Verse]\nsunlight on the water","seed":1}});
    let ia = a.id.clone();
    core.push(Event::LibraryChanged(LibraryDelta {
        full: true,
        upserted: vec![a],
        removed: vec![],
    }));
    let mut h = harness(core.clone());
    h.get_by_label_contains("Review (1)").click();
    h.run_steps(4);
    core.take();
    assert!(!has_text(&h, "sunlight on the water"));
    h.key_press(egui::Key::L);
    h.run_steps(3);
    assert!(has_text(&h, "sunlight on the water"), "L opens the lyrics");
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(
        core.take(),
        vec![Command::Play(ia)],
        "review keys still work (the fake player never reports Playing)"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(!has_text(&h, "sunlight on the water"), "Escape closes it");
    h.get_by_label("Lyrics").click();
    h.run_steps(3);
    assert!(has_text(&h, "sunlight on the water"), "the button opens it");
    h.get_by_label("Close lyrics").click();
    h.run_steps(3);
    assert!(!has_text(&h, "sunlight on the water"));
}

#[test]
fn stop_on_a_busy_server_warns_first() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    core.push(Event::ServerStatus(
        ServerId(0),
        ServerState::Busy {
            job: None,
            since: SystemTime::now(),
        },
    ));
    let mut h = harness(core.clone());
    assert!(disabled(&h, "Launch gpu1"));
    h.get_by_label("Stop gpu1").click();
    h.run_steps(4);
    assert!(core.take().is_empty());
    assert!(has_text(&h, "can't be interrupted"));
    h.get_by_label("Stop when done").click();
    h.run_steps(4);
    assert_eq!(core.take(), vec![Command::StopServer(ServerId(0))]);
    core.push(Event::ServerStatus(
        ServerId(0),
        ServerState::Stopping {
            since: SystemTime::now(),
        },
    ));
    h.run_steps(4);
    assert!(
        disabled(&h, "Launch gpu1"),
        "launch stays disabled while stopping"
    );
}

/// §2.3.1: a core that never answers doesn't stall any panel.
#[test]
fn ui_stays_responsive_when_the_core_never_answers() {
    let core = Arc::new(FakeCore::default());
    core.push(init(Some(preset())));
    let mut h = harness(core.clone());
    type_into(&mut h, "Name", "x");
    let t0 = std::time::Instant::now();
    for tab in [
        "Queue",
        "Projects",
        "Inputs",
        "Audio Library",
        "Review",
        "Log",
        "Generate",
    ] {
        h.get_by_label_contains(tab).click();
        h.run_steps(4);
    }
    assert!(t0.elapsed() < std::time::Duration::from_secs(5));
    // pending transcription is shown, input still works
    core.push(Event::TranscribeStarted("x".into()));
    h.run_steps(4);
    assert!(has_text(&h, "Transcribing"));
    type_into(&mut h, "Starting seed", "3");
    assert_eq!(
        h.get_by_label("Starting seed").value().unwrap_or_default(),
        "3"
    );
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);
impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not read in tests".into())
    }
}

fn drop_file(h: &mut Harness<'_, GuiApp>, path: &str) {
    h.input_mut().dropped_files = vec![Arc::new(Dropped(path.into()))];
    h.run_steps(1);
    h.input_mut().dropped_files.clear();
    h.run_steps(3);
}

#[test]
fn dropping_files_onto_the_generate_panel() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let mut h = harness(core.clone());

    // no name yet: the drop is refused with the same reason as the buttons
    drop_file(&mut h, "/in/tune.abc");
    assert!(h.state().effects_log.is_empty());
    assert!(has_text(&h, "Enter a name first"));

    type_into(&mut h, "Name", "sunny-hook");
    drop_file(&mut h, "/in/tune.abc");
    assert_eq!(
        h.state().effects_log,
        [Effect::ReadAbcFile("/in/tune.abc".into())]
    );

    drop_file(&mut h, "/in/My Demo.mp3");
    let cmds = core.take();
    let [
        Command::Transcribe {
            project,
            audio,
            force: false,
        },
    ] = cmds.as_slice()
    else {
        panic!("{cmds:?}")
    };
    assert_eq!(project.as_str(), "sunny-hook");
    assert_eq!(audio, std::path::Path::new("/in/My Demo.mp3"));
}

#[test]
fn audio_without_a_name_asks_for_one_from_the_file_name() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let mut h = harness(core.clone());

    drop_file(&mut h, "/in/My Demo.mp3");
    assert!(core.take().is_empty(), "nothing sent before a name is chosen");
    assert!(has_text(&h, "Name the project"));
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(!has_text(&h, "Name the project"));
    assert!(core.take().is_empty(), "cancel sends nothing");

    drop_file(&mut h, "/in/My Demo.mp3");
    type_into(&mut h, "Project name", "/");
    assert!(disabled(&h, "Transcribe"), "invalid name");
    h.state_mut().dispatch(UiAction::SetDialogName("My Demo".into()));
    h.run_steps(2);
    h.get_by_label("Transcribe").click();
    h.run_steps(3);
    let cmds = core.take();
    let [Command::Transcribe { project, audio, .. }] = cmds.as_slice() else {
        panic!("{cmds:?}")
    };
    assert_eq!(project.as_str(), "My Demo");
    assert_eq!(audio, std::path::Path::new("/in/My Demo.mp3"));
    assert_eq!(h.state().state.form.name, "My Demo", "the form takes the name");
}

#[test]
fn hovering_files_shows_what_a_drop_would_do() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let mut h = harness(core.clone());
    type_into(&mut h, "Name", "sunny-hook");
    h.input_mut().hovered_files = vec![egui::HoveredFile {
        path: Some("/in/demo.flac".into()),
        ..Default::default()
    }];
    h.run_steps(2);
    let painted = h.output().shapes.iter().any(|s| {
        matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "Drop to transcribe this audio")
    });
    assert!(painted);
    assert!(core.take().is_empty(), "hovering sends nothing");
}

#[test]
fn a_new_form_focuses_the_name_and_enter_moves_to_the_abc_section() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    let mut h = harness(core);
    assert!(h.get_by_label("Name").is_focused());
    h.get_by_label("Name").type_text("sunny-hook");
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert!(h.get_by_label("Load .abc…").is_focused());
}

#[test]
fn missing_model_paths_show_on_the_server() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    core.push(Event::ModelPaths(
        ServerId(0),
        vec!["yue2: /m/yue2.gguf does not exist".into()],
    ));
    let h = harness(core);
    assert!(has_text(&h, "1 model path missing on gpu1"));
}

fn log_line(source: &str, message: &str, output: bool) -> Event {
    Event::Log(audiocpp_core::service::LogLine {
        time: SystemTime::now(),
        level: audiocpp_core::service::LogLevel::Error,
        source: source.into(),
        message: message.into(),
        output,
    })
}

/// The Log tab's toggles show and hide the app's messages and each server's lines.
#[test]
fn log_toggles_sources() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    core.push(log_line("core", "ffmpeg missing", false));
    core.push(log_line("gpu1", "vk: device lost", true));
    let mut h = harness(core.clone());
    h.get_by_label("Log").click();
    h.run_steps(4);
    assert!(has_text(&h, "ffmpeg missing"));
    assert!(has_text(&h, "vk: device lost"));
    assert!(has_text(&h, "gpu1 on port 9123"));
    h.get_by_label("App").click();
    h.run_steps(4);
    assert!(has_text(&h, "vk: device lost"));
    assert!(!has_text(&h, "ffmpeg missing"));
    h.get_by_label("gpu1 (1 error)").click();
    h.run_steps(4);
    // (the status bar still shows gpu1's error as the last one)
    assert!(!has_text(&h, "gpu1 on port 9123"));
    assert!(has_text(&h, "No sources selected."));
    h.get_by_label("All").click();
    h.run_steps(4);
    assert!(has_text(&h, "ffmpeg missing"));
    assert!(has_text(&h, "vk: device lost"));
}

fn project(name: &str, sha: Option<&str>) -> audiocpp_core::project::ProjectSummary {
    audiocpp_core::project::ProjectSummary {
        name: name.into(),
        has_reference: sha.is_some(),
        dir: format!("/lib/inputs/{name}").into(),
        created_at: "2026-09-23T21:40:02Z".into(),
        reference: sha.map(|sha| audiocpp_core::project::Reference {
            original_name: "My Demo.mp3".into(),
            file: "reference.mp3".into(),
            sha256: sha.into(),
            upload_file: None,
            upload_sha256: sha.into(),
            conversion: None,
            format: "mp3".into(),
        }),
        reference_bytes: Some(3 << 20),
        ..Default::default()
    }
}

#[test]
fn projects_tab_renames_and_deletes() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    core.push(Event::Projects(vec![
        project("sunny-hook", Some("aaa")),
        project("rainy", None),
    ]));
    let mut h = harness(core.clone());
    h.get_by_label("Projects (2)").click();
    h.run_steps(4);
    assert!(has_text(&h, "Select a project."));
    h.get_by_label_contains("sunny-hook  ·  0 songs").click();
    h.run_steps(4);
    assert!(has_text(&h, "My Demo.mp3 (mp3, 3.0 MB)"));

    h.get_by_label("Rename to").focus();
    h.run_steps(1);
    h.get_by_label("Rename to").type_text("-2");
    h.run_steps(4);
    h.get_by_label("Rename project").click();
    h.run_steps(4);
    let cmds = core.take();
    assert!(
        matches!(&cmds[..], [Command::RenameProject(from, to)]
            if from == "sunny-hook" && to.as_str() == "sunny-hook-2"),
        "{cmds:?}"
    );
    core.push(Event::Projects(vec![
        project("sunny-hook-2", Some("aaa")),
        project("rainy", None),
    ]));
    h.run_steps(4);

    h.get_by_label("Delete project").click();
    h.run_steps(4);
    h.get_by_label("Move project to trash").click();
    h.run_steps(4);
    assert_eq!(core.take(), [Command::DeleteProject("sunny-hook-2".into())]);

    h.get_by_label("Load into Generate").click();
    h.run_steps(4);
    assert_eq!(core.take(), [Command::LoadProject("sunny-hook-2".into())]);
    assert_eq!(
        h.get_by_label("Name").value().unwrap_or_default(),
        "sunny-hook-2"
    );
}

#[test]
fn inputs_tab_lists_references_and_plays_them() {
    let core = Arc::new(FakeCore::default());
    core.push(init(None));
    core.push(Event::Projects(vec![
        project("a", Some("aaa")),
        project("b", Some("aaa")),
        project("c", None),
    ]));
    let mut h = harness(core.clone());
    h.get_by_label("Inputs (1)").click();
    h.run_steps(4);
    assert!(has_text(&h, "not transcribed"));
    assert!(disabled(&h, "Use My Demo.mp3"), "no name in the form yet");
    h.get_by_label("Play My Demo.mp3").click();
    h.run_steps(4);
    assert_eq!(core.take(), [Command::PlayReference("a".into())]);
    h.get_by_label("Re-transcribe My Demo.mp3").click();
    h.run_steps(4);
    assert!(matches!(&core.take()[..], [Command::Retranscribe(n)] if n.as_str() == "a"));
}
