//! Smoke test against a real audio.cpp server. Run with
//! `AUDIOCPP_SERVER_URL=http://127.0.0.1:8080 cargo test -p audiocpp-core --test real_server -- --ignored`.
//! Model paths come from `AUDIOCPP_MODELS_ROOT` (default: the server's `/v1/ui/models-root`).

use std::time::Duration;

use audiocpp_core::api::{AudioCppClient, GenerateOutcome};
use audiocpp_core::config::ModelSpec;
use audiocpp_core::models::ensure_loaded;
use audiocpp_core::params::GenerationParams;

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
        audiocpp_core::wav::read_header(&out).unwrap().sample_rate,
        g.sample_rate
    );
    assert!(g.timing.wall_ms > 0);
}
