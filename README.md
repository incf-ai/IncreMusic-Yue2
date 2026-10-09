Human generated note:

I made this program with the intent of speeding up the process of making, reviewing, keeping track of, etc. song covers with the yue2 music model and the sheetsage2 transcription model. It supports using multiple GPUs (by connecting to multiple local audio.cpp servers, installed separately) to generate several songs simultaneously. On my machine the generation rate sometimes exceeds one song per minute, which is helpful given the large role that RNG plays in AI music generation.

Machine generated below:

# IncreMusic-Yue2

Desktop app (Rust, egui) for batch song generation with one or more
[audio.cpp](https://github.com/0xShug0/audio.cpp) servers: launch servers,
transcribe a reference to ABC with SheetSage2, generate batches with YuE2 across all GPUs,
and review/organize the results. See [docs/DESIGN.md](docs/DESIGN.md).

**This app does no inference itself.** It needs audio.cpp's `audiocpp_server` and the YuE2
and SheetSage2 models; see [Setup](#setup).

![Generate tab: a new run's name, ABC melody, lyrics, style, starting seed and count](pub_docs/generate.png)

## Screenshots

The server bar at the top shows each server's state and the job it's running. Songs are
played from any tab, and the current one shows at the right of the tab bar. Tables sort
by clicking a column header; click it again to reverse.

### Generate

Set a name, then give YuE2 a melody: load an `.abc` file, transcribe one from any audio
file with SheetSage2, paste one in, or let YuE2 compose. Add lyrics (typed, or loaded from
and saved to a `.txt` file) and a style, pick a
starting seed and how many songs to make (or *until stopped*), and start the run. The
name is kept afterwards, and the seed moves past the run just started.

![Generate tab: ABC melody editor](pub_docs/generate-abc.png)
![Generate tab: lyrics, style, starting seed and count](pub_docs/generate-lyrics-style.png)

### Queue

Runs waiting or in progress, with each running seed and the server it's on. Runs can be
paused, stopped, edited, reordered, or loaded back into Generate.

![Queue tab](pub_docs/queue.png)

### History

Every run is recorded, newest first. Select one to load it, or one of its revisions, into Generate.

![History tab](pub_docs/history.png)

### Projects

One project per name, holding its reference audio, transcription, ABC and review
keypoints.

![Projects tab](pub_docs/projects.png)

### Inputs

Reference audio files: play them, use one in the current project, or transcribe it again.

![Inputs tab](pub_docs/inputs.png)
![Inputs tab playing a reference](pub_docs/inputs-playing.png)

### Audio Library

Every generated song, filtered by folder, text, tag, run name, seed range or revision.
Rate, tag, rename, continue a run, regenerate, or export as MP3, WAV or FLAC. Each song
keeps the recipe it was made from. MP3 exports also carry the project's keypoints as chapters.

![Audio Library tab: songs with ratings, and the selected song's waveform, rating and export controls](pub_docs/library.png)

### Review

Go through unreviewed songs one at a time with the keyboard. Rating a song moves its
folder to `reviewed/good/`, `neutral/` or `bad/`. The keypoints pane on the right keeps a
project's moments worth checking, such as the chorus or a tricky line: *Add at playhead*
adds one, and each has an optional name and pre-roll. *Review next keypoint* steps
through them in the order you set.

| Key | Action |
|---|---|
| Space | Play / pause |
| ← / → | Seek 5 s back / forward (Shift: 30 s) |
| 1 / 2 / 3 | Rate good / neutral / bad and move on |
| C | Rate bad, after asking if the song played for less than 30 seconds |
| X / Z | Next / previous keypoint |
| N / P | Next / previous song |
| T / R / L | Tag, rename, show lyrics |
| Ctrl+Z | Undo the last rating or rename (also in other tabs) |

![Review tab: a song's waveform with keypoint markers, and the project's keypoints pane](pub_docs/review.png)

### Log

The app's own messages and each server's output, merged by time.

![Log tab](pub_docs/log.png)

## Setup

Only Linux has been tested. Windows and macOS builds are untested. There are no prebuilt
binaries, so the app is built and run from a clone of this repository.

### 1. audio.cpp and the models

- Get `audiocpp_server` from [audio.cpp](https://github.com/0xShug0/audio.cpp), either a
  release build or built from source, with the backend for your GPU (`vulkan`, `cuda`,
  `hip`/`rocm`, `metal` or `cpu`). It was tested with v0.8.2 and Vulkan.
  `audiocpp_server --backend vulkan --list-devices` prints the device indexes the config
  needs.
- Download the **YuE2** model (a directory holding the model GGUF, the VAE GGUF and the
  `sidecars/` folder) and the **SheetSage2** GGUF. Both come from audio.cpp's model
  packages ([audio-cpp/audio.cpp-gguf](https://huggingface.co/audio-cpp/audio.cpp-gguf)).
  The example config uses the bf16 YuE2 model; if you download another quantization,
  change the GGUF file names in the config to match. SheetSage2 is only needed to
  transcribe reference audio.

### 2. Build tools and the source

- Rust 1.88+ (edition 2024)
- `ffmpeg` and `ffprobe` on `PATH` (or `ffmpeg:` in the config). On Debian/Ubuntu:
  `sudo apt install ffmpeg`
- Linux: ALSA development files (`libasound2-dev`) for audio output. Build without audio
  with `cargo build -p incremusic-gui --no-default-features`.

```sh
git clone https://github.com/incf-ai/IncreMusic-Yue2.git
cd IncreMusic-Yue2
```

### 3. Config

```sh
mkdir -p ~/.config/incremusic-yue2/presets
cp config.example.ron ~/.config/incremusic-yue2/config.ron
cp presets/default.ron ~/.config/incremusic-yue2/presets/   # optional starting values for Generate
```

The example config has the author's paths, so edit it before the first run:

| Field | What to set |
|---|---|
| `server_binary`, `working_dir` | Path to `audiocpp_server`, and the directory each launched server starts in. |
| `servers` | One `Server` per GPU, each on its own `port`. `launch` sets the `backend` and `device` index (from `--list-devices`), and `autostart: true` starts it with the app. Leave out `launch` to attach to a server you start yourself; start it with the flags in the next row. |
| `extra_args` | Keep `--ui-management`, which lets the app load models and check model paths, and `--busy-timeout-ms 0`, without which songs longer than 5 minutes fail with 503. |
| `request_timeout_secs` | How long one request may take (1800 in the example). Raise it if long songs time out. |
| `terminal` | `Native` opens each server in a terminal window, `Command([...])` uses a terminal of your choice, and `Headless` shows server output only in the Log tab. |
| `models.yue2.path`, `models.sheetsage2.path` | The YuE2 directory and the SheetSage2 GGUF. The GGUF file names in `session_options` are relative to the YuE2 directory. |
| `library.root` | Where all the app's data lives, so back this folder up: songs in `unreviewed/` and `reviewed/{good,neutral,bad}/`, projects and reference audio in `inputs/`, run history in `runs/`, and `exports/`. It is created if missing. |
| `library.encoder` | MP3 quality: `Vbr(quality: 0)` (LAME -V0) or `Cbr(bitrate_kbps: 320)`. |
| `defaults` | The preset that fills a new Generate form, relative to the config file. |

The full reference is [DESIGN.md §3](docs/DESIGN.md#3-configuration-ron). The app reads
`~/.config/incremusic-yue2/config.ron` on Linux unless `--config` gives another path. If
the file is missing or invalid, the app opens a page naming the file and the problem
instead.

### 4. Run

```sh
cargo run --release -p incremusic-gui -- [--config path/to/config.ron]
```

Servers with `autostart: true` start with the app; start the others from the server bar.
Setup worked if:

- the Log tab says `ffmpeg found`,
- each server in the server bar reaches **Ready** within a minute, and
- no model path warning appears in the server bar or the Log.

## Getting started

### Terms

- **Run:** one batch of songs, started from the Generate tab: a name, the inputs, a
  starting seed and a count. Settings can be edited while the run goes; each edit is a new
  **revision**, and songs not yet started use it.
- **Name:** every run has one, and it ties things together. The run's inputs are kept in
  the **project** of the same name (`inputs/<name>/`), and each song is named
  `<name>-<seed>`.
- **Seed:** each song in a run gets the next seed, so a name and seed identify a song.
  The same request and seed give a song that sounds the same, but not a bit-identical file.
- **Continue run** makes more songs: the same name and settings, from the next unused
  seed. **Regenerate** makes the same song again: the same request and seed, saved next to
  the original as `<name>-<seed>-r2`.

### Your first song

1. Start the servers and wait for **Ready** (see [Run](#4-run)).
2. In **Generate**, type a name. Then give a melody: *Transcribe audio…* from a song, or
   *Load .abc…*, or choose **None** to let YuE2 compose.
3. Add lyrics with section tags (`[Verse]`, `[Chorus]`) and a style, such as
   `English, indie pop, warm lead vocal`.
4. Set the count and start the run. **Queue** shows each seed and the server it's on. A
   song takes a few minutes per GPU.
5. Finished songs go to `unreviewed/`. Go through them in **Review**, and find them later
   in **Audio Library**.

## How it works

- **Servers.** Each server is its own `audiocpp_server` process on `127.0.0.1:<port>`,
  normally one per GPU. The app launches it through a small script, or attaches if
  something already answers `/health` on that port. Stopping a busy server waits for its
  current song to finish, since a GPU job can't be interrupted. Server states are
  Stopped, Starting, Ready, Busy, Stopping and Down.
- **Scheduling.** Each Ready server takes the next seed of the oldest active run as soon
  as it is free, so faster GPUs take more seeds. Edits to a run apply to songs not yet
  started. A failed song is retried once with the same seed.
- **Generating a song.** The app loads YuE2 on the server if needed, sends one HTTP
  request with the lyrics, style, ABC and seed, and receives a WAV. It keeps the WAV as
  the lossless master and encodes an MP3 from it with ffmpeg. The MP3's ID3 tags hold the
  title, rating, tags and the **recipe**: the exact request, model, server and revision.
  The song folder appears in `unreviewed/` only once complete.
- **Transcribing.** The reference is copied into the project, converted to WAV with
  ffmpeg, and sent to SheetSage2 on an idle server. The result is saved in the project,
  and SheetSage2 is unloaded afterwards to leave VRAM for YuE2. The same audio is never
  transcribed twice unless you ask.
- **Files.** Everything lives under `library.root`, and the MP3 tags are the source of
  truth: `.cache/` only speeds up startup and can be deleted.
- **Logs.** The Log tab shows the app's messages and each server's output. Launched
  servers also log to `$XDG_RUNTIME_DIR/incremusic-yue2/<name>.log`, next to their
  launch scripts.

The full design is in [docs/DESIGN.md](docs/DESIGN.md); known gaps and quirks are in
[docs/CURRENT_STATUS.md](docs/CURRENT_STATUS.md).

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| The app shows a config error page | Fix the file and problem it names; struct names like `Server(...)` are required. |
| Log: `ffmpeg missing` | Install ffmpeg, or set `ffmpeg:` in the config. Generation and transcription need it. |
| Log: `no terminal opener found`, or the server goes Down right after Start | The desktop can't open a terminal for the server. Set `terminal: Command([...])` with your terminal, or `Headless`. |
| Server goes Down: `no healthy /health within 60 s` | Check its terminal or log file: wrong `server_binary` or `working_dir`, an invalid `backend`/`device`, or the port is used by another program. |
| Model path warning in the server bar | A model `path` or GGUF file name in the config is wrong, or the server lacks `--ui-management`. |
| Songs fail with 503 after about 5 minutes | The server is missing `--busy-timeout-ms 0`. |
| Server goes Down: `request timed out; may still be busy` | The song took longer than `request_timeout_secs`. The seed is retried elsewhere; press **Recheck** on the server once it's idle. |
| A song fails at once with an HTTP 4xx error | The server rejected the request's parameters; the error is in the Log. |
| Out-of-memory errors when loading a model | The GPU lacks VRAM for that model; try a smaller quantization. |

## Development

| Crate | What |
|---|---|
| `crates/incremusic-core` | config, launcher, API client, scheduler, media, library, playback, `CoreHandle` — no GUI deps |
| `crates/incremusic-gui-core` | `AppState` + pure `update()` reducer — no egui |
| `crates/incremusic-gui` | egui/eframe view and the `incremusic-yue2` binary |

### Test

```sh
cargo test --workspace                                # unit, mock-server integration and headless GUI tests
INCREMUSIC_REQUIRE_FFMPEG=1 cargo test --workspace    # fail instead of skip when ffmpeg is missing
# against a real server (generates one short song):
AUDIOCPP_SERVER_URL=http://127.0.0.1:9123 cargo test -p incremusic-core --test real_server -- --ignored
```

The HAR captures used to derive `tests/fixtures/` are not committed (`*.har` is ignored).

## License

[MIT](LICENSE)
