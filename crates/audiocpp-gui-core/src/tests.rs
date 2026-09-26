//! Reducer tests: the right `Command`s for each `UiAction`, review navigation and form
//! validation (design §9.1).

use super::*;
use audiocpp_core::api::Timing;
use audiocpp_core::config::{ModelSpec, Models};
use audiocpp_core::library::LibraryDelta;
use audiocpp_core::media::{Recipe, RecipeModel, RecipeOutput, RecipeRun, RecipeServer, SongMeta};
use audiocpp_core::run::RunName;
use pretty_assertions::assert_eq;

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

fn init() -> InitInfo {
    InitInfo {
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
        default_preset: None,
        default_preset_path: None,
        opener: Ok("gio launch".into()),
        ffmpeg: Ok("ffmpeg 7".into()),
    }
}

fn state() -> AppState {
    let mut s = AppState::default();
    update(&mut s, Event::Init(init()));
    s
}

fn fill(s: &mut AppState) {
    update(&mut s.clone(), UiAction::SetName("x".into()));
    update(s, UiAction::SetName("sunny".into()));
    update(s, UiAction::SetSeed("100".into()));
    update(s, UiAction::SetCount("3".into()));
    let mut p = s.form.params.clone();
    p.lyrics = "la".into();
    p.style = "pop".into();
    update(s, UiAction::SetParams(p));
    update(s, UiAction::SetAbcText("X:1\nK:C\n".into()));
}

fn song(name: &str, seed: u32, loc: Location, created: &str) -> Song {
    let recipe = Recipe {
        schema: 1,
        app_version: "0".into(),
        song_id: format!("{name}-{seed}-id"),
        run: RecipeRun {
            id: RunId::new(),
            name: name.into(),
            seed,
            start_seed: seed,
            index: 0,
            revision: 2,
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
        request: serde_json::json!({"model":"yue2","request":{"lyrics":"old lyrics","seed":seed,"options":{"style":"jazz","abc":"X:9\nK:D\n"}}}),
        project: name.into(),
        abc_source: AbcSource::Manual,
        timing: Timing::default(),
        output: RecipeOutput {
            sample_rate: 48000,
            channels: 2,
            wav_sha256: String::new(),
            encoder: "mp3 V0".into(),
        },
    };
    let stem = format!("{name}-{seed}");
    Song {
        id: SongId(format!("{stem}-id")),
        stem: stem.clone(),
        dir: Some(format!("/lib/x/{stem}").into()),
        mp3: format!("/lib/x/{stem}/{stem}.mp3").into(),
        wav: None,
        location: loc,
        meta: SongMeta {
            title: Some(stem),
            recipe: Some(recipe),
            tags: vec!["t1".into()],
            ..Default::default()
        },
        wav_ok: None,
        modified: SystemTime::UNIX_EPOCH,
        size: 1,
    }
}

fn with_songs(s: &mut AppState, songs: Vec<Song>) {
    update(
        s,
        Event::LibraryChanged(LibraryDelta {
            full: true,
            upserted: songs,
            removed: vec![],
        }),
    );
}

#[test]
fn start_run_disabled_until_name_and_seed_valid() {
    let mut s = state();
    assert!(matches!(s.start_blocker(), Some(Blocker::Name(_))));
    assert!(update(&mut s, UiAction::StartRun).is_empty());
    fill(&mut s);
    update(&mut s, UiAction::SetSeed("".into()));
    assert!(matches!(s.start_blocker(), Some(Blocker::Seed(_))));
    update(&mut s, UiAction::SetSeed("100".into()));
    update(&mut s, UiAction::SetName("bad/name".into()));
    assert!(matches!(s.start_blocker(), Some(Blocker::Name(_))));
    update(&mut s, UiAction::SetName("sunny".into()));
    assert_eq!(s.start_blocker(), None);
}

#[test]
fn start_run_sends_spec_and_advances_seed() {
    let mut s = state();
    fill(&mut s);
    let cmds = update(&mut s, UiAction::StartRun);
    let [Command::StartRun(spec)] = cmds.as_slice() else {
        panic!("{cmds:?}")
    };
    assert_eq!(spec.name, RunName::parse("sunny").unwrap());
    assert_eq!((spec.start_seed, spec.count), (100, Some(3)));
    assert_eq!(spec.params.abc.as_deref(), Some("X:1\nK:C\n"));
    assert_eq!(spec.params.lyrics, "la");
    assert_eq!(spec.model.id, "yue2");
    assert_eq!(s.form.name, "sunny", "name is kept after a run starts");
    assert_eq!(s.form.seed, "103", "seed advances by the run's count");
    assert_eq!(s.form.params.lyrics, "la", "everything else is kept");
}

#[test]
fn a_new_form_starts_at_seed_zero() {
    let mut s = state();
    assert_eq!(s.form.seed, "0");
    update(&mut s, UiAction::SetName("sunny".into()));
    assert!(!matches!(s.start_blocker(), Some(Blocker::Seed(_))));
}

#[test]
fn random_seed_is_visible_and_editable() {
    let mut s = state();
    update(&mut s, UiAction::RandomSeed(4_000_000_000));
    assert_eq!(s.form.seed, "4000000000");
}

#[test]
fn collision_offers_continue() {
    let mut s = state();
    with_songs(
        &mut s,
        vec![
            song("sunny", 101, Location::Unreviewed, "a"),
            song("sunny", 150, Location::Reviewed(Rating::Good), "b"),
        ],
    );
    fill(&mut s);
    match s.start_blocker() {
        Some(Blocker::Collision(c)) => {
            assert_eq!(c.seeds, vec![101]);
            assert_eq!(c.continue_from, Some(151));
            update(&mut s, UiAction::ContinueFromSeed(151));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(s.start_blocker(), None);
}

#[test]
fn no_abc_confirmation_flow() {
    let mut s = state();
    fill(&mut s);
    update(&mut s, UiAction::SetAbcText("".into()));
    assert!(update(&mut s, UiAction::StartRun).is_empty());
    assert_eq!(s.dialog, Some(Dialog::NoAbc { dont_ask: false }));
    update(&mut s, UiAction::SetDontAsk(true));
    let cmds = update(&mut s, UiAction::ConfirmDialog);
    let [Command::StartRun(spec)] = cmds.as_slice() else {
        panic!("{cmds:?}")
    };
    assert_eq!(spec.params.abc, None);
    assert_eq!(spec.abc_source, AbcSource::None);
    assert!(
        spec.params.to_request(1)["options"].get("abc").is_none(),
        "abc field left out entirely"
    );
    assert!(s.form.allow_no_abc);
    // now it doesn't ask again
    update(&mut s, UiAction::SetName("again".into()));
    assert!(matches!(
        update(&mut s, UiAction::StartRun).as_slice(),
        [Command::StartRun(_)]
    ));
}

#[test]
fn choosing_none_on_purpose_skips_the_dialog() {
    let mut s = state();
    fill(&mut s);
    update(&mut s, UiAction::SetAbcChoice(AbcChoice::None));
    let cmds = update(&mut s, UiAction::StartRun);
    assert!(s.dialog.is_none());
    let [Command::StartRun(spec)] = cmds.as_slice() else {
        panic!()
    };
    assert_eq!(spec.params.abc, None);
}

#[test]
fn preset_with_allow_no_abc_skips_the_dialog_and_dont_ask_saves_it() {
    let mut s = state();
    let preset = Preset {
        allow_no_abc: true,
        ..Default::default()
    };
    update(&mut s, Event::PresetLoaded(Ok(("/p.ron".into(), preset))));
    fill(&mut s);
    update(&mut s, UiAction::SetAbcText("".into()));
    assert!(matches!(
        update(&mut s, UiAction::StartRun).as_slice(),
        [Command::StartRun(_)]
    ));

    let mut s = state();
    update(
        &mut s,
        Event::PresetLoaded(Ok(("/p.ron".into(), Preset::default()))),
    );
    fill(&mut s);
    update(&mut s, UiAction::SetAbcText("".into()));
    update(&mut s, UiAction::StartRun);
    update(&mut s, UiAction::SetDontAsk(true));
    let cmds = update(&mut s, UiAction::ConfirmDialog);
    assert!(
        matches!(&cmds[0], Command::SavePreset(p, pr) if p == &PathBuf::from("/p.ron") && pr.allow_no_abc)
    );
    assert!(matches!(&cmds[1], Command::StartRun(_)));
}

#[test]
fn inputs_need_a_name_first() {
    let mut s = state();
    assert!(s.inputs_blocker().is_some());
    update(
        &mut s,
        UiAction::AbcFileLoaded {
            file_name: "m.abc".into(),
            contents: "X:1".into(),
        },
    );
    assert_eq!(s.form.abc_text, "", "ignored without a name");
    update(&mut s, UiAction::SetName("n".into()));
    update(
        &mut s,
        UiAction::AbcFileLoaded {
            file_name: "m.abc".into(),
            contents: "X:1\nK:C".into(),
        },
    );
    assert!(
        matches!(&s.form.abc_source, AbcSource::File { file_name, .. } if file_name == "m.abc")
    );
    assert_eq!(s.form.abc_summary.key.as_deref(), Some("C"));
    let cmds = update(&mut s, UiAction::TranscribeFile("/a.mp3".into()));
    assert_eq!(
        cmds,
        vec![Command::Transcribe {
            project: RunName::parse("n").unwrap(),
            audio: "/a.mp3".into(),
            force: false
        }]
    );
}

#[test]
fn transcribing_without_a_name_suggests_the_file_name() {
    let mut s = state();
    assert_eq!(
        update(&mut s, UiAction::TranscribeFile("/in/Take: 3.flac".into())),
        vec![]
    );
    assert_eq!(
        s.dialog,
        Some(Dialog::NameProject {
            audio: "/in/Take: 3.flac".into(),
            name: "Take_ 3".into()
        }),
        "forbidden characters replaced"
    );
    update(&mut s, UiAction::CancelDialog);
    assert_eq!(s.dialog, None);
    assert_eq!(s.form.name, "", "cancel leaves the form alone");

    update(&mut s, UiAction::TranscribeFile("/in/demo.mp3".into()));
    update(&mut s, UiAction::SetDialogName("a/b".into()));
    assert_eq!(update(&mut s, UiAction::ConfirmDialog), vec![]);
    assert!(s.dialog.is_some(), "an invalid name keeps the dialog open");
    update(&mut s, UiAction::SetDialogName(" demo ".into()));
    assert_eq!(
        update(&mut s, UiAction::ConfirmDialog),
        vec![Command::Transcribe {
            project: RunName::parse("demo").unwrap(),
            audio: "/in/demo.mp3".into(),
            force: false
        }]
    );
    assert_eq!(s.form.name, "demo");
    assert_eq!(s.dialog, None);
}

#[test]
fn new_reference_in_existing_project_asks_first() {
    let mut s = state();
    update(
        &mut s,
        Event::Projects(vec![ProjectSummary {
            name: "n".into(),
            has_reference: true,
            ..Default::default()
        }]),
    );
    update(&mut s, UiAction::SetName("n".into()));
    assert!(s.offers_project());
    assert!(update(&mut s, UiAction::TranscribeFile("/b.flac".into())).is_empty());
    assert!(matches!(s.dialog, Some(Dialog::ReplaceReference { .. })));
    let cmds = update(&mut s, UiAction::ConfirmDialog);
    assert!(matches!(&cmds[..], [Command::Transcribe { .. }]));
}

#[test]
fn transcription_result_fills_the_editor() {
    let mut s = state();
    update(&mut s, UiAction::SetName("n".into()));
    let reference = audiocpp_core::run::ReferenceAudio {
        file_name: "a.mp3".into(),
        format: "mp3".into(),
        sha256: "x".into(),
        upload_sha256: "y".into(),
        abc_model: "sheetsage2".into(),
    };
    update(
        &mut s,
        Event::TranscribeDone(Ok(audiocpp_core::transcribe::AbcScore {
            project: "n".into(),
            abc: "X:1\nK:Am\n".into(),
            reference: reference.clone(),
            reused: true,
            wall_ms: None,
        })),
    );
    assert_eq!(s.form.abc_text, "X:1\nK:Am\n");
    assert_eq!(s.form.abc_source, AbcSource::Transcribed(reference));
    assert!(s.transcribe.reused);
}

#[test]
fn load_project_inputs_fills_editors_and_next_seed() {
    let mut s = state();
    with_songs(&mut s, vec![song("p", 7, Location::Unreviewed, "a")]);
    update(
        &mut s,
        Event::Projects(vec![ProjectSummary {
            name: "p".into(),
            has_reference: false,
            ..Default::default()
        }]),
    );
    update(&mut s, UiAction::SetName("p".into()));
    update(&mut s, UiAction::SetSeed("7".into()));
    assert_eq!(
        update(&mut s, UiAction::LoadProjectInputs),
        vec![Command::LoadProject("p".into())]
    );
    update(
        &mut s,
        Event::ProjectInputs(Ok(audiocpp_core::project::ProjectInputs {
            name: "p".into(),
            abc: Some("X:1\nK:E\n".into()),
            lyrics: Some("ly".into()),
            style: Some("st".into()),
            abc_source: Some(AbcSource::Manual),
            has_reference: false,
        })),
    );
    assert_eq!(
        (s.form.params.lyrics.as_str(), s.form.params.style.as_str()),
        ("ly", "st")
    );
    assert_eq!(s.form.abc_text, "X:1\nK:E\n");
    assert_eq!(s.form.seed, "8", "collision → next free seed");
}

#[test]
fn continue_run_fills_recipe_name_and_next_seed() {
    let mut s = state();
    let a = song("sunny", 5, Location::Unreviewed, "a");
    let id = a.id.clone();
    with_songs(
        &mut s,
        vec![a, song("sunny", 9, Location::Reviewed(Rating::Bad), "b")],
    );
    update(&mut s, UiAction::ContinueRun(id));
    assert_eq!(s.form.name, "sunny");
    assert_eq!(s.form.seed, "10");
    assert_eq!(s.form.params.lyrics, "old lyrics");
    assert_eq!(s.form.params.style, "jazz");
    assert_eq!(s.form.abc_text, "X:9\nK:D\n");
    assert_eq!(s.tab, Tab::Generate);
}

#[test]
fn queue_editor_sends_edit_run() {
    let mut s = state();
    fill(&mut s);
    let Command::StartRun(spec) = update(&mut s, UiAction::StartRun).remove(0) else {
        panic!()
    };
    let id = RunId::new();
    let snap = RunSnapshot {
        state: audiocpp_core::run::RunState::new(id, spec),
        position: 0,
        done: 0,
        failed: 0,
        running: 1,
        queued_retries: 0,
    };
    update(&mut s, Event::RunUpdate(snap));
    let mut ed = s.editors[&id].clone();
    assert_eq!(ed.abc_text, "X:1\nK:C\n");
    ed.params.style = "rock".into();
    ed.count = "5".into();
    ed.next_seed = "200".into();
    update(&mut s, UiAction::SetRunEditor(id, ed));
    let cmds = update(&mut s, UiAction::ApplyRunParams(id));
    let [Command::EditRun(rid, RunEdit::Params(p))] = cmds.as_slice() else {
        panic!("{cmds:?}")
    };
    assert_eq!(*rid, id);
    assert_eq!(p.style, "rock");
    assert_eq!(p.abc.as_deref(), Some("X:1\nK:C\n"));
    assert_eq!(
        update(&mut s, UiAction::ApplyRunCount(id)),
        vec![Command::EditRun(id, RunEdit::Count(Some(5)))]
    );
    assert_eq!(
        update(&mut s, UiAction::ApplyRunNextSeed(id)),
        vec![Command::EditRun(id, RunEdit::NextSeed(200))]
    );
    // collision check on next seed
    with_songs(&mut s, vec![song("sunny", 201, Location::Unreviewed, "a")]);
    assert!(update(&mut s, UiAction::ApplyRunNextSeed(id)).is_empty());
    assert!(s.editor_collision(id).is_some());
}

fn queued(s: &mut AppState, name: &str, start: u32, next: u32, count: Option<u32>) -> RunId {
    let spec = audiocpp_core::run::RunSpec {
        name: RunName::parse(name).unwrap(),
        params: GenerationParams {
            style: "rock".into(),
            abc: Some("X:1\nK:G\nG".into()),
            ..Default::default()
        },
        start_seed: start,
        count,
        model: model("yue2"),
        abc_source: AbcSource::Manual,
    };
    let id = RunId::new();
    let mut state = audiocpp_core::run::RunState::new(id, spec);
    state.next_seed = next;
    update(
        s,
        Event::RunUpdate(RunSnapshot {
            state,
            position: 0,
            done: 0,
            failed: 0,
            running: 1,
            queued_retries: 0,
        }),
    );
    id
}

#[test]
fn a_queued_run_loads_into_generate_continuing_its_seeds() {
    let mut s = state();
    let id = queued(&mut s, "live", 40, 43, None);
    update(&mut s, UiAction::SelectTab(Tab::Queue));
    assert_eq!(update(&mut s, UiAction::LoadIntoForm(FormLoad::Queue(id))), vec![]);
    assert_eq!(s.tab, Tab::Generate);
    assert_eq!(s.focus, Some(Focus::Name));
    assert_eq!(s.form.name, "live");
    assert_eq!(s.form.params.style, "rock");
    assert_eq!(s.form.abc_text, "X:1\nK:G\nG");
    assert_eq!(s.form.abc_choice, AbcChoice::Paste);
    assert_eq!(s.form.seed, "43", "after the seeds the run already handed out");
    assert!(s.form.until_stopped);

    // the library's next free seed wins when it's higher
    with_songs(&mut s, vec![song("live", 90, Location::Unreviewed, "a")]);
    s.form.name.clear();
    update(&mut s, UiAction::LoadIntoForm(FormLoad::Queue(id)));
    assert_eq!(s.form.seed, "91");
}

#[test]
fn loading_over_a_named_form_asks_first() {
    let mut s = state();
    let id = queued(&mut s, "live", 1, 2, Some(4));
    update(&mut s, UiAction::SetName("draft".into()));
    update(&mut s, UiAction::LoadIntoForm(FormLoad::Queue(id)));
    assert_eq!(s.dialog, Some(Dialog::ReplaceForm(FormLoad::Queue(id))));
    update(&mut s, UiAction::CancelDialog);
    assert_eq!(s.form.name, "draft", "cancel keeps the form");
    update(&mut s, UiAction::LoadIntoForm(FormLoad::Queue(id)));
    update(&mut s, UiAction::ConfirmDialog);
    assert_eq!(s.form.name, "live");
    assert_eq!((s.form.count.as_str(), s.form.until_stopped), ("4", false));
}

#[test]
fn history_tab_lists_runs_and_loads_a_revision() {
    use audiocpp_core::history::{RecordStatus, Revision, RunRecord};
    let mut s = state();
    assert_eq!(
        update(&mut s, UiAction::SelectTab(Tab::History)),
        vec![Command::ListRuns]
    );
    let id = RunId::new();
    let rev = |n: u32, style: &str, first_seed: u32| Revision {
        revision: n,
        at: "2026-09-25T10:00:00Z".into(),
        first_seed,
        params: GenerationParams {
            style: style.into(),
            ..Default::default()
        },
        abc_source: AbcSource::None,
    };
    let record = RunRecord {
        schema: 1,
        app_version: "0".into(),
        id,
        name: RunName::parse("old").unwrap(),
        created_at: "2026-09-25T10:00:00Z".into(),
        finished_at: None,
        status: RecordStatus::Interrupted,
        start_seed: 5,
        count: Some(10),
        next_seed: 8,
        issued: 3,
        model: ModelSpec {
            id: "other-model".into(),
            ..model("yue2")
        },
        regenerate_of: None,
        resumed_from: None,
        revisions: vec![rev(0, "folk", 5), rev(1, "punk", 7)],
        seeds: vec![],
    };
    update(&mut s, Event::RunHistory(vec![record.summary()]));
    assert_eq!(s.history.filtered().len(), 1);
    update(&mut s, UiAction::SetHistorySearch("nope".into()));
    assert!(s.history.filtered().is_empty());
    assert_eq!(
        update(&mut s, UiAction::SelectHistoryRun(id)),
        vec![Command::LoadRunRecord(id)]
    );
    update(&mut s, Event::RunRecord(id, Ok(record)));
    update(&mut s, UiAction::LoadIntoForm(FormLoad::Record(Some(0))));
    assert_eq!(s.form.params.style, "folk");
    assert_eq!(s.form.abc_choice, AbcChoice::None);
    assert_eq!((s.form.seed.as_str(), s.form.count.as_str()), ("8", "10"));
    assert!(
        s.status.as_deref().unwrap().contains("other-model"),
        "a different model is pointed out"
    );
    s.form.name.clear();
    update(&mut s, UiAction::LoadIntoForm(FormLoad::Record(None)));
    assert_eq!(s.form.params.style, "punk", "latest revision by default");
    assert_eq!(
        update(&mut s, UiAction::ResumeInterrupted(id)),
        vec![Command::ResumeInterrupted(id)]
    );
}

#[test]
fn only_inactive_runs_can_be_removed() {
    let mut s = state();
    fill(&mut s);
    let Command::StartRun(spec) = update(&mut s, UiAction::StartRun).remove(0) else {
        panic!()
    };
    let id = RunId::new();
    let mut snap = RunSnapshot {
        state: audiocpp_core::run::RunState::new(id, spec),
        position: 0,
        done: 0,
        failed: 0,
        running: 0,
        queued_retries: 0,
    };
    update(&mut s, Event::RunUpdate(snap.clone()));
    assert!(update(&mut s, UiAction::RemoveRun(id)).is_empty());
    snap.state.status = RunStatus::Paused;
    update(&mut s, Event::RunUpdate(snap.clone()));
    assert_eq!(
        update(&mut s, UiAction::RemoveRun(id)),
        vec![Command::RemoveRun(id)]
    );
    assert!(s.runs.is_empty() && !s.editors.contains_key(&id));
    // a late update resurrects it until the core confirms the removal
    update(&mut s, Event::RunUpdate(snap));
    update(&mut s, Event::RunRemoved(id));
    assert!(s.runs.is_empty());
}

#[test]
fn job_updates_are_tracked() {
    let mut s = state();
    let info = JobInfo {
        id: JobId(1),
        run_id: RunId::new(),
        seed: 5,
        revision: 0,
        attempts: 0,
        state: JobState::Running {
            server: ServerId(0),
            started: SystemTime::now(),
            estimate_ms: Some(180_000),
        },
        timing: None,
    };
    update(&mut s, Event::JobUpdate(JobId(1), info));
    assert!(job_label(&s.jobs[&JobId(1)].state, SystemTime::now()).contains("ETA ~3 min"));
    let mut done = s.jobs[&JobId(1)].clone();
    done.state = JobState::Done(SongId("x".into()));
    update(&mut s, Event::JobUpdate(JobId(1), done));
    assert_eq!(
        job_label(&s.jobs[&JobId(1)].state, SystemTime::now()),
        "done"
    );
}

#[test]
fn stopping_a_busy_server_warns_first() {
    let mut s = state();
    update(
        &mut s,
        Event::ServerStatus(
            ServerId(0),
            ServerState::Ready {
                backend: None,
                loaded_models: vec![],
            },
        ),
    );
    assert_eq!(
        update(&mut s, UiAction::StopServer(ServerId(0))),
        vec![Command::StopServer(ServerId(0))]
    );
    update(
        &mut s,
        Event::ServerStatus(
            ServerId(0),
            ServerState::Busy {
                job: Some(JobId(3)),
                since: SystemTime::now(),
            },
        ),
    );
    assert!(update(&mut s, UiAction::StopServer(ServerId(0))).is_empty());
    assert!(matches!(s.dialog, Some(Dialog::StopBusy { .. })));
    assert_eq!(
        update(&mut s, UiAction::ConfirmDialog),
        vec![Command::StopServer(ServerId(0))]
    );
    // launch is disabled while stopping
    update(
        &mut s,
        Event::ServerStatus(
            ServerId(0),
            ServerState::Stopping {
                since: SystemTime::now(),
            },
        ),
    );
    assert!(update(&mut s, UiAction::LaunchServer(ServerId(0))).is_empty());
    update(
        &mut s,
        Event::ServerStatus(ServerId(0), ServerState::Stopped),
    );
    assert_eq!(
        update(&mut s, UiAction::LaunchServer(ServerId(0))),
        vec![Command::LaunchServer(ServerId(0))]
    );
}

#[test]
fn review_mode_rate_then_next() {
    let mut s = state();
    let a = song("r", 1, Location::Unreviewed, "2026-01-01");
    let b = song("r", 2, Location::Unreviewed, "2026-01-02");
    let c = song("r", 3, Location::Reviewed(Rating::Good), "2026-01-03");
    let (ia, ib) = (a.id.clone(), b.id.clone());
    with_songs(&mut s, vec![b, c, a]);
    let cmds = update(&mut s, UiAction::EnterReview);
    assert_eq!(
        cmds,
        vec![Command::Play(ia.clone())],
        "oldest first, plays automatically"
    );
    let cmds = update(&mut s, UiAction::ReviewKey(ReviewKey::Rate(Rating::Good)));
    assert_eq!(
        cmds,
        vec![
            Command::Library(LibraryCommand::Rate(ia.clone(), Rating::Good)),
            Command::Play(ib.clone())
        ]
    );
    assert_eq!(
        update(&mut s, UiAction::ReviewKey(ReviewKey::Prev)),
        vec![Command::Play(ia)]
    );
    assert_eq!(
        update(&mut s, UiAction::ReviewKey(ReviewKey::Next)),
        vec![Command::Play(ib.clone())]
    );
    assert_eq!(
        update(
            &mut s,
            UiAction::ReviewKey(ReviewKey::Forward { big: true })
        ),
        vec![Command::SeekBy(30.0)]
    );
    assert_eq!(
        update(&mut s, UiAction::ReviewKey(ReviewKey::Back { big: false })),
        vec![Command::SeekBy(-5.0)]
    );
    update(&mut s, Event::PlaybackState {
        state: PlayState::Playing,
        song: Some(ib.clone()),
        duration: Duration::from_secs(60),
    });
    assert_eq!(
        update(&mut s, UiAction::ReviewKey(ReviewKey::PlayPause)),
        vec![Command::TogglePause]
    );
    update(&mut s, UiAction::ReviewKey(ReviewKey::Tag));
    assert_eq!(s.focus, Some(Focus::Tag));
    assert_eq!(
        update(&mut s, UiAction::ReviewKey(ReviewKey::Next)),
        vec![Command::Stop],
        "end of queue"
    );
}

#[test]
fn review_lyrics_popup_toggles_and_follows_the_song() {
    let mut s = state();
    let a = song("r", 1, Location::Unreviewed, "2026-01-01");
    let b = song("r", 2, Location::Unreviewed, "2026-01-02");
    let ib = b.id.clone();
    with_songs(&mut s, vec![a, b]);
    update(&mut s, UiAction::EnterReview);
    assert_eq!(update(&mut s, UiAction::ReviewKey(ReviewKey::Lyrics)), vec![]);
    assert!(s.review.lyrics);
    update(&mut s, UiAction::ReviewKey(ReviewKey::Rate(Rating::Good)));
    assert!(s.review.lyrics, "stays open for the next song");
    assert_eq!(s.review_song(), Some(&ib));
    update(&mut s, UiAction::EnterReview);
    assert!(s.review.lyrics, "restart keeps it open");
    update(&mut s, UiAction::ReviewKey(ReviewKey::Lyrics));
    assert!(!s.review.lyrics, "L toggles");
    update(&mut s, UiAction::ReviewKey(ReviewKey::Lyrics));
    update(&mut s, UiAction::CloseLyrics);
    assert!(!s.review.lyrics);
    update(&mut s, UiAction::ReviewKey(ReviewKey::Lyrics));
    update(&mut s, UiAction::ReviewKey(ReviewKey::Tag));
    assert!(!s.review.lyrics, "T closes it to reach the tag field");
    assert_eq!(s.focus, Some(Focus::Tag));
}

#[test]
fn returning_to_review_resumes_instead_of_restarting() {
    let mut s = state();
    let a = song("r", 1, Location::Unreviewed, "2026-01-01");
    let b = song("r", 2, Location::Unreviewed, "2026-01-02");
    let c = song("r", 3, Location::Unreviewed, "2026-01-03");
    let (ia, ib, ic) = (a.id.clone(), b.id.clone(), c.id.clone());
    with_songs(&mut s, vec![a, b.clone(), c]);
    let playing = |s: &mut AppState, id: &SongId| {
        update(s, Event::PlaybackState {
            state: PlayState::Playing,
            song: Some(id.clone()),
            duration: Duration::from_secs(60),
        });
    };
    assert_eq!(
        update(&mut s, UiAction::SelectTab(Tab::Review)),
        vec![Command::Play(ia.clone())],
        "first visit starts a review"
    );
    update(&mut s, UiAction::ReviewKey(ReviewKey::Next));
    playing(&mut s, &ib);

    // away and back while the review song plays: no restart, same place
    update(&mut s, UiAction::SelectTab(Tab::Library));
    update(&mut s, UiAction::SelectSong(ia.clone()));
    assert!(update(&mut s, UiAction::SelectTab(Tab::Review)).is_empty());
    assert_eq!(s.review_song(), Some(&ib));
    assert_eq!(s.current.as_ref(), Some(&ib));

    // another tab's song is playing: leave it; Space switches to the review song
    update(&mut s, UiAction::SelectTab(Tab::Library));
    update(&mut s, UiAction::Play(ia.clone()));
    playing(&mut s, &ia);
    assert!(update(&mut s, UiAction::SelectTab(Tab::Review)).is_empty());
    assert_eq!(
        update(&mut s, UiAction::ReviewKey(ReviewKey::PlayPause)),
        vec![Command::Play(ib.clone())]
    );
    playing(&mut s, &ib);

    // current song rated elsewhere: skip to the next unreviewed one
    update(&mut s, UiAction::SelectTab(Tab::Library));
    let mut rated = b;
    rated.location = Location::Reviewed(Rating::Good);
    update(
        &mut s,
        Event::LibraryChanged(LibraryDelta {
            full: false,
            upserted: vec![rated],
            removed: vec![],
        }),
    );
    update(&mut s, Event::PlaybackState {
        state: PlayState::Stopped,
        song: None,
        duration: Duration::ZERO,
    });
    assert_eq!(
        update(&mut s, UiAction::SelectTab(Tab::Review)),
        vec![Command::Play(ic)]
    );
    assert_eq!(s.review.index, 2, "earlier songs stay reachable with Prev");

    // the restart button still resets
    assert_eq!(
        update(&mut s, UiAction::EnterReview),
        vec![Command::Play(ia)]
    );
}

#[test]
fn global_pause_resume_works_from_any_tab_and_remembers_the_source() {
    let mut s = state();
    let a = song("r", 1, Location::Reviewed(Rating::Good), "2026-01-01");
    let b = song("r", 2, Location::Reviewed(Rating::Good), "2026-01-02");
    let (ia, ib) = (a.id.clone(), b.id.clone());
    with_songs(&mut s, vec![a, b]);
    assert_eq!(update(&mut s, UiAction::PauseResume), vec![], "nothing loaded");
    update(&mut s, UiAction::SelectTab(Tab::Library));
    update(&mut s, UiAction::Play(ia.clone()));
    assert_eq!(s.player.tab, Some(Tab::Library));
    update(&mut s, Event::PlaybackState {
        state: PlayState::Playing,
        song: Some(ia),
        duration: Duration::from_secs(60),
    });
    update(&mut s, UiAction::SelectSong(ib));
    update(&mut s, UiAction::SelectTab(Tab::Log));
    assert_eq!(
        update(&mut s, UiAction::PauseResume),
        vec![Command::TogglePause],
        "pauses the loaded song even though another is selected"
    );
    assert_eq!(s.player.tab, Some(Tab::Library));
    update(&mut s, UiAction::PlayReference("proj".into()));
    assert_eq!(s.player.tab, Some(Tab::Inputs));
}

#[test]
fn library_filters() {
    let mut s = state();
    let mut a = song("alpha", 1, Location::Unreviewed, "1");
    a.meta.tags = vec!["keeper".into()];
    let mut b = song("beta", 20, Location::Reviewed(Rating::Good), "2");
    b.meta.recipe.as_mut().unwrap().request = serde_json::json!({"request":{"options":{}}});
    with_songs(&mut s, vec![a, b]);
    let names = |s: &AppState| {
        s.filtered_songs()
            .iter()
            .map(|x| x.stem.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&s), vec!["alpha-1", "beta-20"], "oldest first");
    let f = |f: LibraryFilter| {
        let mut s = s.clone();
        update(&mut s, UiAction::SetFilter(f));
        names(&s)
    };
    assert_eq!(
        f(LibraryFilter {
            folder: FolderFilter::Unreviewed,
            ..Default::default()
        }),
        vec!["alpha-1"]
    );
    assert_eq!(
        f(LibraryFilter {
            folder: FolderFilter::Rated(Rating::Good),
            ..Default::default()
        }),
        vec!["beta-20"]
    );
    assert_eq!(
        f(LibraryFilter {
            tag: "KEEPER".into(),
            ..Default::default()
        }),
        vec!["alpha-1"]
    );
    assert_eq!(
        f(LibraryFilter {
            text: "bet".into(),
            ..Default::default()
        }),
        vec!["beta-20"]
    );
    assert_eq!(
        f(LibraryFilter {
            run_name: "alp".into(),
            ..Default::default()
        }),
        vec!["alpha-1"]
    );
    assert_eq!(
        f(LibraryFilter {
            seed_min: "10".into(),
            seed_max: "30".into(),
            ..Default::default()
        }),
        vec!["beta-20"]
    );
    assert_eq!(
        f(LibraryFilter {
            revision: "2".into(),
            ..Default::default()
        })
        .len(),
        2
    );
    assert_eq!(
        f(LibraryFilter {
            only_no_abc: true,
            ..Default::default()
        }),
        vec!["beta-20"]
    );
}

#[test]
fn metadata_actions() {
    let mut s = state();
    let a = song("m", 1, Location::Unreviewed, "1");
    let id = a.id.clone();
    with_songs(&mut s, vec![a]);
    update(&mut s, UiAction::SelectSong(id.clone()));
    assert_eq!(s.meta.title, "m-1");
    assert_eq!(s.meta.rename, "m");
    assert!(
        update(&mut s, UiAction::CommitTitle).is_empty(),
        "unchanged"
    );
    update(
        &mut s,
        UiAction::SetMeta(MetaEditor {
            title: "Nice".into(),
            rename: "new name".into(),
            notes: "n".into(),
            new_tag: String::new(),
        }),
    );
    assert_eq!(
        update(&mut s, UiAction::CommitTitle),
        vec![Command::Library(LibraryCommand::SetTitle(
            id.clone(),
            "Nice".into()
        ))]
    );
    assert_eq!(
        update(&mut s, UiAction::CommitRename),
        vec![Command::Library(LibraryCommand::Rename(
            id.clone(),
            "new name".into()
        ))]
    );
    assert_eq!(
        update(&mut s, UiAction::CommitNotes),
        vec![Command::Library(LibraryCommand::SetNotes(
            id.clone(),
            "n".into()
        ))]
    );
    assert_eq!(
        update(&mut s, UiAction::AddTag("fav".into())),
        vec![Command::Library(LibraryCommand::SetTags(
            id.clone(),
            vec!["t1".into(), "fav".into()]
        ))]
    );
    assert_eq!(
        update(&mut s, UiAction::RemoveTag("t1".into())),
        vec![Command::Library(LibraryCommand::SetTags(
            id.clone(),
            vec![]
        ))]
    );
    assert_eq!(
        update(&mut s, UiAction::Rate(Rating::Bad)),
        vec![Command::Library(LibraryCommand::Rate(
            id.clone(),
            Rating::Bad
        ))]
    );
    assert_eq!(
        update(&mut s, UiAction::Undo),
        vec![Command::Library(LibraryCommand::Undo)]
    );
    let m = MetaEditor {
        rename: "bad/name".into(),
        ..s.meta.clone()
    };
    update(&mut s, UiAction::SetMeta(m));
    assert!(update(&mut s, UiAction::CommitRename).is_empty());
    assert!(s.status.as_deref().unwrap().starts_with("Rename"));
    assert!(update(&mut s, UiAction::Delete).is_empty());
    assert_eq!(
        update(&mut s, UiAction::ConfirmDialog),
        vec![Command::Library(LibraryCommand::Delete(id))]
    );
}

#[test]
fn exit_asks_about_launched_servers() {
    let mut s = state();
    assert_eq!(
        update(&mut s, UiAction::RequestExit),
        vec![Command::Shutdown {
            stop_launched: false
        }]
    );
    let mut s = state();
    update(
        &mut s,
        Event::ServerStatus(
            ServerId(0),
            ServerState::Ready {
                backend: None,
                loaded_models: vec![],
            },
        ),
    );
    assert!(update(&mut s, UiAction::RequestExit).is_empty());
    assert_eq!(s.dialog, Some(Dialog::Exit));
    // asking again (e.g. closing the window twice) leaves them running — the default
    assert_eq!(
        update(&mut s, UiAction::RequestExit),
        vec![Command::Shutdown {
            stop_launched: false
        }]
    );
    let mut s2 = state();
    update(
        &mut s2,
        Event::ServerStatus(
            ServerId(0),
            ServerState::Ready {
                backend: None,
                loaded_models: vec![],
            },
        ),
    );
    update(&mut s2, UiAction::RequestExit);
    assert_eq!(
        update(&mut s2, UiAction::ConfirmDialog),
        vec![Command::Shutdown {
            stop_launched: true
        }]
    );
}

#[test]
fn a_new_form_focuses_the_name_then_the_abc_section() {
    let mut s = state();
    assert!(s.form.abc_section_open);
    assert_eq!(s.focus, Some(Focus::Name));
    update(&mut s, UiAction::FocusHandled);

    // Enter on an invalid name keeps focus where it is
    update(&mut s, UiAction::SetName("bad/name".into()));
    update(&mut s, UiAction::SubmitName);
    assert_eq!(s.focus, None);

    update(&mut s, UiAction::SetAbcSectionOpen(false));
    update(&mut s, UiAction::SetName("sunny".into()));
    update(&mut s, UiAction::SubmitName);
    assert_eq!(s.focus, Some(Focus::AbcSection));
    assert!(s.form.abc_section_open);

    // once there is an ABC (or None was chosen), Enter doesn't jump anywhere
    update(&mut s, UiAction::FocusHandled);
    update(&mut s, UiAction::SetAbcText("X:1\nK:C\n".into()));
    update(&mut s, UiAction::SubmitName);
    assert_eq!(s.focus, None);
    update(&mut s, UiAction::SetAbcText(String::new()));
    update(&mut s, UiAction::SetAbcChoice(AbcChoice::None));
    update(&mut s, UiAction::SubmitName);
    assert_eq!(s.focus, None);
}

#[test]
fn dropped_files_follow_the_input_rules() {
    let mut s = state();
    let abc = PathBuf::from("/in/tune.ABC");
    let mp3 = PathBuf::from("/in/demo.mp3");
    // no name yet: nowhere to save an ABC file; audio asks for a name once dropped
    assert!(
        s.drop_target(std::slice::from_ref(&abc))
            .unwrap_err()
            .contains("Enter a name first")
    );
    assert_eq!(
        s.drop_target(std::slice::from_ref(&mp3)).unwrap(),
        (DropKind::Audio, mp3.clone())
    );
    update(&mut s, UiAction::SetName("sunny".into()));
    assert_eq!(
        s.drop_target(std::slice::from_ref(&abc)).unwrap(),
        (DropKind::Abc, abc.clone())
    );
    assert_eq!(
        s.drop_target(std::slice::from_ref(&mp3)).unwrap(),
        (DropKind::Audio, mp3.clone())
    );
    assert!(s.drop_target(&[abc.clone(), mp3.clone()]).is_err());
    assert!(s.drop_target(&[]).is_err());

    update(&mut s, Event::TranscribeStarted("sunny".into()));
    assert!(s.drop_target(std::slice::from_ref(&mp3)).is_err());
    // an .abc still loads while a transcription runs
    assert!(s.drop_target(std::slice::from_ref(&abc)).is_ok());

    update(&mut s, UiAction::ShowStatus("nope".into()));
    assert_eq!(s.status.as_deref(), Some("nope"));
}

#[test]
fn model_path_problems_are_kept_per_server() {
    let mut s = state();
    assert!(s.servers[0].path_problems.is_empty());
    update(
        &mut s,
        Event::ModelPaths(ServerId(0), vec!["yue2: /m does not exist".into()]),
    );
    assert_eq!(s.servers[0].path_problems, ["yue2: /m does not exist"]);
    update(&mut s, Event::ModelPaths(ServerId(0), vec![]));
    assert!(s.servers[0].path_problems.is_empty());
    // unknown server ids are ignored
    update(&mut s, Event::ModelPaths(ServerId(7), vec!["x".into()]));
}

fn log_line(source: &str, message: &str) -> LogLine {
    LogLine {
        time: SystemTime::UNIX_EPOCH,
        level: audiocpp_core::service::LogLevel::Info,
        source: source.into(),
        message: message.into(),
        output: true,
    }
}

#[test]
fn log_source_toggles() {
    let mut s = state();
    let line = |source: &str, message: &str, secs: u64| {
        let mut l = log_line(source, message);
        l.time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        Event::Log(l)
    };
    update(&mut s, line("core", "ffmpeg found", 1));
    update(&mut s, line("gpu1", "listening", 2));
    update(&mut s, line("core", "library loaded", 3));
    // server lines go to the server's buffer only
    assert_eq!(s.log.len(), 2);
    let shown = |s: &AppState| {
        s.log_lines()
            .iter()
            .map(|l| l.message.clone())
            .collect::<Vec<_>>()
    };
    // everything, merged by time
    assert_eq!(
        shown(&s),
        vec!["ffmpeg found", "listening", "library loaded"]
    );
    update(&mut s, UiAction::ToggleLogSource(LogSource::App));
    assert_eq!(shown(&s), vec!["listening"]);
    // a chatty app doesn't push the server's lines out
    for i in 0..MAX_LOG {
        update(&mut s, line("core", &i.to_string(), 10));
    }
    assert_eq!(shown(&s), vec!["listening"]);
    // clearing only clears the shown sources
    update(&mut s, UiAction::ClearLog);
    assert!(s.log_lines().is_empty());
    assert_eq!(s.log.len(), MAX_LOG);
    // app only
    update(&mut s, UiAction::ToggleLogSource(LogSource::App));
    update(
        &mut s,
        UiAction::ToggleLogSource(LogSource::Server(ServerId(0))),
    );
    update(&mut s, line("gpu1", "again", 11));
    assert_eq!(shown(&s).len(), MAX_LOG);
    update(&mut s, UiAction::ShowAllLogs);
    assert!(s.log_filter.shows_all());
    assert_eq!(shown(&s).last().unwrap(), "again");
    update(&mut s, UiAction::ClearLog);
    assert!(s.log.is_empty() && s.servers[0].log.is_empty());
}

fn project(name: &str, sha: Option<&str>, transcribed: bool) -> ProjectSummary {
    use audiocpp_core::project::{Reference, Transcription};
    ProjectSummary {
        name: name.into(),
        has_reference: sha.is_some(),
        dir: format!("/lib/inputs/{name}").into(),
        reference: sha.map(|sha| Reference {
            original_name: format!("{sha}.mp3"),
            file: "reference.mp3".into(),
            sha256: sha.into(),
            upload_file: None,
            upload_sha256: sha.into(),
            conversion: None,
            format: "mp3".into(),
        }),
        transcription: transcribed.then(|| Transcription {
            file: "transcription.abc".into(),
            model: "sheetsage2".into(),
            created_at: "2026-09-23T21:40:02Z".into(),
            wall_ms: 1000,
        }),
        ..Default::default()
    }
}

#[test]
fn projects_panel_rename_and_delete() {
    let mut s = state();
    with_songs(&mut s, vec![song("p", 1, Location::Unreviewed, "a")]);
    update(
        &mut s,
        Event::Projects(vec![project("p", None, false), project("q", None, false)]),
    );
    assert_eq!(s.project_songs("p"), 1);
    update(&mut s, UiAction::SelectProject("p".into()));
    assert_eq!(s.projects_view.rename, "p");

    // taken, invalid and unchanged names send nothing
    for bad in ["q", "a/b", "p"] {
        update(&mut s, UiAction::SetProjectRename(bad.into()));
        assert!(update(&mut s, UiAction::RenameProject).is_empty(), "{bad}");
    }
    update(&mut s, UiAction::SetProjectRename("p2".into()));
    assert_eq!(
        update(&mut s, UiAction::RenameProject),
        vec![Command::RenameProject(
            "p".into(),
            RunName::parse("p2").unwrap()
        )]
    );
    // the selection follows once the core lists the new name
    assert_eq!(s.projects_view.current.as_deref(), Some("p"));
    update(
        &mut s,
        Event::Projects(vec![project("p2", None, false), project("q", None, false)]),
    );
    assert_eq!(s.projects_view.current.as_deref(), Some("p2"));

    assert!(update(&mut s, UiAction::DeleteProject("q".into())).is_empty());
    assert_eq!(s.dialog, Some(Dialog::DeleteProject("q".into())));
    assert_eq!(
        update(&mut s, UiAction::ConfirmDialog),
        vec![Command::DeleteProject("q".into())]
    );
    // the selection goes when the project does
    update(&mut s, UiAction::SelectProject("q".into()));
    update(&mut s, Event::Projects(vec![project("p2", None, false)]));
    assert_eq!(s.projects_view.current, None);
}

#[test]
fn projects_being_written_cant_be_renamed_or_deleted() {
    let mut s = state();
    update(&mut s, Event::Projects(vec![project("p", None, false)]));
    update(&mut s, Event::TranscribeStarted("p".into()));
    assert!(s.project_busy("p").is_some());
    update(&mut s, UiAction::SelectProject("p".into()));
    update(&mut s, UiAction::SetProjectRename("p2".into()));
    assert!(update(&mut s, UiAction::RenameProject).is_empty());
    assert!(update(&mut s, UiAction::DeleteProject("p".into())).is_empty());
    assert_eq!(s.dialog, None);
}

#[test]
fn open_project_and_show_its_songs() {
    let mut s = state();
    update(&mut s, UiAction::SelectTab(Tab::Projects));
    assert_eq!(
        update(&mut s, UiAction::OpenProject("p".into())),
        vec![Command::LoadProject("p".into())]
    );
    assert_eq!((s.tab, s.form.name.as_str()), (Tab::Generate, "p"));
    update(&mut s, UiAction::ShowProjectSongs("p".into()));
    assert_eq!((s.tab, s.filter.run_name.as_str()), (Tab::Library, "p"));
}

#[test]
fn reference_rows_merge_the_same_audio() {
    let mut s = state();
    update(
        &mut s,
        Event::Projects(vec![
            project("a", Some("bbb"), false),
            project("b", Some("bbb"), true),
            project("c", Some("aaa"), false),
            project("d", None, false),
        ]),
    );
    let rows = s.reference_rows();
    let summary: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r.reference.sha256.as_str(),
                r.projects
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>(),
                r.transcribed.map(|p| p.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![("aaa", vec!["c"], None), ("bbb", vec!["a", "b"], Some("b")),]
    );
}

#[test]
fn inputs_panel_plays_uses_and_retranscribes() {
    let mut s = state();
    update(
        &mut s,
        Event::Projects(vec![
            project("src", Some("aaa"), true),
            project("dst", Some("zzz"), false),
            project("same", Some("aaa"), false),
        ]),
    );
    assert_eq!(
        update(&mut s, UiAction::PlayReference("src".into())),
        vec![Command::PlayReference("src".into())]
    );
    update(
        &mut s,
        Event::PlaybackState {
            state: PlayState::Playing,
            song: Some(reference_play_id("src")),
            duration: Duration::from_secs(3),
        },
    );
    assert!(s.playing_reference("src"));
    assert_eq!(
        update(&mut s, UiAction::PlayReference("src".into())),
        vec![Command::TogglePause]
    );

    // no name in the form: sent to Generate to type one
    update(&mut s, UiAction::SelectTab(Tab::Inputs));
    assert!(update(&mut s, UiAction::UseReference("src".into())).is_empty());
    assert_eq!((s.tab, s.focus), (Tab::Generate, Some(Focus::Name)));

    // a project with another reference asks before replacing it
    update(&mut s, UiAction::SetName("dst".into()));
    assert!(update(&mut s, UiAction::UseReference("src".into())).is_empty());
    assert!(matches!(
        &s.dialog,
        Some(Dialog::ReplaceReference { audio, force: false })
            if audio == std::path::Path::new("/lib/inputs/src/reference.mp3")
    ));
    update(&mut s, UiAction::CancelDialog);

    // the same audio: nothing to replace
    update(&mut s, UiAction::SetName("same".into()));
    assert_eq!(
        update(&mut s, UiAction::UseReference("src".into())),
        vec![Command::Transcribe {
            project: RunName::parse("same").unwrap(),
            audio: "/lib/inputs/src/reference.mp3".into(),
            force: false,
        }]
    );

    assert_eq!(
        update(&mut s, UiAction::RetranscribeProject("src".into())),
        vec![Command::Retranscribe(RunName::parse("src").unwrap())]
    );
    update(&mut s, Event::TranscribeStarted("src".into()));
    assert!(update(&mut s, UiAction::RetranscribeProject("src".into())).is_empty());
}

#[test]
fn song_arriving_after_review_caught_up_becomes_the_review_song() {
    let mut s = state();
    let a = song("r", 1, Location::Unreviewed, "2026-01-01");
    let ia = a.id.clone();
    with_songs(&mut s, vec![a.clone()]);
    update(&mut s, UiAction::EnterReview);
    assert_eq!(
        update(&mut s, UiAction::ReviewKey(ReviewKey::Rate(Rating::Good))),
        vec![
            Command::Library(LibraryCommand::Rate(ia, Rating::Good)),
            Command::Stop
        ]
    );
    let mut rated = a;
    rated.location = Location::Reviewed(Rating::Good);
    let delta = |songs| {
        Event::LibraryChanged(LibraryDelta {
            full: false,
            upserted: songs,
            removed: vec![],
        })
    };
    assert!(update(&mut s, delta(vec![rated])).is_empty());
    assert_eq!(s.review_song(), None, "caught up");

    let b = song("r", 2, Location::Unreviewed, "2026-01-02");
    let ib = b.id.clone();
    assert_eq!(update(&mut s, delta(vec![b])), vec![Command::Play(ib.clone())]);
    assert_eq!(s.review_song(), Some(&ib));
    assert_eq!(s.current.as_ref(), Some(&ib));

    // more songs while one is under review just queue up
    let c = song("r", 3, Location::Unreviewed, "2026-01-03");
    assert!(update(&mut s, delta(vec![c])).is_empty());
    assert_eq!(s.review_song(), Some(&ib));
    assert_eq!(s.review.queue.len(), 3);
}
