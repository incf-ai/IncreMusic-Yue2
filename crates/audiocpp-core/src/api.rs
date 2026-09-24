//! Typed client for the audio.cpp server endpoints (design §1).

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::ModelSpec;
use crate::error::{Error, IoContext, Result};
use crate::wav::{self, WavInfo};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub models: Option<u32>,
    #[serde(default)]
    pub ui: Option<bool>,
    #[serde(default)]
    pub ui_management: Option<bool>,
}

impl Health {
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub loaded: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub session_options: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelInfo>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct LoadResponse {
    pub id: String,
    pub loaded: bool,
    #[serde(default)]
    pub reconfigured: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct UploadResponse {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Timing {
    pub wall_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtf: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Artifact {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TranscribeResponse {
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub timing: Timing,
}

impl TranscribeResponse {
    pub fn artifact(&self, id: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PathStatus {
    pub exists: bool,
    #[serde(default)]
    pub directory: bool,
    #[serde(default)]
    pub file: bool,
}

/// Result of a YuE2 generation whose audio was stream-decoded to a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    pub sample_rate: u32,
    pub channels: u16,
    pub timing: Timing,
    pub wav: WavInfo,
    pub wav_sha256: String,
    pub wav_bytes: u64,
    /// `sample_rate`/`channels` fields disagree with the WAV header (header wins, §1.4).
    pub header_mismatch: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GenerateOutcome {
    Done(Generated),
    /// The job was cancelled while running; the response was drained and thrown away.
    Discarded,
}

#[derive(Clone)]
pub struct AudioCppClient {
    base: String,
    http: reqwest::Client,
    timeout: Duration,
}

impl std::fmt::Debug for AudioCppClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioCppClient")
            .field("base", &self.base)
            .finish()
    }
}

const SHORT: Duration = Duration::from_secs(10);

impl AudioCppClient {
    pub fn new(base_url: impl Into<String>, task_timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .expect("reqwest client");
        AudioCppClient {
            base: base_url.into().trim_end_matches('/').to_string(),
            http,
            timeout: task_timeout,
        }
    }

    pub fn local(port: u16, task_timeout: Duration) -> Self {
        Self::new(format!("http://127.0.0.1:{port}"), task_timeout)
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(Error::Http {
            status: status.as_u16(),
            body: body.chars().take(2000).collect(),
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        timeout: Duration,
    ) -> Result<T> {
        let resp = self
            .http
            .get(self.url(path))
            .timeout(timeout)
            .send()
            .await
            .map_err(map_err)?;
        let resp = Self::check(resp).await?;
        resp.json().await.map_err(map_err)
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<T> {
        let resp = self
            .http
            .post(self.url(path))
            .json(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(map_err)?;
        let resp = Self::check(resp).await?;
        resp.json().await.map_err(map_err)
    }

    pub async fn health(&self) -> Result<Health> {
        self.get_json("/health", Duration::from_secs(3)).await
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let l: ModelList = self
            .get_json("/v1/models?include_session_options=true", SHORT)
            .await?;
        Ok(l.data)
    }

    pub fn load_body(spec: &ModelSpec) -> Value {
        json!({
            "id": spec.id,
            "path": spec.path,
            "family": spec.family,
            "task": spec.task,
            "mode": spec.mode,
            "load_options": spec.load_options,
            "session_options": spec.session_options,
        })
    }

    /// Loading can take a while (model files are large), so it uses the task timeout.
    pub async fn load_model(&self, spec: &ModelSpec) -> Result<LoadResponse> {
        self.post_json("/v1/models/load", &Self::load_body(spec), self.timeout)
            .await
    }

    pub async fn unload_model(&self, id: &str) -> Result<LoadResponse> {
        self.post_json(
            "/v1/models/unload",
            &json!({ "id": id }),
            Duration::from_secs(120),
        )
        .await
    }

    pub async fn models_root(&self) -> Result<String> {
        let v: Value = self.get_json("/v1/ui/models-root", SHORT).await?;
        v.get("models_root")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::BadResponse("models_root missing".into()))
    }

    pub async fn path_status(&self, path: &str) -> Result<PathStatus> {
        self.post_json("/v1/ui/path-status", &json!({ "path": path }), SHORT)
            .await
    }

    /// `POST /v1/ui/upload` with the captured headers (§1.3). Only WAV is accepted.
    pub async fn upload_wav(&self, path: &Path) -> Result<UploadResponse> {
        let bytes = tokio::fs::read(path).await.at(path)?;
        if !wav::has_wav_magic(&bytes) {
            return Err(Error::Other(format!(
                "{} is not a WAV file",
                path.display()
            )));
        }
        let resp = self
            .http
            .post(self.url("/v1/ui/upload"))
            .header("Content-Type", "audio/vnd.wave")
            .header("x-audiocpp-filename", "upload.wav")
            .body(bytes)
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .map_err(map_err)?;
        Self::check(resp).await?.json().await.map_err(map_err)
    }

    /// SheetSage2 `audio → ABC` (§1.3).
    pub async fn transcribe(
        &self,
        model_id: &str,
        server_audio_path: &str,
    ) -> Result<TranscribeResponse> {
        self.post_json(
            "/v1/tasks/run",
            &Self::transcribe_body(model_id, server_audio_path),
            self.timeout,
        )
        .await
    }

    pub fn transcribe_body(model_id: &str, server_audio_path: &str) -> Value {
        json!({ "model": model_id, "request": { "audio": server_audio_path, "options": {} } })
    }

    /// YuE2 generation. The base64 `audio` field is decoded as it streams in, straight into
    /// `out_wav` (§1.4). If `cancelled()` turns true, the rest of the response is still read
    /// (the connection stays open until the server is done) but nothing is written.
    pub async fn generate(
        &self,
        body: &Value,
        out_wav: &Path,
        cancelled: impl Fn() -> bool,
    ) -> Result<GenerateOutcome> {
        let resp = self
            .http
            .post(self.url("/v1/tasks/run"))
            .json(body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(map_err)?;
        let resp = Self::check(resp).await?;
        let mut stream = resp.bytes_stream();
        let mut extractor: Option<AudioFieldExtractor<std::io::BufWriter<std::fs::File>>> = None;
        let mut discarding = cancelled();
        if !discarding {
            let f = std::fs::File::create(out_wav).at(out_wav)?;
            extractor = Some(AudioFieldExtractor::new(std::io::BufWriter::with_capacity(
                1 << 20,
                f,
            )));
        }
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_err)?;
            if !discarding && cancelled() {
                discarding = true;
                extractor = None;
                let _ = std::fs::remove_file(out_wav);
            }
            if let Some(x) = extractor.as_mut() {
                x.feed(&chunk).map_err(|e| e.with_path(out_wav))?;
            }
        }
        let Some(x) = extractor else {
            return Ok(GenerateOutcome::Discarded);
        };
        let done = x.finish().map_err(|e| e.with_path(out_wav))?;
        Ok(GenerateOutcome::Done(done.into_generated()?))
    }
}

fn map_err(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout
    } else if let Some(s) = e.status() {
        Error::Http {
            status: s.as_u16(),
            body: e.to_string(),
        }
    } else if e.is_decode() {
        Error::BadResponse(e.to_string())
    } else {
        Error::Transport(format!("{e:#}"))
    }
}

trait WithPath {
    fn with_path(self, p: &Path) -> Error;
}

impl WithPath for Error {
    fn with_path(self, p: &Path) -> Error {
        match self {
            Error::IoPlain(e) => Error::io(p, e),
            other => other,
        }
    }
}

/// Streaming JSON filter: copies the response JSON into memory *except* the string value of
/// the top-level `"audio"` key, which is base64-decoded on the fly into `out`.
pub struct AudioFieldExtractor<W: Write> {
    out: W,
    rest: Vec<u8>,
    depth: u32,
    in_string: bool,
    escape: bool,
    expect_key: bool,
    collecting_key: bool,
    key: Vec<u8>,
    last_key: Vec<u8>,
    after_colon: bool,
    in_audio: bool,
    audio_seen: bool,
    b64: Vec<u8>,
    head: Vec<u8>,
    hasher: Sha256,
    written: u64,
}

pub struct Extracted {
    pub json: Value,
    pub head: Vec<u8>,
    pub sha256: String,
    pub bytes: u64,
}

const HEAD_KEEP: usize = 64 * 1024;
const B64_BLOCK: usize = 256 * 1024;

impl<W: Write> AudioFieldExtractor<W> {
    pub fn new(out: W) -> Self {
        AudioFieldExtractor {
            out,
            rest: Vec::new(),
            depth: 0,
            in_string: false,
            escape: false,
            expect_key: false,
            collecting_key: false,
            key: Vec::new(),
            last_key: Vec::new(),
            after_colon: false,
            in_audio: false,
            audio_seen: false,
            b64: Vec::with_capacity(B64_BLOCK + 4),
            head: Vec::new(),
            hasher: Sha256::new(),
            written: 0,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Result<()> {
        let mut i = 0;
        while i < chunk.len() {
            if self.in_audio {
                // fast path: scan to the closing quote
                while i < chunk.len() {
                    let c = chunk[i];
                    if self.escape {
                        self.escape = false;
                        match c {
                            b'/' => self.b64.push(b'/'),
                            b'n' | b'r' | b't' => {}
                            _ => {
                                return Err(Error::BadResponse(
                                    "unexpected escape in audio".into(),
                                ));
                            }
                        }
                    } else if c == b'\\' {
                        self.escape = true;
                    } else if c == b'"' {
                        break;
                    } else if !c.is_ascii_whitespace() {
                        self.b64.push(c);
                    }
                    i += 1;
                    if self.b64.len() >= B64_BLOCK {
                        self.flush_b64(false)?;
                    }
                }
                if i < chunk.len() {
                    // closing quote
                    self.flush_b64(true)?;
                    self.in_audio = false;
                    self.in_string = false;
                    self.rest.push(b'"');
                    i += 1;
                }
                continue;
            }
            let c = chunk[i];
            i += 1;
            if self.in_string {
                self.rest.push(c);
                if self.escape {
                    self.escape = false;
                    if self.collecting_key {
                        self.key.push(c);
                    }
                } else if c == b'\\' {
                    self.escape = true;
                } else if c == b'"' {
                    self.in_string = false;
                    if self.collecting_key {
                        self.collecting_key = false;
                        self.last_key = std::mem::take(&mut self.key);
                    }
                } else if self.collecting_key && self.key.len() < 64 {
                    self.key.push(c);
                }
                continue;
            }
            match c {
                b'"' => {
                    self.in_string = true;
                    if self.depth == 1 && self.expect_key {
                        self.collecting_key = true;
                        self.expect_key = false;
                        self.key.clear();
                        self.rest.push(c);
                    } else if self.depth == 1
                        && self.after_colon
                        && self.last_key == b"audio"
                        && !self.audio_seen
                    {
                        self.in_audio = true;
                        self.audio_seen = true;
                        self.rest.push(c);
                    } else {
                        self.rest.push(c);
                    }
                    self.after_colon = false;
                }
                b'{' | b'[' => {
                    self.depth += 1;
                    self.expect_key = c == b'{' && self.depth == 1;
                    self.after_colon = false;
                    self.rest.push(c);
                }
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    self.after_colon = false;
                    self.rest.push(c);
                }
                b':' => {
                    if self.depth == 1 {
                        self.after_colon = true;
                    }
                    self.rest.push(c);
                }
                b',' => {
                    if self.depth == 1 {
                        self.expect_key = true;
                    }
                    self.after_colon = false;
                    self.rest.push(c);
                }
                _ => {
                    if !c.is_ascii_whitespace() {
                        self.after_colon = false;
                    }
                    self.rest.push(c);
                }
            }
        }
        Ok(())
    }

    fn flush_b64(&mut self, last: bool) -> Result<()> {
        let n = if last {
            self.b64.len()
        } else {
            self.b64.len() / 4 * 4
        };
        if n == 0 {
            return Ok(());
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&self.b64[..n])
            .map_err(|e| Error::BadResponse(format!("audio base64: {e}")))?;
        self.b64.drain(..n);
        if self.head.len() < HEAD_KEEP {
            let take = (HEAD_KEEP - self.head.len()).min(decoded.len());
            self.head.extend_from_slice(&decoded[..take]);
        }
        self.hasher.update(&decoded);
        self.out.write_all(&decoded)?;
        self.written += decoded.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> Result<Extracted> {
        if self.in_audio || self.in_string {
            return Err(Error::BadResponse("response ended inside a string".into()));
        }
        self.out.flush()?;
        let json: Value = serde_json::from_slice(&self.rest)
            .map_err(|e| Error::BadResponse(format!("response JSON: {e}")))?;
        if !self.audio_seen {
            return Err(Error::BadResponse("response has no `audio` field".into()));
        }
        Ok(Extracted {
            json,
            head: self.head,
            sha256: crate::fsutil::hex(&self.hasher.finalize()),
            bytes: self.written,
        })
    }
}

impl Extracted {
    pub fn into_generated(self) -> Result<Generated> {
        let wav = wav::parse_header(&self.head)?;
        let sample_rate = self
            .json
            .get("sample_rate")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let channels = self
            .json
            .get("channels")
            .and_then(Value::as_u64)
            .map(|v| v as u16);
        let timing: Timing = self
            .json
            .get("timing")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| Error::BadResponse(format!("timing: {e}")))?
            .unwrap_or_default();
        let mut mismatch = None;
        if sample_rate.is_some_and(|r| r != wav.sample_rate)
            || channels.is_some_and(|c| c != wav.channels)
        {
            let m = format!(
                "response says {} Hz / {} ch, WAV header says {} Hz / {} ch; using the header",
                sample_rate.unwrap_or(0),
                channels.unwrap_or(0),
                wav.sample_rate,
                wav.channels
            );
            tracing::warn!("{m}");
            mismatch = Some(m);
        }
        Ok(Generated {
            sample_rate: wav.sample_rate,
            channels: wav.channels,
            timing,
            wav,
            wav_sha256: self.sha256,
            wav_bytes: self.bytes,
            header_mismatch: mismatch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const RESPONSE: &str = include_str!("../../../tests/fixtures/yue2_response.json");
    const WAV: &[u8] = include_bytes!("../../../tests/fixtures/one_second.wav");

    fn run_chunks(input: &[u8], chunk: usize) -> (Vec<u8>, Extracted) {
        let mut out = Vec::new();
        let mut x = AudioFieldExtractor::new(&mut out);
        for c in input.chunks(chunk) {
            x.feed(c).unwrap();
        }
        let e = x.finish().unwrap();
        (out, e)
    }

    #[test]
    fn stream_decodes_audio_any_chunking() {
        for chunk in [1, 3, 7, 1000, 65536, 1 << 20] {
            let (out, e) = run_chunks(RESPONSE.as_bytes(), chunk);
            assert_eq!(out, WAV, "chunk size {chunk}");
            assert_eq!(e.bytes, WAV.len() as u64);
            assert_eq!(e.sha256, crate::fsutil::sha256_hex(WAV));
            assert_eq!(e.json["audio"], "");
            assert_eq!(e.json["timing"]["wall_ms"], 155962);
        }
    }

    #[test]
    fn parses_generated_fields() {
        let (_, e) = run_chunks(RESPONSE.as_bytes(), 4096);
        let g = e.into_generated().unwrap();
        assert_eq!(g.sample_rate, 48000);
        assert_eq!(g.channels, 2);
        assert_eq!(
            g.timing,
            Timing {
                wall_ms: 155962,
                audio_duration_ms: Some(1000),
                rtf: Some(0.56013)
            }
        );
        assert_eq!(g.header_mismatch, None);
    }

    #[test]
    fn detects_header_mismatch() {
        let mut v: Value = serde_json::from_str(RESPONSE).unwrap();
        v["sample_rate"] = json!(44100);
        let text = serde_json::to_string(&v).unwrap();
        let (_, e) = run_chunks(text.as_bytes(), 999);
        let g = e.into_generated().unwrap();
        assert_eq!(g.sample_rate, 48000, "header wins");
        assert!(g.header_mismatch.unwrap().contains("44100"));
    }

    #[test]
    fn handles_key_order_nesting_and_escaped_slashes() {
        let b64 = base64::engine::general_purpose::STANDARD
            .encode(WAV)
            .replace('/', "\\/");
        let text = format!(
            r#"{{ "timing" : {{"wall_ms": 5, "audio": "nested-not-audio"}}, "note":"a \"quoted\" audio",
               "audio" : "{b64}", "sample_rate":48000, "channels":2 }}"#
        );
        let (out, e) = run_chunks(text.as_bytes(), 5);
        assert_eq!(out, WAV);
        assert_eq!(e.json["timing"]["audio"], "nested-not-audio");
        assert_eq!(e.json["note"], "a \"quoted\" audio");
    }

    #[test]
    fn missing_audio_is_an_error() {
        let mut out = Vec::new();
        let mut x = AudioFieldExtractor::new(&mut out);
        x.feed(br#"{"error":"x"}"#).unwrap();
        assert!(x.finish().is_err());
    }

    #[test]
    fn load_body_matches_har() {
        let spec = ModelSpec {
            id: "yue2".into(),
            family: "yue2".into(),
            task: "gen".into(),
            mode: "offline".into(),
            path: "/mnt/storage/audiocpp/models/Yue2-3B-GGUF".into(),
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
        let har: Value = serde_json::from_str(
            r#"{"id":"yue2","path":"/mnt/storage/audiocpp/models/Yue2-3B-GGUF","family":"yue2","task":"gen","mode":"offline","load_options":{},"session_options":{"yue2.ar_lora_scale":"1","yue2.nar_lora_scale":"1","yue2.model_gguf":"yue2-3b-bf16.gguf","yue2.vae_gguf":"yue2-vae-f32.gguf"}}"#,
        )
        .unwrap();
        assert_eq!(AudioCppClient::load_body(&spec), har);
    }

    #[test]
    fn parses_model_list_and_transcription() {
        let l: ModelList = serde_json::from_str(include_str!(
            "../../../tests/fixtures/models_list_yue2.json"
        ))
        .unwrap();
        assert!(l.data[0].loaded);
        assert_eq!(
            l.data[0].session_options.as_ref().unwrap()["yue2.vae_gguf"],
            "yue2-vae-f32.gguf"
        );
        let t: TranscribeResponse = serde_json::from_str(include_str!(
            "../../../tests/fixtures/sheetsage2_response.json"
        ))
        .unwrap();
        assert!(t.text.starts_with("X:1"));
        assert_eq!(t.timing.wall_ms, 10513);
        assert_eq!(t.artifact("score").unwrap().meta["format"], "abc");
        assert!(t.artifact("events").is_some());
    }

    #[test]
    fn transcribe_request_matches_har() {
        let har: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/sheetsage2_request.json"
        ))
        .unwrap();
        let path = har["request"]["audio"].as_str().unwrap();
        assert_eq!(AudioCppClient::transcribe_body("sheetsage2", path), har);
    }
}
