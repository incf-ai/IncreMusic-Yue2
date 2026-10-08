//! Smoke test against a real audio.cpp server. Run with
//! `AUDIOCPP_SERVER_URL=http://127.0.0.1:8080 cargo test -p incremusic-core --test real_server -- --ignored`.
//! Model paths come from `AUDIOCPP_MODELS_ROOT` (default: the server's `/v1/ui/models-root`).
//! `generates_one_song_through_the_core` uses the models in `config.example.ron` and keeps
//! its library in `INCREMUSIC_SMOKE_LIBRARY` if set (default: a temp dir).
//! `transcribes_through_the_core` transcribes `INCREMUSIC_SMOKE_AUDIO` (default: the tiny MP3
//! fixture) with SheetSage2.

use std::time::Duration;

use incremusic_core::api::{AudioCppClient, GenerateOutcome};
use incremusic_core::config::ModelSpec;
use incremusic_core::models::ensure_loaded;
use incremusic_core::params::GenerationParams;

#[tokio::test]
#[ignore]
async fn generates_one_short_song() {
    let Ok(url) = std::env::var("AUDIOCPP_SERVER_URL") else {
        eprintln!("AUDIOCPP_SERVER_URL not set");
        return;
    };
    let c = AudioCppClient::new(url, Duration::from_secs(1800));
    assert!(c.health().await.unwrap().is_ok());
    let root = match std::env::var("AUDIOCPP_MODELS_ROOT") {
        Ok(r) => r,
        Err(_) => c.models_root().await.unwrap(),
    };
    let spec = ModelSpec {
        id: "yue2".into(),
        family: "yue2".into(),
        task: "gen".into(),
        mode: "offline".into(),
        path: format!("{root}/Yue2-3B-GGUF"),
        load_options: Default::default(),
        session_options: [
            ("yue2.ar_lora_scale", "1"),
            ("yue2.nar_lora_scale", "1"),
            ("yue2.model_gguf", "yue2-3b-bf16.gguf"),
            ("yue2.vae_gguf", "yue2-vae-f32.gguf"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
    };
    assert!(c.path_status(&spec.path).await.unwrap().directory);
    ensure_loaded(&c, &spec).await.unwrap();
    let mut p = GenerationParams {
        lyrics: "[Verse]\nsmoke test, a tiny little song".into(),
        style: "English, indie pop, acoustic guitar".into(),
        ..Default::default()
    };
    p.semantic_sampling.min_tokens = 50;
    p.semantic_sampling.max_tokens = 300;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("smoke.wav");
    let r = c
        .generate(&p.to_task_body("yue2", 1234), &out, || false)
        .await
        .unwrap();
    let GenerateOutcome::Done(g) = r else {
        panic!("discarded")
    };
    eprintln!("{g:?}");
    assert!(g.wav_bytes > 44);
    assert_eq!(
        incremusic_core::wav::read_header(&out).unwrap().sample_rate,
        g.sample_rate
    );
    assert!(g.timing.wall_ms > 0);
}

/// The whole pipeline against a real server: attach, model path check, scheduler,
/// generation, MP3 encoding, recipe and library.
#[test]
#[ignore]
fn generates_one_song_through_the_core() {
    use incremusic_core::config::{Config, ServerConfig};
    use incremusic_core::run::{AbcSource, RunName, RunSpec};
    use incremusic_core::scheduler::JobState;
    use incremusic_core::service::{CoreHandle, CoreOptions, Event};
    use incremusic_core::{Command, CoreBackend};

    let Ok(url) = std::env::var("AUDIOCPP_SERVER_URL") else {
        eprintln!("AUDIOCPP_SERVER_URL not set");
        return;
    };
    let port: u16 = url
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::env::var("INCREMUSIC_SMOKE_LIBRARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let text = include_str!("../../../config.example.ron");
    let mut cfg = Config::parse(text).unwrap();
    cfg.servers = vec![ServerConfig {
        name: "real".into(),
        port,
        launch: None,
    }];
    cfg.library.root = root.clone();
    cfg.defaults = None;
    let model = cfg.models.yue2.clone();
    let core = CoreHandle::start(cfg, CoreOptions::default()).unwrap();

    let mut p = GenerationParams {
        lyrics: "[Verse]\nsmoke test, a tiny little song".into(),
        style: "English, indie pop, acoustic guitar".into(),
        abc: Some("X:1\nM:4/4\nL:1/8\nQ:1/4=110\nK:C\nCDEF GABc|c2B2 A2G2|".into()),
        ..Default::default()
    };
    p.semantic_sampling.min_tokens = 50;
    p.semantic_sampling.max_tokens = 300;
    let mut started = false;
    let mut paths_checked = false;
    let end = std::time::Instant::now() + Duration::from_secs(900);
    let song = loop {
        assert!(std::time::Instant::now() < end, "timed out");
        let Some(e) = core.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        match e {
            Event::ServerStatus(_, st) if st.is_up() && !started => {
                started = true;
                core.send(Command::StartRun(RunSpec {
                    name: RunName::parse("smoke").unwrap(),
                    params: p.clone(),
                    start_seed: 1234,
                    count: Some(1),
                    model: model.clone(),
                    abc_source: AbcSource::Manual,
                }));
            }
            Event::ModelPaths(_, problems) => {
                assert!(problems.is_empty(), "{problems:?}");
                paths_checked = true;
            }
            Event::JobUpdate(_, j) => {
                eprintln!("job: {:?}", j.state);
                match j.state {
                    JobState::Done(id) => break id,
                    JobState::Failed(e) => panic!("job failed: {e}"),
                    _ => {}
                }
            }
            Event::Log(l) => eprintln!("[{:?}] {}: {}", l.level, l.source, l.message),
            _ => {}
        }
    };
    assert!(paths_checked, "model paths were checked");
    let dir = root.join("unreviewed").join("smoke-1234");
    let mp3 = dir.join("smoke-1234.mp3");
    assert!(dir.join("smoke-1234.wav").is_file());
    let meta = incremusic_core::media::read_meta(&mp3).unwrap();
    let recipe = meta.recipe.unwrap();
    assert_eq!((recipe.run.name.as_str(), recipe.run.seed), ("smoke", 1234));
    assert_eq!(recipe.server.backend.as_deref(), Some("vulkan"));
    assert!(recipe.timing.wall_ms > 0);
    eprintln!("song {song:?} at {}", mp3.display());
    core.shutdown(false);
}

/// Reference audio → WAV → upload → SheetSage2 → ABC through the core, then SheetSage2 is
/// unloaded again (§5.2).
#[test]
#[ignore]
fn transcribes_through_the_core() {
    use incremusic_core::config::{Config, ServerConfig};
    use incremusic_core::run::RunName;
    use incremusic_core::service::{CoreHandle, CoreOptions, Event};
    use incremusic_core::{Command, CoreBackend};

    let Ok(url) = std::env::var("AUDIOCPP_SERVER_URL") else {
        eprintln!("AUDIOCPP_SERVER_URL not set");
        return;
    };
    let port: u16 = url
        .rsplit(':')
        .next()
        .unwrap()
        .trim_end_matches('/')
        .parse()
        .unwrap();
    let audio = std::env::var("INCREMUSIC_SMOKE_AUDIO")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tiny.mp3")
        });
    let tmp = tempfile::tempdir().unwrap();
    let root = std::env::var("INCREMUSIC_SMOKE_LIBRARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let mut cfg = Config::parse(include_str!("../../../config.example.ron")).unwrap();
    cfg.servers = vec![ServerConfig {
        name: "real".into(),
        port,
        launch: None,
    }];
    cfg.library.root = root.clone();
    cfg.defaults = None;
    let core = CoreHandle::start(cfg, CoreOptions::default()).unwrap();

    let mut sent = false;
    let end = std::time::Instant::now() + Duration::from_secs(900);
    let score = loop {
        assert!(std::time::Instant::now() < end, "timed out");
        let Some(e) = core.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        match e {
            Event::ServerStatus(_, st) if st.is_up() && !sent => {
                sent = true;
                core.send(Command::Transcribe {
                    project: RunName::parse("transcribe-smoke").unwrap(),
                    audio: audio.clone(),
                    force: true,
                });
            }
            Event::TranscribeDone(r) => break r.expect("transcription"),
            Event::Log(l) => eprintln!("[{:?}] {}: {}", l.level, l.source, l.message),
            _ => {}
        }
    };
    eprintln!(
        "--- ABC ({} ms) ---\n{}",
        score.wall_ms.unwrap_or(0),
        score.abc
    );
    assert!(!score.reused);
    assert!(score.abc.contains("X:") && score.abc.contains("K:"));
    let dir = root.join("inputs").join("transcribe-smoke");
    for f in [
        "project.ron",
        "transcription.abc",
        "transcription.events.json",
    ] {
        assert!(dir.join(f).is_file(), "{f}");
    }

    // unloaded after the transcription, before any YuE2 job
    let c = AudioCppClient::new(url, Duration::from_secs(30));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let models = rt.block_on(c.list_models()).unwrap();
    assert!(
        !models.iter().any(|m| m.id == "sheetsage2" && m.loaded),
        "{models:?}"
    );
    core.shutdown(false);
}
