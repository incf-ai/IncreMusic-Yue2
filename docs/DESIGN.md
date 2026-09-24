# audiocpp-ui — Design Document

Status: **Draft** · 2026-09-23

A desktop app, written in Rust with egui, for batch song generation with one or more
[audio.cpp](#1-background-the-audiocpp-server-api) servers. It also covers reviewing and
organizing the results. It:

1. Launches 0–2 local `audiocpp_server` instances, each in its own terminal, or connects
   to servers that are already running.
2. Can turn a reference track into ABC notation with **SheetSage2**.
3. Generates a **batch** of songs with **YuE2** from one set of parameters and a series of
   seeds, spreading the jobs across all available servers.
4. Encodes each result to **MP4 (AAC)** and embeds the full generation recipe in it so the
   song can be reproduced. Each file goes into `unreviewed/`.
5. Lets you play, seek, name, tag, rate, and export songs. Reviewed songs move to
   `reviewed/{good,neutral,bad}/`.

---

## 0. Goals / Non-goals

**Goals**

- Produce many generations without supervision, using every GPU available.
- Make every generated file reproducible on its own: the MP4 carries everything needed
  to regenerate it.
- Make review fast with the keyboard (play, rate, next).
- **Never block the UI** on a long-running operation (section 2.3.1). The window keeps
  repainting and responding to input while jobs run, files are encoded or scanned, servers
  start or stop, and dialogs are open.
- Build it as a library. Logic, networking, and I/O are testable without a GUI. The GUI is
  testable headlessly through AccessKit.

**Non-goals (v1)**

- Managing model downloads and installs. The audio.cpp built-in web UI already does this
  (`/v1/ui/models/*`).
- Remote or multi-user servers. Only local servers on `127.0.0.1` are assumed.
- Editing ABC or lyrics beyond a plain text box.
- Platform-specific polish beyond launching and paths. The app **is cross-platform**
  (Linux, Windows, macOS). Linux is the main target for testing, and Windows and macOS get
  CI build and unit-test coverage.

---

## 1. Background: the audio.cpp server API

Everything below comes from two captured browser sessions against the built-in UI
(`--ui --ui-management`). Note that the HAR file names are the reverse of what their
names might suggest:

| HAR file          | What it actually shows                          |
|-------------------|-------------------------------------------------|
| `sheetsage2.har`  | **Audio → ABC** transcription (SheetSage2, `task: "midi"`) |
| `yue2.har`        | **Song generation** from ABC + lyrics + style (YuE2, `task: "gen"`) |

### 1.1 Endpoints used

| Method | Path | Purpose |
|---|---|---|
| GET  | `/health` | Liveness and readiness. `{"status":"ok","backend":"vulkan","models":1,"ui":true,"ui_management":true}` |
| GET  | `/v1/models?include_session_options=true` | Lists registered models with their `loaded` flag and `session_options` |
| POST | `/v1/models/load` | Loads or registers a model (see below) |
| POST | `/v1/models/unload` | `{"id":"sheetsage2"}` → `{"id":"sheetsage2","loaded":false}` |
| POST | `/v1/ui/upload` | Raw body upload. The response gives a **server-local path** (needs `--ui-management`) |
| POST | `/v1/tasks/run` | Runs a task on a loaded model. It **blocks until done** and sends no progress events |
| GET  | `/v1/ui/models-root` | `{"models_root":"/mnt/storage/audiocpp/models",...}`. Used to resolve model paths |
| POST | `/v1/ui/path-status` | `{"path":...}` → `{"exists":true,"directory":true,"file":false}`. Used to validate config |
| GET  | `/v1/audio/voices` | Voice list (not needed for YuE2) |

### 1.2 Loading models

```jsonc
// POST /v1/models/load (YuE2)
{
  "id": "yue2",
  "path": "/mnt/storage/audiocpp/models/Yue2-3B-GGUF",
  "family": "yue2",
  "task": "gen",
  "mode": "offline",
  "load_options": {},
  "session_options": {
    "yue2.ar_lora_scale": "1",
    "yue2.nar_lora_scale": "1",
    "yue2.model_gguf": "yue2-3b-bf16.gguf",
    "yue2.vae_gguf": "yue2-vae-f32.gguf"
  }
}
// → {"id":"yue2","loaded":true,"reconfigured":false}

// POST /v1/models/load (SheetSage2)
{ "id": "sheetsage2",
  "path": "/mnt/storage/audiocpp/models/SheetSage2-GGUF/sheetsage2-orig.gguf",
  "family": "sheetsage2", "task": "midi", "mode": "offline",
  "load_options": {}, "session_options": {} }
```

Session option values are **strings**, even when they hold numbers.

### 1.3 Audio → ABC (SheetSage2)

audio.cpp accepts **only WAV** uploads. So the app converts every reference to WAV before
uploading it, whatever the user supplied (MP3, FLAC, OGG, M4A, … anything ffmpeg can
read). See section 5.2 for the conversion and where the files are kept.

1. `POST /v1/ui/upload`
   - Body: the raw bytes of the WAV. The headers are always the same (as captured):
     `Content-Type: audio/vnd.wave` and `x-audiocpp-filename: upload.wav`.
   - Response: `{"path":"/tmp/audiocpp-ui-…/1-upload.wav","bytes":47054892}`.
2. `POST /v1/tasks/run`:
   ```json
   {"model":"sheetsage2","request":{"audio":"/tmp/audiocpp-ui-…/1-upload.wav","options":{}}}
   ```
3. Response (took about 10.5 s for a 4.4-minute track):
   ```jsonc
   {
     "text": "X:1\nT:\nM:4/4\nL:1/32\nQ:1/4=94\nV: Vocal …\nK:Fm\n…",  // ABC
     "language": "abc",
     "artifacts": [
       { "id": "score",  "kind": "custom", "payload": "<abc>",
         "meta": { "format": "abc", "extension": "abc", "mime": "text/vnd.abc",
                   "tokens": "3387", "windows": "1", "memory_steps": "7500" } },
       { "id": "events", "kind": "custom", "payload": "<~280 KB>",
         "meta": { "format": "sheetsage2-events-json", "extension": "json",
                   "mime": "application/json" } }
     ],
     "timing": { "wall_ms": 10513 }
   }
   ```

### 1.4 Song generation (YuE2)

```jsonc
// POST /v1/tasks/run
{
  "model": "yue2",
  "request": {
    "lyrics": "[Pre-Chorus]\n…\n\n[Chorus]\n…\n\n[Outro]\n…",
    "seed": 1233,
    "options": {
      "style": "English, indie pop, bright acoustic guitar, soft drums, warm lead vocal, polished demo mix",
      "abc": "X:1\nT:\nM:4/4\n…",          // e.g. SheetSage2 output
      "cot": "full",
      "guidance_scale": 1.01,
      "num_inference_steps": 8,
      "abc_temperature": 0.7,  "abc_top_p": 0.9,  "abc_top_k": 30,
      "abc_repetition_penalty": 1.005, "abc_penalty_window": 100,
      "abc_min_tokens": 32,    "abc_max_tokens": 4096,
      "semantic_temperature": 1, "semantic_top_p": 0.95, "semantic_top_k": 100,
      "semantic_repetition_penalty": 1.2, "semantic_penalty_window": 50,
      "semantic_min_tokens": 200, "semantic_max_tokens": 9000
    }
  }
}
```

Response:

```jsonc
{
  "audio": "<base64 WAV: 71280360 chars>",   // RIFF/WAVE, PCM s16le
  "sample_rate": 48000,
  "channels": 2,
  "timing": {
    "wall_ms": 155962,
    "audio_duration_ms": 278439,
    "rtf": 0.56013                          // wall_ms / audio_duration_ms
  }
}
```

The `audio` field is a complete WAV file with its RIFF header, not raw PCM. The
`sample_rate` and `channels` fields repeat what the header says. Parse the header as the
source of truth, and log a warning if the two disagree. In the two runs seen, a
278 s song took 156 s (RTF 0.56), and a ~360 s song took 219 s. Implications:

- HTTP client timeouts must be long (default 30 min) and configurable.
- **A running task cannot be cancelled.** There is no cancel endpoint. The built-in web
  client "cancels" by closing the request. The server keeps computing anyway, and the GPU
  stays busy until the job finishes. Signals don't help either: SIGTERM, Ctrl-C, and even
  `kill -9` don't end the process until the current GPU job completes, which can take
  several minutes. The process is presumably stuck in an uninterruptible wait in the
  driver. The design treats a started job as something that **always runs to the end**
  (sections 4.2 and 5.3).
- Response bodies are around 100 MB. Decode the base64 as a stream, straight into a temp
  WAV file, instead of holding several copies in memory.
- The API has no progress events. The UI shows elapsed time and an ETA from each server's
  recent `rtf`. Song length is not known before the song is generated, so the first
  estimate uses the median `wall_ms` and then switches to rtf-based estimates as history
  builds up.
- A song's duration is available without decoding: `timing.audio_duration_ms`.

---

## 2. Architecture

### 2.1 Workspace layout

The current single-package `Cargo.toml` becomes a workspace:

```
audiocpp-ui/
├── Cargo.toml                 # [workspace]
├── crates/
│   ├── audiocpp-core/         # logic, network, process, media, library: no GUI deps
│   ├── audiocpp-gui-core/     # GUI state machine / view-model: no egui rendering
│   └── audiocpp-gui/          # egui/eframe widgets + the binary `audiocpp-ui`
├── tests/fixtures/            # trimmed HAR-derived request/response fixtures
└── docs/DESIGN.md
```

```
            ┌────────────────────┐
            │   audiocpp-gui     │  eframe app, egui widgets, audio output device
            │   (bin + lib)      │  headless tests: egui_kittest + AccessKit
            └─────────┬──────────┘
                      │ renders AppState, emits UiAction
            ┌─────────▼──────────┐
            │ audiocpp-gui-core  │  AppState, UiAction → reducer → Commands,
            │   (lib)            │  selection/review queue, form validation
            └─────────┬──────────┘
                      │ Command / Event channels
            ┌─────────▼──────────┐
            │  audiocpp-core     │  config, launcher, api client, scheduler,
            │   (lib, tokio)     │  media encode/metadata, library (fs), playback decode
            └────────────────────┘
```

**Dependency rule:** `core` must not depend on `gui-core` or `gui`, and `gui-core` must not
depend on `egui`. Everything above `core` talks to it only through a `CoreHandle`
(section 2.3).

### 2.2 `audiocpp-core` modules

| Module | Responsibility |
|---|---|
| `config` | RON config types, loading and validation (section 3) |
| `launcher` | Builds server command lines, opens terminals, tracks PIDs, polls health (section 4) |
| `api` | Typed `reqwest` client for the endpoints in section 1 (`AudioCppClient`) |
| `models` | `ensure_loaded(server, ModelSpec)`: compares against `/v1/models`, then loads, reloads, or does nothing |
| `project` | `inputs/<name>/` project folders: reference audio, WAV conversion, transcriptions, used ABC/lyrics/style, `project.ron` (section 5.2.1) |
| `transcribe` | Reference audio → convert to WAV → upload → SheetSage2 → `AbcScore`. Reuses an existing transcription when the audio hash matches |
| `run` | `RunSpec`/`RunState`: name validation, seed cursor, param revisions, handing out jobs |
| `scheduler` | Work queue: one worker per healthy server, retries, cancellation (section 5) |
| `media` | WAV → MP4/AAC via `ffmpeg`, reads and writes MP4 metadata, computes waveform peaks (section 6) |
| `library` | Folder layout, scanning, rename/tag/rate/move, export (section 7) |
| `playback` | Decodes MP4 with `symphonia` into a sample source with seek. The GUI owns the output device |
| `service` | `CoreHandle`: async command handler and event broadcaster that ties it all together |

### 2.3 Core ↔ GUI boundary

```rust
pub enum Command {
    LaunchServer(ServerId), StopServer(ServerId), RefreshServers,
    Transcribe { audio: PathBuf },
    StartRun(RunSpec), EditRun(RunId, RunEdit),   // RunEdit: params / count / next_seed
    PauseRun(RunId), ResumeRun(RunId), StopRun(RunId), CancelJob(JobId),
    Library(LibraryCommand),          // rename, tag, rate, move, export, rescan
    Play(SongId), Seek(Duration), Pause, Stop,
}

pub enum Event {
    ServerStatus(ServerId, ServerState),
    TranscribeDone(Result<AbcScore>),
    JobUpdate(JobId, JobState),       // Queued / Running{server, started} / Encoding / Done(SongId) / Failed
    LibraryChanged(LibraryDelta),
    PlaybackPosition(Duration), PlaybackState(PlayState),
    Log(LogLine),
}
```

`gui-core` holds `AppState` and a pure `fn update(&mut AppState, Input) -> Vec<Command>`,
where `Input` is either `UiAction` or `Event`. Almost all GUI behavior can therefore be
unit tested without a window, and the egui layer stays a thin view.

Runtime: `core` runs on a tokio multi-thread runtime in a background thread. The GUI
drains `Event`s at the start of each frame and calls `ctx.request_repaint()` from an
event-forwarding task.

#### 2.3.1 The UI thread never blocks

No long-running operation may run on, or wait on, the UI thread. Concretely:

- The UI thread only sends `Command`s (non-blocking channel send) and drains `Event`s with
  `try_recv`. It never waits for a reply. Results arrive later as events, and the UI shows
  progress or a pending state in the meantime.
- All network calls, `ffmpeg`/`ffprobe` runs, hashing, decoding, library scans, moves,
  exports and project writes run in `core`: on the tokio runtime, or on its blocking pool
  (`spawn_blocking`) for synchronous I/O.
- `gui-core`'s `update()` is pure and fast: no file system or network access. Anything
  that needs I/O (for example, finding a project's reference for *Re-transcribe*) is a
  `Command`.
- Native file dialogs (`rfd`) and reading a picked `.abc` file run on a helper thread.
  The chosen path or contents come back to the UI as a `UiAction` through a channel.
- The core's own command loop also stays responsive: commands that wait on servers or
  disk (launch, recheck, load/save preset, load project, play, regenerate) run as their
  own tasks, so playback and queue commands are never stuck behind them.
- Long operations never hold a shared lock while they work. For example, an export
  copies the song list under the library lock and encodes after releasing it.
- The audio output callback only reads decoded samples from memory; decoding happens
  beforehand on the blocking pool.

---

## 3. Configuration (RON)

- Location: the platform config directory from `directories::ProjectDirs`:
  `$XDG_CONFIG_HOME/audiocpp-ui/config.ron` on Linux, `%APPDATA%\audiocpp-ui\config.ron`
  on Windows, and `~/Library/Application Support/audiocpp-ui/config.ron` on macOS.
  Override it with `--config <path>`.
- Parsing enables **every RON extension**:
  ```rust
  let opts = ron::Options::default().with_default_extension(ron::extensions::Extensions::all());
  let cfg: Config = opts.from_str(&text)?;
  ```
  This turns on `implicit_some`, `unwrap_newtypes`, `unwrap_variant_newtypes`, and
  `explicit_struct_names`. The last one means **struct names are required** in the file,
  so the example below uses them. Files can also add their own `#![enable(...)]` lines;
  these do no harm.
- Saving (for example, "save current params as preset") uses `ron::ser::to_string_pretty`
  with the same options, so the file round-trips.

### 3.1 Example

```ron
Config(
    server_binary: "/opt/audiocpp/audiocpp_server",
    working_dir: "/opt/audiocpp",
    terminal: Native,          // .desktop on Linux, .command on macOS, .cmd on Windows
                               // or: Command(["kitty", "--title", "{name}", "--", "{script}"]), Headless
    request_timeout_secs: 1800,

    servers: [
        Server(
            name: "gpu1",
            port: 9123,
            launch: Launch(                      // omit `launch` → attach-only
                backend: "vulkan",
                device: 1,
                extra_args: ["--ui", "--ui-management", "--log"],
                autostart: true,
            ),
        ),
        Server(
            name: "gpu2",
            port: 9124,
            launch: Launch(backend: "vulkan", device: 2,
                           extra_args: ["--ui", "--ui-management", "--log"], autostart: true),
        ),
    ],

    models: Models(
        yue2: ModelSpec(
            id: "yue2", family: "yue2", task: "gen", mode: "offline",
            path: "/mnt/storage/audiocpp/models/Yue2-3B-GGUF",
            session_options: {
                "yue2.ar_lora_scale": "1",
                "yue2.nar_lora_scale": "1",
                "yue2.model_gguf": "yue2-3b-bf16.gguf",
                "yue2.vae_gguf": "yue2-vae-f32.gguf",
            },
        ),
        sheetsage2: ModelSpec(
            id: "sheetsage2", family: "sheetsage2", task: "midi", mode: "offline",
            path: "/mnt/storage/audiocpp/models/SheetSage2-GGUF/sheetsage2-orig.gguf",
            session_options: {},
        ),
    ),

    library: Library(
        root: "~/Music/audiocpp",               // unreviewed/, reviewed/{good,neutral,bad}/
        encoder: Aac(bitrate_kbps: 256),        // or: Alac
        // file names are always "<run name>-<seed>" (section 5.1.1)
    ),

    defaults: "presets/default.ron",            // a GenerationParams preset
)
```

With 0 servers the app still works as a **library browser and player**. With 0 servers
that have `launch`, it only attaches to servers that are already running.

---

## 4. Launching servers in terminals

The command line for each server is built from config. For the two servers above:

```
./audiocpp_server --ui --ui-management --backend vulkan --device 1 --log --port 9123
./audiocpp_server --ui --ui-management --backend vulkan --device 2 --log --port 9124
```

### 4.1 Terminal strategy (`TerminalLauncher` trait)

Launching is **cross-platform**. Each OS uses its own "open this in the user's default
terminal" mechanism, so no terminal emulator has to be named in config.

| `terminal:` value | Behavior |
|---|---|
| `Native` **(default)** | Platform-native launcher (section 4.1.1) |
| `Command([...])` | Override on any OS: spawns a user-given terminal argv. `{script}` expands to the launcher script path and `{name}` to the server name. Example: `["kitty", "--title", "{name}", "--", "{script}"]` |
| `Headless` | Runs the process directly with no terminal and pipes stdout/stderr into the in-app log view. Used by tests and CI |

Every mode starts from the same generated **launcher script**. The script records its PID
and then `exec`s the server (section 4.2), so PID tracking works the same way everywhere.
Scripts and launchers are written to a per-user runtime directory (`directories` crate:
`$XDG_RUNTIME_DIR/audiocpp-ui/` on Linux, `%LOCALAPPDATA%\audiocpp-ui\run\` on Windows,
`~/Library/Caches/audiocpp-ui/run/` on macOS).

#### 4.1.1 Native launchers

| OS | Generated file | Opened with |
|---|---|---|
| **Linux** | `<name>.sh` plus `<name>.desktop`, which has `Terminal=true` | The first that works of `gio launch <file>` → `kioclient exec <file>` → `dex <file>` → `xdg-open <file>` |
| **macOS** | `<name>.command` (the script itself, `chmod +x`) | `open -a Terminal <file>`, or the app named in `macos_terminal_app` |
| **Windows** | `<name>.cmd` | `wt new-tab --title <name> <file>` if Windows Terminal is installed, else `cmd /c start "<name>" cmd /k <file>` |

The Linux `.desktop` file (`Exec` points to the script, which avoids desktop-entry quoting
rules):

```ini
[Desktop Entry]
Type=Application
Name=audiocpp gpu1 (:9123)
Exec=/run/user/1000/audiocpp-ui/gpu1.sh
Path=/opt/audiocpp
Terminal=true
NoDisplay=true
```

Why this order of openers on Linux:

- **`xdg-open` is the last resort, not the first.** On most desktops it hands a `.desktop`
  file to the handler for `application/x-desktop`, which is usually a **text editor**, not
  an executor. It works on some setups but cannot be relied on.
- `gio launch` (GLib ≥ 2.60) actually runs the entry and follows `Terminal=true`. GLib
  picks the terminal: `xdg-terminal-exec` if it is present (newer GLib), otherwise a
  built-in list (`gnome-terminal`, `konsole`, `x-terminal-emulator`, …).
- `kioclient exec` is the equivalent on KDE, and `dex` covers minimal WMs.
- The app detects at startup which opener works, logs it, and shows it in the Servers
  panel. If none is found, it tells you to set `terminal: Command([...])`.

Trade-off: the window title comes from the terminal, not from `Name=`. The script sets it
with an OSC escape (`printf '\033]0;%s\007' "audiocpp gpu1"`), which most terminals
support.

### 4.2 Process lifetime

The terminal owns the process, not the app. The launcher script therefore records the PID
and `exec`s the server, so the PID in the file *is* the server:

```sh
#!/bin/sh
# generated by audiocpp-ui — gpu1
printf '\033]0;%s\007' "audiocpp gpu1 :9123"
echo $$ > "/run/user/1000/audiocpp-ui/gpu1.pid"
cd "/opt/audiocpp"
exec ./audiocpp_server --ui --ui-management --backend vulkan --device 1 --log --port 9123
```

On Windows there is no `exec`. The `.cmd` instead runs the server through
`powershell -NoProfile -Command "$p = Start-Process -NoNewWindow -PassThru …; $p.Id | Out-File …; $p.WaitForExit()"`
so that the recorded PID belongs to the server itself.

- **Start:** If `GET /health` already answers on the port, **attach** and do not launch.
  Otherwise launch, then poll `/health` every 500 ms until it returns ok or 60 s pass.
- **Stop:**
  - Unix: SIGTERM to the PID from the pidfile, then SIGKILL after 10 s.
  - Windows: `taskkill /PID <pid> /T`, then `/F` after 10 s.
  - The process may not exit until its current GPU job finishes, which can take minutes
    even after SIGKILL (section 1.4). The server therefore goes to **`Stopping`** and stays
    there until the PID is gone **and** the port stops accepting connections. There is no
    timeout that assumes it is dead. The Servers panel shows "Stopping — waiting for the
    GPU job to finish", with the elapsed time.
  - If the server is idle, Stop is effectively immediate. If it is busy, the UI warns
    first: "The running job can't be interrupted. The server will exit when it finishes
    (about N min)." N is estimated from the job's elapsed time and the RTF seen so far.
  - Launch stays disabled while the server is `Stopping`, so a new server never races the
    old one for the port or VRAM.
  - The terminal closes when its child exits, or stays open depending on the terminal's
    settings.
- **Crash detection:** The health poll fails, or the PID disappears, while the server is
  *not* `Stopping`. The server's state becomes `Down`, and any job running on it is
  requeued (section 5).
- On app exit, ask whether to stop the servers the app launched. Default: leave them
  running.

`ServerState = Stopped | Starting | Ready { backend, loaded_models } | Busy(JobId) | Stopping { since } | Down(reason)`

---

## 5. Batch generation and scheduling

### 5.1 Parameters

```rust
pub struct GenerationParams {          // serde, RON presets, embedded in MP4
    pub lyrics: String,
    pub style: String,
    pub abc: Option<String>,           // None → YuE2 generates its own ABC (supported, see 5.2)
    pub cot: String,                   // "full"
    pub guidance_scale: f32,
    pub num_inference_steps: u32,
    pub abc_sampling: Sampling,        // temperature, top_p, top_k, rep_penalty, window, min/max tokens
    pub semantic_sampling: Sampling,
    pub extra_options: BTreeMap<String, serde_json::Value>, // forward-compat passthrough
}

pub struct RunSpec {                   // a "run" = one batch
    pub name: RunName,                 // required, validated (5.1.1); fixed for the run's lifetime
    pub params: GenerationParams,      // editable while the run progresses (5.1.2)
    pub start_seed: u32,               // required, entered by the user
    pub count: Option<u32>,            // None → keep going until stopped
    pub model: ModelSpec,              // snapshot at submit time
    pub abc_source: AbcSource,         // where params.abc came from
}

pub enum AbcSource {
    File { file_name: String, sha256: String },      // loaded .abc file (copied into the project)
    Transcribed(ReferenceAudio),                     // any audio → WAV → SheetSage2; files live in the project
    Manual,                                          // typed or pasted into the editor
    None,                                            // params.abc == None; YuE2 generates its own
}

pub struct RunState {                  // owned by the scheduler
    pub spec: RunSpec,
    pub revision: u32,                 // bumped on every params edit
    pub next_seed: u32,                // seed cursor; the next job takes this, then +1
    pub issued: u32,                   // jobs handed out so far
    pub status: RunStatus,             // Active / Paused / Stopping / Done
}
```

#### 5.1.1 Run name (required) and output file names

- **Every run must be given a name.** *Start run* stays disabled until the name field is
  valid, and the reason is shown next to the field.
- The name field is **cleared after each run starts**. Presets do not store a name. Every
  new run therefore needs a name typed on purpose, even if everything else is reused.
- Each song is a **folder named `<name>-<seed>`** that holds `<name>-<seed>.mp4` and its
  lossless master `<name>-<seed>.wav`. For example, `sunny-hook-1233/sunny-hook-1233.mp4`.
  The folder, both files, and the MP4 title (`©nam`) all share this one stem (section 7.1).
- `RunName` validation, the same on every OS so files stay portable:
  - Trimmed and not empty. At most 100 characters.
  - None of `/ \ : * ? " < > |` or control characters. Must not end in `.` or a space.
  - Not a Windows reserved name (`CON`, `NUL`, `COM1`, …).
  - Spaces are kept.
- **Collision check** before starting: if any `<name>-<seed>` from the planned seed range
  already exists anywhere in the library (unreviewed or reviewed), the form points out the
  conflict and offers to **continue** instead: `start_seed = max existing seed + 1`. The
  same check runs when `next_seed` is edited during a run.
- As a final safeguard, if a song folder still collides at write time, the new folder
  (and its files) gets `-2`, `-3`, … added. Existing songs are never overwritten.
- **Continue run** (library context menu on any song): fills in the form with that song's
  recipe, name, and the next unused seed for that name.

#### 5.1.2 Seeds and editing a running job

- The user enters the **starting seed**, and the scheduler hands out `start_seed`,
  `start_seed + 1`, … in order. A 🎲 button next to the field fills in a random seed for
  convenience, but the value is always visible and editable before the run starts. Seeds
  are `u32` (0 … 4 294 967 295). Incrementing past the maximum stops the run. The server
  accepts more than this: the check in its binary is `Yue2 seed must be in [0, 2^63)`, and
  the spec says `int`, `min: 0`, `default: 1234`. So every `u32` is valid, and the UI never
  sends negative or "random" (`-1`) seeds.
- Across several servers, seeds are handed out **in the order workers ask for them**, so
  they are unique and have no gaps. A job that fails is retried **with the same seed**.
  If it fails for good, the seed is recorded as failed in the queue view, and the run does
  not skip ahead silently.
- **Changes while running:** the Queue panel's run editor stays active while the run is in
  progress. It can change:
  - `params`: lyrics, style, ABC, sampling. Each change bumps `revision`.
  - `count`: raise it, lower it, or set it to "until stopped".
  - `next_seed`: jump ahead or back, subject to the collision check.
  - Pause / resume / stop.
- A change affects **jobs that have not started yet**. Jobs already running finish with
  the parameters they started with. Each song's recipe records the exact request and the
  run `revision` it used. When several revisions share one name, you can still tell which
  settings produced which seed.
- The run name cannot be changed while the run is going, because it determines the file
  names. Stop the run and start a new one under a different name instead.

### 5.2 ABC source (encouraged, not required)

YuE2 **can run without `abc`** and will write its own melody. The default workflow still
**encourages supplying one**, because it gives much more control over the melody and
structure. The Generate panel has an **ABC source** selector:

| Option | Behavior |
|---|---|
| **Load .abc file** | File picker for `*.abc`, or drop the file onto the panel. The contents go into the ABC editor |
| **Transcribe from audio** | Any audio file → WAV → SheetSage2 (below). The result goes into the ABC editor |
| **Paste / edit** | Type directly into the ABC editor (`AbcSource::Manual`) |
| **None: let YuE2 compose** | Sends no `abc` field |

How the UI encourages an ABC:

- A new, empty form opens with the **ABC source** section expanded and focused. Its first
  two options (load or transcribe) are shown as the main buttons.
- If a batch is started with no ABC, a confirmation appears: *"No ABC melody: YuE2 will
  compose its own. Continue?"* It has a *"Don't ask again for this preset"* checkbox, which
  is stored on the preset as `allow_no_abc: true`. Choosing **None** on purpose in the
  selector skips the dialog.
- A preset can hold an ABC (inline, or as a path to a `.abc` file). A batch started from
  such a preset already has one.
- The ABC editor shows a short summary: meter, key, tempo, voices, bar count, and section
  markers (`% chorus`, …). It also validates the header before submitting. It warns when
  `X:`/`K:` are missing, but the batch can still be submitted.
- Transcriptions and the ABC actually used are always saved in the project folder
  (section 5.2.1), so they can be reused without re-running SheetSage2. The editor can
  also **Save as .abc** to any other location.

Songs made without an ABC are labeled in the library ("no ABC"), so they can be filtered
and compared against songs that had one.

#### Transcription

`Reference audio → WAV → SheetSage2 → ABC` runs once for each reference file.

1. The user picks or drops any audio file. The type is detected by probing it with
   `ffprobe`, not from the extension. Files ffmpeg can't read are rejected before any
   copying.
2. The original is **copied unchanged** into the project as `reference.<ext>`.
3. **Conversion to WAV**, since the server only accepts WAV (section 1.3):
   - An existing PCM s16 WAV is used as is. No second copy is made.
   - Anything else (MP3, FLAC, float or 24-bit WAV, …) is converted with
     `ffmpeg -i reference.<ext> -vn -map_metadata -1 -c:a pcm_s16le reference.upload.wav`.
     The original sample rate and channel count are kept, and SheetSage2 does its own
     resampling.
   - Conversion runs on the blocking pool and shows progress in the ABC section.
4. The WAV is uploaded and transcribed (section 1.3). The raw result is saved as
   `transcription.abc`, and the `events` artifact as `transcription.events.json`.
5. The ABC goes into the editor, so it can be changed before the run starts.

**Reuse:** the library index maps the SHA-256 of each original reference to its
`transcription.abc`, across all projects. When the same audio is loaded again, into this
project or another, the existing transcription is copied and no server call is made. A
**Re-transcribe** button forces a new run. The project folders are the transcription
cache, so there is no separate one.

The reference's hashes and file names go into each song's recipe. The audio itself does
not, because it stays in the project.

Transcription runs on whichever server is idle, using `ensure_loaded(sheetsage2)`. After
it finishes, the server **always** unloads SheetSage2 with
`POST /v1/models/unload {"id":"sheetsage2"}` before it goes back to YuE2 jobs. The unload
also runs if the transcription fails. A song is usually transcribed once at the start of a
run, so keeping SheetSage2 loaded would gain little and could crowd out YuE2 in VRAM.
There is no setting for this.

#### 5.2.1 Projects (`inputs/<name>/`)

A **project** is the folder of inputs for a run name. Its name is the **run name**, so
the project `sunny-hook` produces the songs `sunny-hook-1233/`, `sunny-hook-1234/`, …:

```
<library_root>/inputs/
└── sunny-hook/
    ├── project.ron                  # provenance (below)
    ├── reference.mp3                # the file the user supplied, copied unchanged
    ├── reference.upload.wav         # converted WAV that was uploaded (absent if the original was PCM s16 WAV)
    ├── transcription.abc            # raw SheetSage2 output, never edited
    ├── transcription.events.json    # SheetSage2 `events` artifact
    ├── sunny-hook.abc               # the ABC the run actually used (after edits)
    ├── lyrics.txt                   # lyrics the run used
    └── style.txt                    # style the run used
```

- **Name first.** The **Name** field moves to the top of the Generate panel. Load .abc and
  Transcribe stay disabled until it holds a valid `RunName`, because inputs need a project
  to be saved into. The tooltip says: *"Enter a name first. Inputs are saved to
  inputs/<name>/."* The name is still required for every run and still cleared after each
  run starts (section 5.1.1).
- **Existing projects:** typing a name that matches an existing project offers **"Load
  project inputs"**. This fills the ABC, lyrics, and style editors from the project. The
  seed collision check (section 5.1.1) then offers to continue from the next free seed.
  **Continue run** from a song opens that song's project the same way.
- **When files are written:**
  - The reference, its conversion, and the transcription are written as soon as they are
    produced.
  - `<name>.abc`, `lyrics.txt`, and `style.txt` are written when the run starts, and
    rewritten on every params edit (section 5.1.2). They always show the latest
    revision.
  - The exact inputs of each song are in that song's recipe (section 6.2), which is still
    the source of truth. The project files are the convenient, human-readable copy.
  - Every write goes through a temp file and a rename.
- **A new reference** in a project that already has one asks for confirmation. The old
  `reference.*` and `transcription.*` files go to the system trash. Songs made from them
  still record their hashes in their recipes.
- **ABC from a file or the editor:** a loaded `.abc` file is copied to `<name>.abc` (its
  original name is kept in `project.ron`). ABC that was typed or pasted is just saved to
  `<name>.abc`. With **None**, no ABC file is written.
- **Renaming a song doesn't rename its project.** A song finds its project through
  `recipe.run.name`.

`project.ron`:

```ron
Project(
    name: "sunny-hook",
    created_at: "2026-09-23T21:40:02Z",
    reference: Some(Reference(
        original_name: "My Demo (final).mp3",   // what the user picked
        file: "reference.mp3",
        sha256: "…",
        upload_file: Some("reference.upload.wav"),   // None → the original was uploaded
        upload_sha256: "…",
        conversion: Some("ffmpeg 7.1 -c:a pcm_s16le"),
    )),
    transcription: Some(Transcription(
        file: "transcription.abc", model: "sheetsage2", created_at: "…", wall_ms: 10513,
    )),
    abc: Some(AbcFile(file: "sunny-hook.abc", source: Transcribed)), // File(original_name) / Manual
    runs: ["01J8…", "01J9…"],               // run ids that used this project
)
```

### 5.3 Scheduler

- Jobs are **created one at a time**, not all at submit time. When a worker is free, it
  asks the scheduler for the next job. The scheduler then takes the active run's
  *current* `params` and `revision`, takes `next_seed`, and advances the cursor, all under
  one lock. This is what lets edits (5.1.2) apply to every job that hasn't started.
- **First come, first served.** Work is never assigned to a particular server ahead of
  time. Whichever server frees up first gets the next seed, so every GPU is busy whenever
  there is work, and seeds come out as fast as the hardware allows. A faster GPU simply
  takes more seeds. There is no setting for this.
- Several runs can be queued. The next job always comes from the **oldest active run that
  still has work**, and within that run retries come before new seeds. So run A's seeds
  are spread over every server. Once A has handed out its last seed, the next server to
  free up starts run B right away, even while A's final jobs are still running on the
  other servers. No GPU sits idle between runs. Paused runs are skipped. A run with
  `count: None` ("until stopped") keeps later runs waiting until it is stopped. The Queue
  panel lets the user drag runs to reorder them.
- **One worker per `Ready` server.** A worker loops:
  1. Get the next job from the scheduler.
  2. `ensure_loaded(job.model)` (a no-op if the same id is loaded with the same
     `session_options`).
  3. `POST /v1/tasks/run`.
  4. Stream-decode the audio into a temp WAV.
  5. Hand it to the encoder.
  6. Ask for the next job.
- Encoding (ffmpeg) runs on a separate blocking pool, so the GPU never waits for it.
- **Failure handling:** A transport error or server crash puts the job back at the front
  of its run's queue with `attempts += 1`, keeping the same seed and revision. After 2 failed attempts the job is `Failed(reason)`.
  An HTTP 4xx is `Failed` right away, because it is a parameter error.
- **Cancel:** A started job always runs to the end (section 1.4), so cancelling it is
  really *discarding* its result:
  - **Queued job:** dropped right away.
  - **Running job:** marked `Cancelling`. The worker **keeps the HTTP request open**
    instead of closing it. Closing it would free nothing on the GPU, and would also hide
    the moment the server becomes free again. When the response arrives, it is thrown
    away without being decoded or encoded, and the worker takes the next job. The Queue
    row reads "Cancelling — finishes in ~N min".
  - **Restarting the server is not offered as a faster cancel.** The process doesn't
    exit until the GPU job is done anyway (section 4.2).
  - **Stop run** stops handing out new seeds, and drops the run's queued retries. Jobs
    already running finish and are **kept**, because their GPU time has already been
    spent. To throw them away too, the user cancels them individually.
- **Timeouts:** When `request_timeout_secs` runs out, the client closes the request, but
  the server probably keeps working (section 1.4). The job is requeued with the same seed,
  so another server can pick it up. The server that timed out goes to
  `Down("request timed out; may still be busy")`, and gets no new work until the user
  restarts it or clicks **Recheck**. Recheck returns it to `Ready` once `/health` answers.
- Servers can join or leave while the app runs, and workers start and stop to match.
- Adding servers gives close to linear speedup, because each job keeps one GPU busy for
  about 3–4 minutes.

---

## 6. Output files: MP4 + reproducibility metadata

### 6.1 Encoding

- The server's WAV (48 kHz s16 stereo) is decoded straight into the song's staging folder
  and **kept as the lossless master**, `<name>-<seed>.wav`, byte for byte as the server
  sent it. At about 11 MB per minute, a 4–6 min song takes about 45–65 MB.
- The MP4 is encoded from the master:
  `ffmpeg -i <stem>.wav -c:a aac -b:a 256k -movflags +faststart <stem>.mp4`. The `Alac`
  setting encodes losslessly instead. The MP4 is the file for listening and sharing, and
  it carries all the metadata.
- `ffmpeg` is an external dependency. It is found on `PATH` or through config, and its
  presence is checked at startup.
- The whole song is staged in `unreviewed/<stem>.part/`. The WAV, the MP4, and its
  metadata are all written there, and then the **folder is atomically renamed** to
  `unreviewed/<stem>/`. A half-finished song never shows up in the library. Leftover
  `*.part` folders are removed at startup.
- The extension is `.mp4`, as specified. `.m4a` is a config option for players that
  prefer it.

### 6.2 Metadata

Written with the `mp4ameta` crate, so tags can be edited later in place without
re-encoding.

| Atom | Content |
|---|---|
| `©nam` (title) | Defaults to `"<run name>-<seed>"`, which is the same as the file name. Can be edited in review |
| `©cmt` (comment) | User notes |
| `©too` (encoder) | `audiocpp-ui {version}` |
| `----:org.audiocpp-ui:recipe` | **Reproducibility record** as JSON (below) |
| `----:org.audiocpp-ui:tags` | JSON array of user tags. Also mirrored to `keyw` or `©gen` for other players |
| `----:org.audiocpp-ui:rating` | `"good"`, `"neutral"`, or `"bad"` (also implied by folder; the atom wins if the file is moved by hand) |

**Recipe record** (the JSON embedded in the MP4):

```jsonc
{
  "schema": 1,
  "app_version": "0.1.0",
  "song_id": "01J8…",                 // ULID
  "run": { "id": "01J8…", "name": "sunny-hook", "seed": 1233, "start_seed": 1230,
           "index": 3, "revision": 2 },      // revision = params edit count at job start
  "created_at": "2026-09-23T22:05:11Z",
  "server": { "name": "gpu2", "port": 9124, "backend": "vulkan", "device": 2 },
  "model": { "id": "yue2", "family": "yue2", "task": "gen", "mode": "offline",
             "path": "…/Yue2-3B-GGUF", "load_options": {}, "session_options": { … },
             "file_hashes": { "yue2-3b-bf16.gguf": "sha256:…" } },   // optional, cached
  "request": { /* the exact /v1/tasks/run body sent, including seed */ },
  "project": "sunny-hook",            // inputs/<project>/ (section 5.2.1)
  "abc_source": { "Transcribed": { "file_name": "My Demo (final).mp3", "format": "mp3",
                                   "sha256": "…", "upload_sha256": "…",
                                   "abc_model": "sheetsage2" } },
                // or {"File":{…}}, "Manual", "None"; the ABC text itself is in `request`
  "timing": { "wall_ms": 155962, "audio_duration_ms": 278439, "rtf": 0.56013 }, // verbatim from server
  "output": { "sample_rate": 48000, "channels": 2,
              "wav_sha256": "…", "encoder": "aac 256k" }
}
```

The **Regenerate** action reads `request` and `model` back into the editor. It can submit
exactly the same request or open it as the base for a new batch. Output is only *nearly*
deterministic: the same recipe on the same model files and backend gives a result that
sounds the same, but it is not guaranteed to be bit-identical. GPU floating-point
reduction order and quantization differ between backends and devices. So the UI promises
"same recipe", never "identical file". The recipe records `backend` and `device`, and the
regenerated song is saved as a new take in its own folder, `<name>-<seed>-r2/`, next to
the original.

---

## 7. Library and review workflow

### 7.1 Layout

```
<library_root>/
├── unreviewed/
│   ├── sunny-hook-1233/
│   │   ├── sunny-hook-1233.mp4    # AAC + all metadata (title, tags, rating, recipe)
│   │   └── sunny-hook-1233.wav    # lossless master, exactly as the server returned it
│   └── sunny-hook-1234/
│       ├── sunny-hook-1234.mp4
│       └── sunny-hook-1234.wav
├── reviewed/
│   ├── good/           # song folders, same shape as above
│   ├── neutral/
│   └── bad/
├── inputs/             # one project folder per run name (section 5.2.1)
│   └── sunny-hook/     # reference audio, WAV conversion, transcription, used ABC/lyrics/style
├── exports/            # default export target
└── .cache/             # waveform peaks, library index (incl. reference-hash → transcription map)
```

- **The song folder is the unit.** Every move, rename, rating, and delete acts on the whole
  folder, so the MP4 and its master can't drift apart.
- The **MP4 is the source of truth** for metadata. The WAV holds only audio, and it is
  checked against `output.wav_sha256` in the recipe. `.cache/index.ron` only makes startup
  faster. It is rebuilt from a scan whenever an entry's mtime/size differs or the file is
  missing.
- **Scanning:** a folder that holds an `.mp4` is a song. The WAV is optional: if it is
  missing (deleted by hand, for example), the song is still listed, marked "no master",
  and WAV export falls back to decoding the MP4. A loose `.mp4` dropped straight into a
  rating folder is also listed. The next rename or move wraps it in a folder.
- A file watcher (`notify`) picks up songs that are moved or deleted outside the app.

### 7.2 Actions

| Action | Effect |
|---|---|
| Play / pause / stop / seek | `playback` module. Waveform strip with a click-to-seek bar and a position readout |
| Name | Writes `©nam`. Can also rename the song: the folder and both files become `<new name>-<seed>`. The `-<seed>` suffix is always kept, and the same `RunName` rules apply. Name collisions get `-2`, `-3`, … The files inside are renamed first, then the folder, and undo reverses the steps |
| Tag | Adds or removes free-form tags with autocomplete from tags already in the library |
| Notes | Writes `©cmt` |
| Rate good / neutral / bad | Writes the rating atom and **moves** the song folder to `reviewed/<rating>/`. Re-rating moves it between rating folders. "Unreview" moves it back |
| Export | Copies to a chosen folder as MP4, WAV, or MP3. **WAV is copied from the master**, so it is lossless. MP3 (and FLAC) are encoded **from the master**, never from the AAC. Metadata can optionally be stripped. Many songs can be exported at once |
| Regenerate | Section 6.2 |
| Delete | Moves the song folder to the system trash (`trash` crate), never a hard delete |

All moves happen in `core::library` and are atomic (a directory `rename` within one
filesystem).
Operations are recorded in an in-memory undo stack: **Ctrl+Z** undoes the last
rate/move/rename.

### 7.3 Review mode

A focused view that walks through `unreviewed/` in creation order and plays each song
automatically:

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `←` / `→` | Seek ±5 s (`Shift`: ±30 s) |
| `1` / `2` / `3` | Rate good / neutral / bad, then go to the next song |
| `N` / `P` | Next / previous song without rating |
| `T` | Focus the tag field |
| `R` | Focus the rename field |

---

## 8. GUI

Panels (eframe, native):

1. **Servers** (top bar or side panel): the state of each server, a Launch/Stop button,
   the loaded model, the current job and its elapsed time.
2. **Generate:** at the top, the **name** field (required, empty for each new run; it
   names the project and the songs, and offers "Load project inputs" when the project
   already exists; section 5.2.1). Then the preset picker, the **ABC source** section
   (load .abc, transcribe any audio file, paste, or none; section 5.2), lyrics/style/ABC
   editors, and sampling parameters (collapsible "advanced"),
   **starting seed** (required, 🎲 fills in a random one), count (or "until stopped"),
   **Start run**.
3. **Queue:** runs and jobs with their state, seed, revision, server, elapsed time and ETA.
   Each active run has an **inline editor** for params, count, and next seed, plus
   pause/resume/stop (section 5.1.2). A job is cancelled with its own button.
4. **Library:** a filterable table (folder/rating, tag, text search, run name, seed range, revision). Selecting a
   song shows the player, the metadata editor, and a read-only recipe view.
5. **Log:** core `tracing` output plus server stdout for `Headless` launches.

Every interactive widget gets a stable, **unique accessible label**, or an explicit
`id_salt` plus an AccessKit label. The headless tests (section 9.3) depend on these
labels.

---

## 9. Testing strategy

### 9.1 Unit tests (per crate, `#[cfg(test)]`)

- `config`: parsing RON with `Extensions::all()`, validation errors, round-trip
  serialization.
- `run`:
  - `RunName` validation: every rejected character, reserved Windows names, trimming,
    length.
  - The seed cursor hands out unique seeds with no gaps when several workers ask at once.
  - A retry keeps its seed and revision.
  - Stops at `u32::MAX`.
  - Collision detection and the "continue" suggestion (max existing seed + 1).
  - A params edit affects only jobs not yet started.
- `api`: request bodies match the HAR-derived fixtures **byte for byte in JSON
  semantics** (serde_json `Value` equality). The YuE2 response parses correctly,
  including `timing`, and a mismatch between the WAV header and `sample_rate`/`channels`
  is detected.
- `media`: writing and reading back the recipe, tag, and rating atoms on a tiny fixture
  MP4. Base64 stream decoding.
- `library`: rate → move, re-rate, unreview, name collisions, undo, and rebuilding the
  index after external changes (with `tempfile`). Every operation moves or renames the
  **whole song folder**, and the folder and both files keep the same stem. Also tested:
  songs whose WAV is missing, loose MP4s, leftover `*.part` folders being cleaned up, and
  a WAV whose hash doesn't match `wav_sha256` being flagged.
- `gui-core`: the `update()` reducer: the right `Command`s for each `UiAction`, review
  navigation, and form validation.

### 9.2 Integration tests (`crates/audiocpp-core/tests/`)

- A **mock audio.cpp server** (`axum` on an ephemeral port) that serves the fixture
  responses. It can inject delay, errors, and a mid-job crash. Tests cover:
  - A 2-server run spreads jobs across both, and the output is a
    `unreviewed/<name>-<seed>/` folder with `<name>-<seed>.mp4` and `<name>-<seed>.wav`
    for every seed from `start_seed` to `start_seed + count - 1`. Each has the correct
    recipe, and each WAV is byte-identical to the server's audio.
  - Editing params partway through: songs that started before the edit record revision N,
    and songs that started after it record N+1 and the new request.
  - Killing one mock server requeues its job onto the other.
  - Transcribe → generate pipeline with **MP3, FLAC, float WAV, and PCM s16 WAV** input.
    The mock server **accepts only WAV**, like the real one: it checks the `RIFF…WAVE`
    magic, `Content-Type`, and `x-audiocpp-filename`. Also checked:
    - The project folder holds the original unchanged and a converted
      `reference.upload.wav`, which is absent for PCM s16 input.
    - `transcription.abc`, `<name>.abc`, `lyrics.txt`, `style.txt`, and `project.ron` are
      written.
    - A file ffmpeg can't read is rejected before anything is copied.
  - Loading the same reference into a second project reuses the transcription with **no**
    server call. Re-transcribe forces one.
  - A params edit mid-run rewrites the project's `<name>.abc` / `lyrics.txt` / `style.txt`.
  - `ensure_loaded` reloads when `session_options` change.
  - Every transcription, whether it succeeds or fails, is followed by an `unload` of
    `sheetsage2` before the server's next YuE2 job.
  - Cancelling a running job keeps its connection open, and the server gets no new job
    until the (slow) response arrives. The response is then discarded, and no file is
    written.
  - Stop run keeps jobs that are already running and drops queued retries.
  - First come, first served: with one fast and one slow mock server, the fast one
    completes proportionally more seeds, and seeds stay unique with no gaps. With two
    queued runs, run B's first job starts on the first server to free up after A has
    handed out its last seed, without waiting for A's last job to finish.
  - After a timeout, the job is requeued on the other server, and the timed-out server is
    `Down` until Recheck.
- **Launcher** tests with the `Headless` terminal and a stub server script. The stub
  **ignores SIGTERM and keeps its port open for a while**, to check that the server stays
  `Stopping` (not `Down`), and that Launch stays disabled until both the PID and the port
  are gone.
- **Launcher file generation** (unit): golden tests for the `.sh`, `.desktop`, `.command`,
  and `.cmd` output on every OS, since generating a file doesn't need that OS. Also tests
  that the opener-probe order is followed, with a fake `PATH`.
- **Native launch** (`#[ignore]`, needs a desktop session): opens a real terminal with a
  stub server and waits for the pidfile and `/health`.
- **Real-server smoke test**, `#[ignore]`, enabled by `AUDIOCPP_SERVER_URL`: loads the
  models and generates one short song with a low `semantic_max_tokens`.

**Fixtures:** small JSON fixtures are cut out of the HARs, with a ~1 s WAV in place of the
audio payload. The HAR files themselves are 0.3 MB and 555 MB and are **not committed**.
Add `*.har` to `.gitignore`.

### 9.3 Headless GUI tests (`crates/audiocpp-gui/tests/`)

These use **`egui_kittest`**, which drives the real egui app headlessly and finds widgets
through the **AccessKit** tree (`harness.get_by_label("Start run").click()`). The core is
replaced by a fake `CoreHandle` that records `Command`s and replays scripted `Event`s.
Scenarios:

- *Start run* stays disabled while the name is empty or invalid, or the seed is missing.
  Once both are filled in, pressing it sends `StartRun` with the expected `RunSpec`, and
  the name field is empty again afterwards.
- A name that collides shows the "continue from seed N" suggestion.
- Editing a running run's params in the Queue panel sends `EditRun`.
- A `JobUpdate` event updates the queue row, which the test reads back by its accessible
  label.
- Starting a batch with no ABC shows the confirmation dialog. Choosing **None** on
  purpose, or a preset with `allow_no_abc`, skips it. A request built without an ABC
  leaves out the `abc` field entirely.
- In review mode, pressing `1` sends a `Rate(Good)` + `Next` pair of commands.
- The UI stays responsive: with a fake `CoreHandle` that never answers, every panel still
  renders and accepts input, and no frame waits on the core (section 2.3.1).
- Optional image snapshots (`egui_kittest` `snapshot` feature + wgpu) for layout
  regressions. These are off by default in CI.

---

## 10. Key dependencies

| Area | Crate |
|---|---|
| GUI | `eframe`, `egui`, `egui_extras` (tables), `egui_kittest` (dev) |
| Async / HTTP | `tokio`, `reqwest` (`json`, `stream`) |
| Serialization | `serde`, `serde_json`, `ron` (≥ 0.9 for `explicit_struct_names`) |
| Media | `mp4ameta` (tags), `symphonia` (`aac`, `isomp4`, `wav`), `rodio` or `cpal` (output), external `ffmpeg` |
| Misc | `base64`, `sha2`, `ulid`, `rand`, `directories`, `notify`, `trash`, `thiserror`, `tracing`, `nix` (Unix signals, `cfg(unix)`), `which` (probing for the opener and terminal) |
| Test | `tempfile`, `axum` (mock server), `pretty_assertions`, `insta` (optional) |

---

## 11. Milestones

1. **M1 Core skeleton:** workspace, config, API client and mock server, one job end to end
   → WAV on disk.
2. **M2 Media and library:** MP4 encode, recipe metadata, folder layout, rate/move/tag
   (all tested without a GUI).
3. **M3 Scheduler:** multiple servers, retries, cancellation, launcher (Native
   `.desktop` on Linux, plus Command and Headless).
4. **M4 GUI:** servers, generate, queue, library panels, playback and seek. kittest suite.
5. **M5 Review mode and polish:** keyboard flow, undo, export, SheetSage2 transcription,
   presets, native launchers for macOS and Windows.

---

## 12. Open questions

| # | Question |
|---|---|
| ~~Q1~~ | **Resolved:** `audio`, `sample_rate`, `channels`, `timing{wall_ms, audio_duration_ms, rtf}` (section 1.4) |
| ~~Q2~~ | **Resolved:** `Native` launcher by default (`.desktop` + `Terminal=true` on Linux, opened with `gio launch`, falling back to `xdg-open`). `Command` override and `Headless` also exist (section 4.1) |
| ~~Q3~~ | **Resolved:** YuE2 runs without `abc`. The workflow still encourages loading or transcribing one (section 5.2) |
| ~~Q4a~~ | **Resolved:** the user enters a starting seed, and the app increments it for each job. Params can be edited as the run goes. Outputs are `<name>-<seed>/` song folders. A name is required for every run (sections 5.1.1–5.1.2) |
| ~~Q4b~~ | **Resolved:** output is close to deterministic, not bit-exact. "Regenerate" promises the same recipe and a near-identical result (section 6.2). The server validates `seed ∈ [0, 2^63)`, so the full `u32` range is safe (section 5.1.2) |
| ~~Q5~~ | **Resolved:** SheetSage2 is always unloaded after each transcription, because a song is usually transcribed only once, at the start of a run. There is no setting (section 5.2) |
| ~~Q6~~ | **Resolved:** a running task can't be cancelled. Closing the request doesn't stop the GPU work, and even `kill -9` only takes effect once the job finishes. Cancel therefore means "let it finish and discard the result", with the request kept open so the worker knows when the GPU is free. Stop puts the server in `Stopping` until the process has actually exited (sections 1.4, 4.2, 5.3) |
| ~~Q7~~ | **Resolved:** first come, first served. The next free server takes the next seed, from the oldest active run that still has work. No server is ever pinned to a run, and there is no interleave setting (section 5.3) |
| ~~Q8~~ | **Resolved:** keep the WAV master. Each song is a `<name>-<seed>/` folder that holds `<name>-<seed>.mp4` and `<name>-<seed>.wav`. The folder is what gets moved, renamed, and deleted. WAV export copies the master, and other formats are encoded from it (sections 6.1, 7.1, 7.2) |
| ~~Q9~~ | **Resolved:** the server accepts only WAV. Every reference is converted to WAV with ffmpeg (PCM s16 WAV passes through) and uploaded with the captured `audio/vnd.wave` + `upload.wav` headers. The original, the converted WAV, and the transcriptions are kept in a per-name **project** folder, `inputs/<name>/` (sections 1.3, 5.2, 5.2.1) |
