//! Reference audio → WAV → SheetSage2 → ABC (design §5.2 "Transcription").

use std::future::Future;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::{AudioCppClient, TranscribeResponse};
use crate::config::ModelSpec;
use crate::error::{Error, Result};
use crate::media::Ffmpeg;
use crate::models;
use crate::project::ProjectStore;
use crate::run::{ReferenceAudio, RunName};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AbcScore {
    pub project: String,
    pub abc: String,
    pub reference: ReferenceAudio,
    /// Copied from an existing transcription; no server call was made.
    pub reused: bool,
    pub wall_ms: Option<u64>,
}

/// Runs the whole pipeline. `on_server` uploads and transcribes the WAV on whichever server
/// is idle (the scheduler decides) and returns the server's response.
pub async fn transcribe<F, Fut>(
    store: &ProjectStore,
    ffmpeg: &Ffmpeg,
    name: &RunName,
    audio: &Path,
    force: bool,
    on_server: F,
) -> Result<AbcScore>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<TranscribeResponse>>,
{
    let prepared = {
        let (store, ffmpeg, name, audio) = (
            store.clone(),
            ffmpeg.clone(),
            name.clone(),
            audio.to_path_buf(),
        );
        tokio::task::spawn_blocking(move || store.prepare_reference(&name, &audio, &ffmpeg))
            .await
            .map_err(|e| Error::Other(e.to_string()))??
    };
    if !force && let Some((from, _)) = store.find_transcription(&prepared.reference.sha256) {
        let abc = store.reuse_transcription(name.as_str(), &from)?;
        tracing::info!("reusing transcription from project `{}`", from.name);
        let mut reference = prepared.reference.clone();
        if let Some(t) = &from.transcription {
            reference.abc_model = t.model.clone();
        }
        return Ok(AbcScore {
            project: name.to_string(),
            abc,
            reference,
            reused: true,
            wall_ms: None,
        });
    }
    let resp = on_server(prepared.upload_path.clone()).await?;
    let events = resp.artifact("events").map(|a| a.payload.as_str());
    store.save_transcription(
        name.as_str(),
        &resp.text,
        events,
        &prepared.reference.abc_model,
        resp.timing.wall_ms,
    )?;
    Ok(AbcScore {
        project: name.to_string(),
        abc: resp.text,
        reference: prepared.reference,
        reused: false,
        wall_ms: Some(resp.timing.wall_ms),
    })
}

/// The server half, run by a worker: ensure SheetSage2 is loaded, upload, transcribe, and
/// **always** unload SheetSage2 afterwards, even on failure (§5.2).
pub async fn run_on_server(
    client: &AudioCppClient,
    spec: &ModelSpec,
    wav: &Path,
) -> Result<TranscribeResponse> {
    let result = async {
        models::ensure_loaded(client, spec).await?;
        let up = client.upload_wav(wav).await?;
        client.transcribe(&spec.id, &up.path).await
    }
    .await;
    if let Err(e) = client.unload_model(&spec.id).await {
        tracing::warn!("unloading {} failed: {e}", spec.id);
    }
    result
}
