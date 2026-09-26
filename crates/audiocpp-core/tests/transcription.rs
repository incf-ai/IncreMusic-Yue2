//! Transcribe → generate pipeline against a WAV-only mock server (design §9.2).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command as Proc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use audiocpp_core::Command;
use audiocpp_core::params::GenerationParams;
use audiocpp_core::project::ProjectStore;
use audiocpp_core::run::{AbcSource, RunName, RunSpec};
use audiocpp_core::scheduler::JobState;
use audiocpp_core::service::Event;
use audiocpp_core::transcribe::AbcScore;
use common::{Harness, MockServer, config};

const LONG: Duration = Duration::from_secs(30);
const FIXTURE_WAV: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/one_second.wav"
);

/// Makes a reference file of the given kind from the fixture WAV.
fn make_input(dir: &Path, kind: &str) -> PathBuf {
    let (file, args): (&str, &[&str]) = match kind {
        "mp3" => (
            "My Demo (final).mp3",
            &["-c:a", "libmp3lame", "-b:a", "96k"],
        ),
        "flac" => ("take.flac", &["-c:a", "flac"]),
        "f32wav" => ("float.wav", &["-c:a", "pcm_f32le"]),
        "s16wav" => ("plain.wav", &[]),
        _ => unreachable!(),
    };
    let out = dir.join(file);
    if kind == "s16wav" {
        std::fs::copy(FIXTURE_WAV, &out).unwrap();
    } else {
        let st = Proc::new("ffmpeg")
            .args(["-v", "error", "-y", "-i", FIXTURE_WAV])
            .args(args)
            .arg(&out)
            .status()
            .unwrap();
        assert!(st.success());
    }
    out
}

fn transcribed(h: &mut Harness, n: usize) -> Vec<Result<AbcScore, String>> {
    h.until(&format!("{n} transcriptions"), LONG, |h| {
        h.events
            .iter()
            .filter(|e| matches!(e, Event::TranscribeDone(_)))
            .count()
            >= n
    });
    h.events
        .iter()
        .filter_map(|e| match e {
            Event::TranscribeDone(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn run_spec(name: &str, abc: String, src: AbcSource) -> RunSpec {
    RunSpec {
        name: RunName::parse(name).unwrap(),
        params: GenerationParams {
            lyrics: "la".into(),
            style: "folk".into(),
            abc: Some(abc),
            ..Default::default()
        },
        start_seed: 1,
        count: Some(1),
        model: common::model("yue2", "yue2", "gen"),
        abc_source: src,
    }
}

#[test]
fn transcribe_then_generate_for_every_input_kind() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let lib = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);

    let kinds = ["mp3", "flac", "f32wav", "s16wav"];
    for (i, kind) in kinds.iter().enumerate() {
        let input = make_input(src.path(), kind);
        let name = format!("p-{kind}");
        h.send(Command::Transcribe {
            project: RunName::parse(&name).unwrap(),
            audio: input.clone(),
            force: false,
        });
        let score = transcribed(&mut h, i + 1).pop().unwrap().unwrap();
        assert!(!score.reused, "{kind}");
        assert!(score.abc.starts_with("X:1"));

        let dir = lib.path().join("inputs").join(&name);
        let ext = input.extension().unwrap().to_string_lossy().to_string();
        let reference = dir.join(format!("reference.{ext}"));
        assert_eq!(
            std::fs::read(&reference).unwrap(),
            std::fs::read(&input).unwrap(),
            "original copied unchanged"
        );
        let upload = dir.join("reference.upload.wav");
        if *kind == "s16wav" {
            assert!(!upload.exists(), "PCM s16 WAV is uploaded as is");
        } else {
            let info = audiocpp_core::wav::read_header(&upload).unwrap();
            assert!(info.is_pcm_s16(), "{kind}");
            assert_eq!(
                (info.sample_rate, info.channels),
                (48000, 2),
                "rate and channels kept"
            );
        }
        assert!(dir.join("transcription.abc").is_file());
        assert!(dir.join("transcription.events.json").is_file());
        let project = ProjectStore::new(lib.path().join("inputs"))
            .load(&name)
            .unwrap();
        let r = project.reference.unwrap();
        assert_eq!(
            r.original_name,
            input.file_name().unwrap().to_string_lossy()
        );
        assert_eq!(r.upload_file.is_none(), *kind == "s16wav");
        assert_eq!(project.transcription.unwrap().wall_ms, 10513);
        assert_eq!(
            score.reference.sha256,
            audiocpp_core::fsutil::sha256_file(&input).unwrap()
        );

        // generate with it
        h.send(Command::StartRun(run_spec(
            &name,
            score.abc.clone(),
            AbcSource::Transcribed(score.reference.clone()),
        )));
        h.until("song", LONG, |h| h.done_jobs() == i + 1);
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("{name}.abc"))).unwrap(),
            score.abc
        );
        assert!(dir.join("lyrics.txt").is_file() && dir.join("style.txt").is_file());
        let project = ProjectStore::new(lib.path().join("inputs"))
            .load(&name)
            .unwrap();
        assert_eq!(project.runs.len(), 1);
        let mp3 = lib.path().join(format!("unreviewed/{name}-1/{name}-1.mp3"));
        let recipe = audiocpp_core::media::read_meta(&mp3)
            .unwrap()
            .recipe
            .unwrap();
        match recipe.abc_source {
            AbcSource::Transcribed(r) => assert_eq!(r.sha256, score.reference.sha256),
            other => panic!("{other:?}"),
        }
    }
    // the server only ever saw WAV uploads
    let calls = a.state.calls();
    assert_eq!(calls.iter().filter(|c| *c == "upload").count(), 4);
    assert!(!calls.iter().any(|c| c == "upload rejected"));
    for up in a.state.uploads.lock().iter() {
        assert!(audiocpp_core::wav::read_header(up).unwrap().is_pcm_s16());
    }
    h.core.shutdown(false);
}

#[test]
fn every_transcription_is_followed_by_an_unload_before_the_next_generation() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let lib = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    let input = make_input(src.path(), "flac");

    // success
    h.send(Command::Transcribe {
        project: RunName::parse("ok").unwrap(),
        audio: input.clone(),
        force: false,
    });
    let s = transcribed(&mut h, 1).pop().unwrap().unwrap();
    h.send(Command::StartRun(run_spec("ok", s.abc, AbcSource::Manual)));
    h.until("song", LONG, |h| h.done_jobs() == 1);

    // failure
    a.state.fail_transcribe.store(true, Ordering::SeqCst);
    h.send(Command::Transcribe {
        project: RunName::parse("fail").unwrap(),
        audio: input,
        force: true,
    });
    let r = transcribed(&mut h, 2).pop().unwrap();
    assert!(r.is_err());
    h.send(Command::StartRun(run_spec(
        "fail",
        "X:1\nK:C\n".into(),
        AbcSource::Manual,
    )));
    h.until("song 2", LONG, |h| h.done_jobs() == 2);

    let calls = a.state.calls();
    let runs: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, c)| *c == "run sheetsage2")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(runs.len(), 2, "{calls:?}");
    for i in runs {
        let next_gen = calls[i..]
            .iter()
            .position(|c| c.starts_with("run yue2"))
            .unwrap()
            + i;
        assert!(
            calls[i..next_gen].contains(&"unload sheetsage2".to_string()),
            "{calls:?}"
        );
    }
    h.core.shutdown(false);
}

#[test]
fn same_reference_in_another_project_reuses_the_transcription() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let lib = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    let input = make_input(src.path(), "mp3");
    h.send(Command::Transcribe {
        project: RunName::parse("one").unwrap(),
        audio: input.clone(),
        force: false,
    });
    transcribed(&mut h, 1).pop().unwrap().unwrap();
    h.send(Command::Transcribe {
        project: RunName::parse("two").unwrap(),
        audio: input.clone(),
        force: false,
    });
    let s2 = transcribed(&mut h, 2).pop().unwrap().unwrap();
    assert!(s2.reused);
    let server_runs = |a: &MockServer| {
        a.state
            .calls()
            .iter()
            .filter(|c| *c == "run sheetsage2")
            .count()
    };
    assert_eq!(server_runs(&a), 1, "no server call for the second project");
    let two = lib.path().join("inputs/two");
    assert!(two.join("transcription.abc").is_file());
    assert!(two.join("reference.mp3").is_file());

    // Re-transcribe forces a server call
    h.send(Command::Transcribe {
        project: RunName::parse("two").unwrap(),
        audio: input,
        force: true,
    });
    let s3 = transcribed(&mut h, 3).pop().unwrap().unwrap();
    assert!(!s3.reused);
    assert_eq!(server_runs(&a), 2);
    h.core.shutdown(false);
}

#[test]
fn unreadable_files_are_rejected_before_copying() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    let lib = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let bogus = src.path().join("not audio.mp3");
    std::fs::write(&bogus, b"this is definitely not an mp3 file at all").unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    h.send(Command::Transcribe {
        project: RunName::parse("bogus").unwrap(),
        audio: bogus,
        force: false,
    });
    let r = transcribed(&mut h, 1).pop().unwrap();
    assert!(r.unwrap_err().contains("not readable"));
    assert!(!lib.path().join("inputs/bogus").exists());
    assert!(a.state.calls().is_empty());
    h.core.shutdown(false);
}

#[test]
fn params_edit_mid_run_rewrites_project_files_and_load_project_reads_them() {
    require_ffmpeg!();
    let a = MockServer::start("a");
    *a.state.gen_delay.lock() = Duration::from_millis(500);
    let lib = tempfile::tempdir().unwrap();
    let mut h = Harness::start(config(lib.path(), &[a.port]));
    h.wait_ready(1);
    let mut spec = run_spec("proj", "X:1\nK:C\n".into(), AbcSource::Manual);
    spec.count = Some(2);
    h.send(Command::StartRun(spec.clone()));
    h.until("running", LONG, |h| {
        h.jobs
            .values()
            .any(|j| matches!(j.state, JobState::Running { .. }))
    });
    let run = h.runs[0].state.id;
    let mut p = spec.params.clone();
    p.abc = Some("X:1\nK:G\n".into());
    p.lyrics = "edited".into();
    h.send(Command::EditRun(
        run,
        audiocpp_core::run::RunEdit::Params(p),
    ));
    h.until("2 songs", LONG, |h| h.done_jobs() == 2);
    let dir = lib.path().join("inputs/proj");
    assert_eq!(
        std::fs::read_to_string(dir.join("proj.abc")).unwrap(),
        "X:1\nK:G\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("lyrics.txt")).unwrap(),
        "edited"
    );

    h.send(Command::LoadProject("proj".into()));
    h.until("inputs", LONG, |h| {
        h.events
            .iter()
            .any(|e| matches!(e, Event::ProjectInputs(_)))
    });
    let inputs = h.find(|e| match e {
        Event::ProjectInputs(Ok(i)) => Some(i.clone()),
        _ => None,
    });
    let inputs = inputs.unwrap();
    assert_eq!(inputs.abc.as_deref(), Some("X:1\nK:G\n"));
    assert_eq!(inputs.lyrics.as_deref(), Some("edited"));
    assert_eq!(inputs.style.as_deref(), Some("folk"));
    h.core.shutdown(false);
}
