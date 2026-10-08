//! End-to-end generation against mock servers (design §9.2).

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{Harness, MockServer, config, wav_for_seed};
use incremusic_core::Command;
use incremusic_core::media;
use incremusic_core::params::GenerationParams;
use incremusic_core::run::{AbcSource, RunEdit, RunName, RunSpec, RunStatus};
use incremusic_core::scheduler::{JobState, ServerId};
use incremusic_core::service::{Event, ServerState};

const LONG: Duration = Duration::from_secs(30);

fn spec(name: &str, start: u32, count: Option<u32>) -> RunSpec {
    RunSpec {
        name: RunName::parse(name).unwrap(),
        params: GenerationParams {
            lyrics: "[Verse]\nla la".into(),
            style: "pop".into(),
            abc: Some("X:1\nK:C\nCDEF|".into()),
            ..Default::default()
        },
        start_seed: start,
        count,
        model: common::model("yue2", "yue2", "gen"),
        abc_source: AbcSource::Manual,
    }
}

fn song_dir(root: &Path, name: &str, seed: u32) -> std::path::PathBuf {
    root.join("unreviewed").join(format!("{name}-{seed}"))
}

fn servers_of_seeds(h: &Harness) -> BTreeMap<u32, ServerId> {
    let mut m = BTreeMap::new();
    for (id, hist) in &h.job_history {
        let seed = h.jobs[id].seed;
        for s in hist {
            if let JobState::Running { server, .. } = s {
                m.insert(seed, *server);
            }
        }
    }
    m
}

#[test]
fn two_servers_share_a_run_and_write_song_folders() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let b = MockServer::start("b");
    for m in [&a, &b] {
        *m.state.gen_delay.lock() = Duration::from_millis(300);
    }
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port, b.port]));
    h.wait_ready(2);
    h.send(Command::StartRun(spec("sunny-hook", 1230, Some(6))));
    h.until("6 songs", LONG, |h| h.done_jobs() == 6);

    let by_server = servers_of_seeds(&h);
    assert_eq!(
        by_server.keys().copied().collect::<Vec<_>>(),
        (1230..1236).collect::<Vec<_>>()
    );
    let used: BTreeSet<_> = by_server.values().collect();
    assert_eq!(used.len(), 2, "both servers got jobs: {by_server:?}");

    for seed in 1230..1236 {
        let dir = song_dir(lib.path(), "sunny-hook", seed);
        let stem = format!("sunny-hook-{seed}");
        let mp3 = dir.join(format!("{stem}.mp3"));
        let wav = dir.join(format!("{stem}.wav"));
        assert!(mp3.is_file(), "{}", mp3.display());
        assert_eq!(
            std::fs::read(&wav).unwrap(),
            wav_for_seed(seed as u64),
            "WAV is the server's audio byte for byte"
        );
        let meta = media::read_meta(&mp3).unwrap();
        assert_eq!(meta.title.as_deref(), Some(stem.as_str()));
        let r = meta.recipe.unwrap();
        assert_eq!(
            (
                r.run.name.as_str(),
                r.run.seed,
                r.run.start_seed,
                r.run.index
            ),
            ("sunny-hook", seed, 1230, seed - 1230)
        );
        assert_eq!(r.request["request"]["seed"], seed);
        assert_eq!(r.request["request"]["options"]["abc"], "X:1\nK:C\nCDEF|");
        assert_eq!(
            r.output.wav_sha256,
            incremusic_core::fsutil::sha256_hex(&wav_for_seed(seed as u64))
        );
        assert_eq!(r.output.encoder, "mp3 128k");
        assert_eq!(r.timing.audio_duration_ms, Some(100));
        assert_eq!(r.server.backend.as_deref(), Some("mock"));
        assert_eq!(r.project, "sunny-hook");
    }
    // project files were written at run start
    let proj = lib.path().join("inputs/sunny-hook");
    assert_eq!(
        std::fs::read_to_string(proj.join("sunny-hook.abc")).unwrap(),
        "X:1\nK:C\nCDEF|"
    );
    assert_eq!(
        std::fs::read_to_string(proj.join("style.txt")).unwrap(),
        "pop"
    );
    assert!(proj.join("project.ron").is_file());
    assert!(
        !lib.path().join("unreviewed").read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part"))
    );
    h.until("run done", LONG, |h| {
        h.runs.iter().any(|r| r.state.status == RunStatus::Done)
    });
    h.core.shutdown(false);
}

#[test]
fn params_edit_mid_run_applies_to_unstarted_jobs_only() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    *a.state.gen_delay.lock() = Duration::from_millis(600);
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    h.send(Command::StartRun(spec("edit", 10, Some(3))));
    h.until("first job running", LONG, |h| {
        h.jobs
            .values()
            .any(|j| matches!(j.state, JobState::Running { .. }))
    });
    let run_id = h.runs[0].state.id;
    let mut p = spec("edit", 10, None).params;
    p.style = "rock".into();
    p.lyrics = "new words".into();
    h.send(Command::EditRun(run_id, RunEdit::Params(p)));
    h.until("3 songs", LONG, |h| h.done_jobs() == 3);

    let recipe = |seed| {
        media::read_meta(&song_dir(lib.path(), "edit", seed).join(format!("edit-{seed}.mp3")))
            .unwrap()
            .recipe
            .unwrap()
    };
    let r10 = recipe(10);
    assert_eq!(r10.run.revision, 0);
    assert_eq!(r10.request["request"]["options"]["style"], "pop");
    for seed in [11, 12] {
        let r = recipe(seed);
        assert_eq!(r.run.revision, 1, "seed {seed}");
        assert_eq!(r.request["request"]["options"]["style"], "rock");
        assert_eq!(r.request["request"]["lyrics"], "new words");
    }
    // the project's files show the latest revision
    let proj = lib.path().join("inputs/edit");
    assert_eq!(
        std::fs::read_to_string(proj.join("style.txt")).unwrap(),
        "rock"
    );
    assert_eq!(
        std::fs::read_to_string(proj.join("lyrics.txt")).unwrap(),
        "new words"
    );
    h.core.shutdown(false);
}

#[test]
fn crashed_server_job_is_requeued_on_the_other() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let b = MockServer::start("b");
    *a.state.gen_delay.lock() = Duration::from_millis(200);
    *b.state.gen_delay.lock() = Duration::from_millis(800);
    a.state.crash_next.store(true, Ordering::SeqCst);
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port, b.port]));
    h.wait_ready(2);
    h.send(Command::StartRun(spec("crash", 0, Some(3))));
    h.until("server a down", LONG, |h| {
        matches!(h.servers.get(&ServerId(0)), Some(ServerState::Down(_)))
    });
    a.kill();
    h.until("3 songs", LONG, |h| h.done_jobs() == 3);
    for seed in 0..3 {
        assert!(
            song_dir(lib.path(), "crash", seed)
                .join(format!("crash-{seed}.mp3"))
                .is_file()
        );
    }
    let retried = h.jobs.values().filter(|j| j.attempts == 1).count();
    assert_eq!(
        retried, 1,
        "the crashed job was retried once with the same seed"
    );
    assert!(matches!(h.servers[&ServerId(0)], ServerState::Down(_)));
    h.core.shutdown(false);
}

#[test]
fn http_4xx_fails_immediately_and_5xx_retries() {
    let a = MockServer::start("a");
    a.state.reject_params.store(true, Ordering::SeqCst);
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    h.send(Command::StartRun(spec("bad", 0, Some(1))));
    h.until("failed", LONG, |h| {
        h.jobs
            .values()
            .any(|j| matches!(j.state, JobState::Failed(_)))
    });
    assert_eq!(
        h.jobs.values().next().unwrap().attempts,
        0,
        "4xx is not retried"
    );
    assert_eq!(
        a.state
            .calls()
            .iter()
            .filter(|c| c.starts_with("run yue2"))
            .count(),
        1
    );

    a.state.reject_params.store(false, Ordering::SeqCst);
    a.state.fail_next.store(5, Ordering::SeqCst);
    h.send(Command::StartRun(spec("flaky", 0, Some(1))));
    h.until("flaky failed", LONG, |h| {
        h.jobs
            .values()
            .filter(|j| matches!(j.state, JobState::Failed(_)))
            .count()
            == 2
    });
    let flaky = h.jobs.values().last().unwrap();
    assert_eq!(flaky.seed, 0);
    assert_eq!(flaky.attempts, 1, "second failure ends it (2 attempts)");
    h.core.shutdown(false);
}

#[test]
fn cancelling_a_running_job_keeps_the_connection_and_discards_the_result() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    *a.state.gen_delay.lock() = Duration::from_millis(1500);
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    h.send(Command::StartRun(spec("cancel", 5, Some(2))));
    h.until("running", LONG, |h| {
        h.jobs
            .values()
            .any(|j| matches!(j.state, JobState::Running { .. }))
    });
    let job = *h.jobs.keys().next().unwrap();
    h.send(Command::CancelJob(job));
    h.until("cancelling", LONG, |h| {
        matches!(h.jobs[&job].state, JobState::Cancelling { .. })
    });
    // while the slow response is pending the server gets no new job
    h.pump(Duration::from_millis(700));
    assert_eq!(
        a.state
            .calls()
            .iter()
            .filter(|c| c.starts_with("run yue2"))
            .count(),
        1,
        "{:?}",
        a.state.calls()
    );
    assert_eq!(
        a.state.in_flight.load(Ordering::SeqCst),
        1,
        "request kept open"
    );
    h.until("cancelled", LONG, |h| {
        h.jobs[&job].state == JobState::Cancelled
    });
    h.until("next seed done", LONG, |h| h.done_jobs() == 1);
    assert!(
        !song_dir(lib.path(), "cancel", 5).exists(),
        "no file for the cancelled seed"
    );
    assert!(
        song_dir(lib.path(), "cancel", 6)
            .join("cancel-6.mp3")
            .is_file()
    );
    assert_eq!(a.state.max_in_flight.load(Ordering::SeqCst), 1);
    h.core.shutdown(false);
}

#[test]
fn stop_run_keeps_running_jobs_and_drops_queued_retries() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let b = MockServer::start("b");
    *a.state.gen_delay.lock() = Duration::from_millis(1200);
    *b.state.gen_delay.lock() = Duration::from_millis(100);
    b.state.fail_next.store(1, Ordering::SeqCst);
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port, b.port]));
    h.wait_ready(2);
    h.send(Command::StartRun(spec("stop", 0, None)));
    h.until("a is running a job", LONG, |h| {
        h.jobs.values().any(|j| {
            matches!(
                j.state,
                JobState::Running {
                    server: ServerId(0),
                    ..
                }
            )
        })
    });
    let run_id = h.runs[0].state.id;
    h.send(Command::PauseRun(run_id));
    h.until("a retry is queued", LONG, |h| {
        h.jobs
            .values()
            .any(|j| j.state == JobState::Queued && j.attempts == 1)
    });
    let a_job = h
        .jobs
        .values()
        .find(|j| {
            matches!(
                j.state,
                JobState::Running {
                    server: ServerId(0),
                    ..
                }
            )
        })
        .unwrap()
        .id;
    let retry = h
        .jobs
        .values()
        .find(|j| j.state == JobState::Queued)
        .unwrap()
        .id;
    h.send(Command::StopRun(run_id));
    h.until("run done", LONG, |h| {
        h.runs.iter().any(|r| r.state.status == RunStatus::Done)
    });
    assert_eq!(
        h.jobs[&retry].state,
        JobState::Cancelled,
        "queued retry dropped"
    );
    assert!(
        matches!(h.jobs[&a_job].state, JobState::Done(_)),
        "running job kept"
    );
    h.core.shutdown(false);
}

#[test]
fn first_come_first_served_across_fast_and_slow_servers_and_runs() {
    require_ffmpeg!();
    let fast = MockServer::start("fast");
    let slow = MockServer::start("slow");
    *fast.state.gen_delay.lock() = Duration::from_millis(100);
    *slow.state.gen_delay.lock() = Duration::from_millis(1100);
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[fast.port, slow.port]));
    h.wait_ready(2);
    h.send(Command::StartRun(spec("A", 0, Some(10))));
    h.send(Command::StartRun(spec("B", 100, Some(2))));
    h.until("12 songs", LONG, |h| h.done_jobs() == 12);
    let fast_n = fast.state.gen_count.load(Ordering::SeqCst);
    let slow_n = slow.state.gen_count.load(Ordering::SeqCst);
    assert!(fast_n > slow_n * 2, "fast {fast_n} vs slow {slow_n}");
    let seeds = servers_of_seeds(&h);
    let a: Vec<u32> = seeds.keys().copied().filter(|s| *s < 100).collect();
    assert_eq!(a, (0..10).collect::<Vec<_>>(), "unique, no gaps");

    // B's first job started before A's last job finished
    let started: Vec<(u32, usize)> = h
        .events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Event::JobUpdate(_, info) if matches!(info.state, JobState::Running { .. }) => {
                Some((info.seed, i))
            }
            _ => None,
        })
        .collect();
    let b_start = started.iter().find(|(s, _)| *s == 100).unwrap().1;
    let a_last_done = h
        .events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Event::JobUpdate(_, info)
                if info.seed < 100 && matches!(info.state, JobState::Done(_)) =>
            {
                Some(i)
            }
            _ => None,
        })
        .max()
        .unwrap();
    assert!(
        b_start < a_last_done,
        "B started at event {b_start}, A finished at {a_last_done}"
    );
    h.core.shutdown(false);
}

#[test]
fn timeout_requeues_on_the_other_server_and_parks_the_slow_one() {
    require_ffmpeg!();
    let slow = MockServer::start("slow");
    let ok = MockServer::start("ok");
    *slow.state.gen_delay.lock() = Duration::from_secs(4);
    *ok.state.gen_delay.lock() = Duration::from_millis(1500);
    let lib = tempfile::tempdir().unwrap();
    let mut cfg = config(lib.path(), &[slow.port, ok.port]);
    cfg.request_timeout_secs = 2;
    let mut h = Harness::start(cfg);
    h.wait_ready(2);
    h.send(Command::StartRun(spec("t", 0, Some(2))));
    h.until("2 songs", LONG, |h| h.done_jobs() == 2);
    assert_eq!(
        h.servers[&ServerId(0)],
        ServerState::Down("request timed out; may still be busy".into())
    );
    // stays down even though /health answers …
    h.pump(Duration::from_millis(800));
    assert!(matches!(h.servers[&ServerId(0)], ServerState::Down(_)));
    // … until Recheck
    h.send(Command::RecheckServer(ServerId(0)));
    h.until("rechecked", LONG, |h| h.servers[&ServerId(0)].is_up());
    h.core.shutdown(false);
}

#[test]
fn regenerate_saves_a_new_take_next_to_the_original() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    h.send(Command::StartRun(spec("regen", 7, Some(1))));
    h.until("1 song", LONG, |h| h.done_jobs() == 1);
    let song = h
        .find(|e| match e {
            Event::LibraryChanged(d) => d
                .upserted
                .iter()
                .find(|s| s.stem == "regen-7")
                .map(|s| s.id.clone()),
            _ => None,
        })
        .unwrap();
    h.send(Command::Regenerate(song));
    h.until("2 songs", LONG, |h| h.done_jobs() == 2);
    let take = lib.path().join("unreviewed/regen-7-r2/regen-7-r2.mp3");
    assert!(take.is_file());
    let r = media::read_meta(&take).unwrap().recipe.unwrap();
    assert_eq!(r.run.seed, 7);
    let orig = media::read_meta(&lib.path().join("unreviewed/regen-7/regen-7.mp3"))
        .unwrap()
        .recipe
        .unwrap();
    assert_eq!(r.request, orig.request, "same recipe");
    h.core.shutdown(false);
}

#[test]
fn name_collision_at_write_time_gets_a_suffix() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let lib = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lib.path().join("reviewed/good/dup-1")).unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    h.send(Command::StartRun(spec("dup", 1, Some(1))));
    h.until("1 song", LONG, |h| h.done_jobs() == 1);
    assert!(lib.path().join("unreviewed/dup-1-2/dup-1-2.mp3").is_file());
    assert!(lib.path().join("unreviewed/dup-1-2/dup-1-2.wav").is_file());
    h.core.shutdown(false);
}

#[test]
fn ensure_loaded_reloads_when_session_options_change() {
    let a = MockServer::start("a");
    common::rt().block_on(async {
        let c = incremusic_core::api::AudioCppClient::local(a.port, Duration::from_secs(5));
        let mut spec = common::model("yue2", "yue2", "gen");
        use incremusic_core::models::{EnsureAction, ensure_loaded};
        assert_eq!(
            ensure_loaded(&c, &spec).await.unwrap(),
            EnsureAction::Loaded
        );
        assert_eq!(
            ensure_loaded(&c, &spec).await.unwrap(),
            EnsureAction::AlreadyLoaded
        );
        spec.session_options
            .insert("yue2.ar_lora_scale".into(), "0.5".into());
        assert_eq!(
            ensure_loaded(&c, &spec).await.unwrap(),
            EnsureAction::Reloaded
        );
    });
    assert_eq!(
        a.state.calls(),
        vec!["load yue2", "unload yue2", "load yue2"]
    );
}

#[test]
fn every_run_is_recorded_in_history_and_an_interrupted_one_resumes() {
    use incremusic_core::history::{RecordStatus, SeedOutcome};
    require_ffmpeg!();
    let a = MockServer::start("a");
    let lib = tempfile::tempdir().unwrap();
    let first = {
        let mut h = Harness::start(config(lib.path(), &[a.port]));
        h.wait_ready(1);
        h.send(Command::StartRun(spec("kept", 40, Some(1))));
        h.until("song", LONG, |h| h.done_jobs() == 1);
        let mut p = spec("kept", 0, None).params;
        p.style = "jazz".into();
        // quits while seed 7 is still generating; seeds 8 and 9 never start
        *a.state.gen_delay.lock() = Duration::from_secs(5);
        h.send(Command::StartRun(spec("unfinished", 7, Some(3))));
        h.until("running", LONG, |h| {
            h.jobs
                .values()
                .any(|j| j.seed == 7 && matches!(j.state, JobState::Running { .. }))
        });
        let id = h.jobs.values().find(|j| j.seed == 7).unwrap().run_id;
        h.send(Command::PauseRun(id));
        h.send(Command::EditRun(id, RunEdit::Params(p)));
        h.pump(Duration::from_millis(200));
        h.core.shutdown(false);
        id
    };
    *a.state.gen_delay.lock() = Duration::from_millis(50);
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    h.send(Command::ListRuns);
    h.until("history", LONG, |h| {
        h.find(|e| match e {
            Event::RunHistory(l) if l.len() == 2 => Some(()),
            _ => None,
        })
        .is_some()
    });
    h.send(Command::LoadRunRecord(first));
    h.until("record", LONG, |h| {
        h.find(|e| match e {
            Event::RunRecord(id, Ok(r)) if *id == first => Some(()),
            _ => None,
        })
        .is_some()
    });
    let r = h
        .find(|e| match e {
            Event::RunRecord(_, Ok(r)) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(r.status, RecordStatus::Interrupted);
    assert_eq!(r.revisions.len(), 2, "the edit made while paused is kept");
    assert_eq!(r.revisions[1].params.style, "jazz");
    let kept = h
        .find(|e| match e {
            Event::RunHistory(l) => l.iter().find(|s| s.name == "kept").cloned(),
            _ => None,
        })
        .unwrap();
    assert_eq!((kept.status, kept.done), (RecordStatus::Done, 1));

    h.send(Command::ResumeInterrupted(first));
    h.until("resumed songs", LONG, |h| h.done_jobs() == 3);
    let seeds: BTreeSet<u32> = h.jobs.values().map(|j| j.seed).collect();
    assert_eq!(
        seeds,
        (7..10).collect(),
        "seed 7 was running at quit, 8 and 9 never started"
    );
    h.send(Command::ListRuns);
    h.until("3 records", LONG, |h| {
        h.find(|e| match e {
            Event::RunHistory(l) if l.len() == 3 => Some(()),
            _ => None,
        })
        .is_some()
    });
    let resumed = h
        .find(|e| match e {
            Event::RunHistory(l) if l.len() == 3 => Some(l[0].id),
            _ => None,
        })
        .unwrap();
    h.send(Command::LoadRunRecord(resumed));
    h.until("resumed record", LONG, |h| {
        h.find(|e| match e {
            Event::RunRecord(id, Ok(r)) if *id == resumed && r.status == RecordStatus::Done => {
                Some(())
            }
            _ => None,
        })
        .is_some()
    });
    let r = h
        .find(|e| match e {
            Event::RunRecord(id, Ok(r)) if *id == resumed => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(r.resumed_from, Some(first));
    assert_eq!(
        r.revisions[0].params.style, "jazz",
        "resumes with the latest params"
    );
    assert!(
        r.seeds
            .iter()
            .all(|s| matches!(s.outcome, SeedOutcome::Song(_)))
    );
    h.core.shutdown(false);
}
