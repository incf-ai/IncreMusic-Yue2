//! Test support: a mock audio.cpp server and a harness around `CoreHandle`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use audiocpp_core::config::{
    Config, Encoder, Extension, LibraryConfig, ModelSpec, Models, ServerConfig, TerminalMode,
};
use audiocpp_core::run::JobId;
use audiocpp_core::scheduler::{JobInfo, JobState, RunSnapshot, ServerId};
use audiocpp_core::service::{CoreHandle, CoreOptions, Event, ServerState};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};

pub const SHEETSAGE_RESPONSE: &str =
    include_str!("../../../../tests/fixtures/sheetsage2_response.json");

pub fn has_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
}

/// Skips a test when ffmpeg is missing, unless `AUDIOCPP_REQUIRE_FFMPEG` is set.
#[macro_export]
macro_rules! require_ffmpeg {
    () => {
        if !common::has_ffmpeg() {
            if std::env::var_os("AUDIOCPP_REQUIRE_FFMPEG").is_some() {
                panic!("ffmpeg is required");
            }
            eprintln!("skipping: ffmpeg not found");
            return;
        }
    };
}

/// A short WAV unique to `seed`, so outputs can be matched byte for byte.
pub fn wav_for_seed(seed: u64) -> Vec<u8> {
    let rate = 48000u32;
    let frames = 4800usize; // 0.1 s
    let freq = 200.0 + (seed % 1000) as f64;
    let mut s = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = ((i as f64 * freq * std::f64::consts::TAU / rate as f64).sin() * 8000.0) as i16;
        s.push(v);
        s.push(v.wrapping_add((seed & 0xff) as i16));
    }
    audiocpp_core::wav::encode_pcm16(&s, 2, rate)
}

/// (path, session_options) of a loaded model.
pub type LoadedModel = (String, BTreeMap<String, String>);

#[derive(Default)]
pub struct MockState {
    pub name: String,
    /// id → session_options of loaded models
    pub loaded: Mutex<BTreeMap<String, LoadedModel>>,
    pub calls: Mutex<Vec<String>>,
    pub gen_delay: Mutex<Duration>,
    pub transcribe_delay: Mutex<Duration>,
    pub fail_transcribe: AtomicBool,
    /// Respond 500 to the next N generation requests.
    pub fail_next: AtomicU32,
    /// Respond 400 to generation requests.
    pub reject_params: AtomicBool,
    /// "Crash" during the next generation: all connections drop, listener closes.
    pub crash_next: AtomicBool,
    pub dead: AtomicBool,
    pub uploads: Mutex<Vec<PathBuf>>,
    pub upload_dir: PathBuf,
    pub gen_count: AtomicU32,
    /// Generation requests currently in flight.
    pub in_flight: AtomicU32,
    pub max_in_flight: AtomicU32,
}

impl MockState {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }
    fn call(&self, c: impl Into<String>) {
        self.calls.lock().push(c.into());
    }
}

pub struct MockServer {
    pub port: u16,
    pub state: Arc<MockState>,
    task: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

type S = State<Arc<MockState>>;

fn check_alive(st: &MockState) {
    if st.dead.load(Ordering::SeqCst) {
        // drops the connection without a response, like a crashed process
        panic!("mock server `{}` is dead", st.name);
    }
}

async fn health(State(st): S) -> Response {
    check_alive(&st);
    let n = st.loaded.lock().len();
    axum::Json(json!({"status":"ok","backend":"mock","models":n,"ui":true,"ui_management":true}))
        .into_response()
}

async fn models(State(st): S) -> Response {
    check_alive(&st);
    let data: Vec<Value> = st
        .loaded
        .lock()
        .iter()
        .map(|(id, (path, opts))| json!({"id": id, "object":"model", "loaded": true, "path": path, "session_options": opts}))
        .collect();
    axum::Json(json!({"object":"list","data":data})).into_response()
}

async fn load(State(st): S, axum::Json(body): axum::Json<Value>) -> Response {
    check_alive(&st);
    let id = body["id"].as_str().unwrap_or_default().to_string();
    let opts: BTreeMap<String, String> =
        serde_json::from_value(body["session_options"].clone()).unwrap_or_default();
    st.call(format!("load {id}"));
    let path = body["path"].as_str().unwrap_or_default().to_string();
    st.loaded.lock().insert(id.clone(), (path, opts));
    axum::Json(json!({"id": id, "loaded": true, "reconfigured": false})).into_response()
}

async fn unload(State(st): S, axum::Json(body): axum::Json<Value>) -> Response {
    check_alive(&st);
    let id = body["id"].as_str().unwrap_or_default().to_string();
    st.call(format!("unload {id}"));
    st.loaded.lock().remove(&id);
    axum::Json(json!({"id": id, "loaded": false})).into_response()
}

async fn upload(State(st): S, headers: HeaderMap, body: Bytes) -> Response {
    check_alive(&st);
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let name = headers
        .get("x-audiocpp-filename")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let is_wav = body.len() >= 12 && &body[0..4] == b"RIFF" && &body[8..12] == b"WAVE";
    if ct != "audio/vnd.wave" || name != "upload.wav" || !is_wav {
        st.call("upload rejected");
        return (
            StatusCode::BAD_REQUEST,
            format!("only WAV accepted (ct={ct}, name={name}, wav={is_wav})"),
        )
            .into_response();
    }
    let n = st.uploads.lock().len() + 1;
    let path = st.upload_dir.join(format!("{n}-upload.wav"));
    std::fs::write(&path, &body).unwrap();
    st.uploads.lock().push(path.clone());
    st.call("upload");
    axum::Json(json!({"path": path, "bytes": body.len()})).into_response()
}

struct InFlight<'a>(&'a MockState);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn run(State(st): S, axum::Json(body): axum::Json<Value>) -> Response {
    check_alive(&st);
    let model = body["model"].as_str().unwrap_or_default().to_string();
    if !st.loaded.lock().contains_key(&model) {
        st.call(format!("run {model} (not loaded)"));
        return (StatusCode::BAD_REQUEST, format!("model {model} not loaded")).into_response();
    }
    if model == "sheetsage2" {
        let audio = body["request"]["audio"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        st.call("run sheetsage2");
        let d = *st.transcribe_delay.lock();
        tokio::time::sleep(d).await;
        if st.fail_transcribe.load(Ordering::SeqCst) {
            return (StatusCode::INTERNAL_SERVER_ERROR, "transcription failed").into_response();
        }
        if !Path::new(&audio).is_file() {
            return (StatusCode::BAD_REQUEST, "audio path not found").into_response();
        }
        return (
            StatusCode::OK,
            [("content-type", "application/json")],
            SHEETSAGE_RESPONSE,
        )
            .into_response();
    }
    let seed = body["request"]["seed"].as_u64().unwrap_or(0);
    st.call(format!("run {model} seed={seed}"));
    let n = st.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    st.max_in_flight.fetch_max(n, Ordering::SeqCst);
    let _guard = InFlight(&st);
    if st.reject_params.load(Ordering::SeqCst) {
        return (StatusCode::BAD_REQUEST, "bad parameter").into_response();
    }
    if st.crash_next.swap(false, Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        st.dead.store(true, Ordering::SeqCst);
        check_alive(&st);
    }
    let d = *st.gen_delay.lock();
    tokio::time::sleep(d).await;
    check_alive(&st);
    if st.fail_next.load(Ordering::SeqCst) > 0 {
        st.fail_next.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response();
    }
    st.gen_count.fetch_add(1, Ordering::SeqCst);
    let wav = wav_for_seed(seed);
    use base64::Engine as _;
    let resp = json!({
        "audio": base64::engine::general_purpose::STANDARD.encode(&wav),
        "sample_rate": 48000,
        "channels": 2,
        "timing": {"wall_ms": d.as_millis() as u64 + 1, "audio_duration_ms": 100, "rtf": (d.as_millis() as f64 + 1.0) / 100.0},
    });
    axum::Json(resp).into_response()
}

/// Mock servers run on their own runtime so tests can block freely.
pub fn rt() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
    })
}

impl MockServer {
    pub fn start(name: &str) -> MockServer {
        Self::start_on(name, 0)
    }

    pub fn start_on(name: &str, port: u16) -> MockServer {
        rt().block_on(Self::start_async(name, port))
    }

    async fn start_async(name: &str, port: u16) -> MockServer {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(MockState {
            name: name.into(),
            upload_dir: dir.path().to_path_buf(),
            ..Default::default()
        });
        let app = Router::new()
            .route("/health", get(health))
            .route("/v1/models", get(models))
            .route("/v1/models/load", post(load))
            .route("/v1/models/unload", post(unload))
            .route("/v1/ui/upload", post(upload))
            .route("/v1/tasks/run", post(run))
            .layer(axum::extract::DefaultBodyLimit::disable())
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        MockServer {
            port,
            state,
            task,
            _dir: dir,
        }
    }

    /// Simulates a crash: in-flight and pooled connections drop, the port closes.
    pub fn kill(&self) {
        self.state.dead.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn model(id: &str, family: &str, task: &str) -> ModelSpec {
    ModelSpec {
        id: id.into(),
        family: family.into(),
        task: task.into(),
        mode: "offline".into(),
        path: format!("/models/{id}"),
        load_options: Default::default(),
        session_options: [("k".to_string(), "1".to_string())].into(),
    }
}

pub fn config(root: &Path, ports: &[u16]) -> Config {
    Config {
        server_binary: None,
        working_dir: None,
        terminal: TerminalMode::Headless,
        macos_terminal_app: None,
        request_timeout_secs: 30,
        ffmpeg: None,
        servers: ports
            .iter()
            .enumerate()
            .map(|(i, p)| ServerConfig {
                name: format!("gpu{}", i + 1),
                port: *p,
                launch: None,
            })
            .collect(),
        models: Models {
            yue2: model("yue2", "yue2", "gen"),
            sheetsage2: model("sheetsage2", "sheetsage2", "midi"),
        },
        library: LibraryConfig {
            root: root.to_path_buf(),
            encoder: Encoder::Aac { bitrate_kbps: 128 },
            extension: Extension::Mp4,
        },
        defaults: None,
    }
}

/// Collects events from a running core and tracks the latest job/server/run state.
pub struct Harness {
    pub core: CoreHandle,
    pub events: Vec<Event>,
    pub jobs: BTreeMap<JobId, JobInfo>,
    pub servers: BTreeMap<ServerId, ServerState>,
    pub runs: Vec<RunSnapshot>,
    /// Every state each job went through, in order.
    pub job_history: BTreeMap<JobId, Vec<JobState>>,
    pub server_history: BTreeMap<ServerId, Vec<ServerState>>,
}

impl Harness {
    pub fn start(cfg: Config) -> Harness {
        Self::start_with(
            cfg,
            CoreOptions {
                no_autostart: true,
                poll: Some(Duration::from_millis(200)),
                ..Default::default()
            },
        )
    }

    pub fn start_with(cfg: Config, opts: CoreOptions) -> Harness {
        let core = CoreHandle::start(cfg, opts).unwrap();
        Harness {
            core,
            events: vec![],
            jobs: BTreeMap::new(),
            servers: BTreeMap::new(),
            runs: vec![],
            job_history: BTreeMap::new(),
            server_history: BTreeMap::new(),
        }
    }

    fn absorb(&mut self, e: Event) {
        match &e {
            Event::JobUpdate(id, info) => {
                self.jobs.insert(*id, info.clone());
                let h = self.job_history.entry(*id).or_default();
                if h.last() != Some(&info.state) {
                    h.push(info.state.clone());
                }
            }
            Event::ServerStatus(id, s) => {
                self.servers.insert(*id, s.clone());
                self.server_history.entry(*id).or_default().push(s.clone());
            }
            Event::RunUpdate(r) => {
                self.runs.retain(|x| x.state.id != r.state.id);
                self.runs.push(r.clone());
            }
            Event::Log(l) => eprintln!("[{:?}] {}: {}", l.level, l.source, l.message),
            _ => {}
        }
        self.events.push(e);
    }

    /// Pumps events until `pred(self)` holds or the timeout passes (then panics).
    pub fn until(&mut self, what: &str, timeout: Duration, mut pred: impl FnMut(&Harness) -> bool) {
        let end = Instant::now() + timeout;
        loop {
            if pred(self) {
                return;
            }
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                panic!(
                    "timed out waiting for: {what}\njobs: {:#?}\nservers: {:#?}",
                    self.jobs, self.servers
                );
            };
            if let Some(e) = self.core.recv_timeout(left.min(Duration::from_millis(50))) {
                self.absorb(e);
            }
        }
    }

    /// Pumps events for a fixed time.
    pub fn pump(&mut self, d: Duration) {
        let end = Instant::now() + d;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Some(e) = self.core.recv_timeout(left) {
                self.absorb(e);
            }
        }
    }

    pub fn wait_ready(&mut self, n: usize) {
        self.until(
            &format!("{n} servers ready"),
            Duration::from_secs(10),
            |h| h.servers.values().filter(|s| s.is_up()).count() >= n,
        );
    }

    pub fn done_jobs(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| matches!(j.state, JobState::Done(_)))
            .count()
    }

    pub fn find<T>(&self, f: impl FnMut(&Event) -> Option<T>) -> Option<T> {
        self.events.iter().find_map(f)
    }

    pub fn send(&self, c: audiocpp_core::Command) {
        use audiocpp_core::CoreBackend;
        self.core.send(c);
    }
}
