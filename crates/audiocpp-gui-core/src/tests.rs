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
            encoder: "aac".into(),
        },
    };
    let stem = format!("{name}-{seed}");
    Song {
        id: SongId(format!("{stem}-id")),
        stem: stem.clone(),
        dir: Some(format!("/lib/x/{stem}").into()),
        mp4: format!("/lib/x/{stem}/{stem}.mp4").into(),
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
fn start_run_sends_spec_and_clears_name() {
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
    assert_eq!(s.form.name, "", "name is cleared after each run starts");
    assert_eq!(s.form.params.lyrics, "la", "everything else is kept");
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
fn new_reference_in_existing_project_asks_first() {
    let mut s = state();
    update(
        &mut s,
        Event::Projects(vec![ProjectSummary {
            name: "n".into(),
            has_reference: true,
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
        vec![Command::Play(ib)]
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
    assert_eq!(names(&s), vec!["beta-20", "alpha-1"], "newest first");
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
