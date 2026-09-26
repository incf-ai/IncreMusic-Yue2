# audiocpp music ui

Desktop app (Rust, egui) for batch song generation with one or more
[audio.cpp](docs/DESIGN.md#1-background-the-audiocpp-server-api) servers: launch servers,
transcribe a reference to ABC with SheetSage2, generate batches with YuE2 across all GPUs,
and review/organize the results. See [docs/DESIGN.md](docs/DESIGN.md).

![Generate tab: a new run's name, ABC melody, lyrics, style, starting seed and count](pub_docs/generate.png)

## Screenshots

The server bar at the top shows each server's state and the job it's running. Songs are
played from any tab, and the current one shows at the right of the tab bar.

### Generate

Set a name, then give YuE2 a melody: load an `.abc` file, transcribe one from any audio
file with SheetSage2, paste one in, or let YuE2 compose. Add lyrics and a style, pick a
starting seed and how many songs to make (or *until stopped*), and start the run. The
name is kept afterwards, and the seed moves past the run just started.

![Generate tab: ABC melody editor](pub_docs/generate-abc.png)
![Generate tab: lyrics, style, starting seed and count](pub_docs/generate-lyrics-style.png)

### Queue

Runs waiting or in progress, with each running seed and the server it's on. Runs can be
paused, stopped, edited, reordered, or loaded back into Generate.

![Queue tab](pub_docs/queue.png)

### History

Every run is recorded. Select one to load it, or one of its revisions, into Generate.

![History tab](pub_docs/history.png)

### Projects

One project per name, holding its reference audio, transcription and ABC.

![Projects tab](pub_docs/projects.png)

### Inputs

Reference audio files: play them, use one in the current project, or transcribe it again.

![Inputs tab](pub_docs/inputs.png)
![Inputs tab playing a reference](pub_docs/inputs-playing.png)

### Audio Library

Every generated song, filtered by folder, text, tag, run name, seed range or revision.
Rate, tag, rename, continue a run, regenerate, or export as MP3, WAV or FLAC. Each song
keeps the recipe it was made from.

![Audio Library tab: songs with ratings, and the selected song's waveform, rating and export controls](pub_docs/library.png)

### Review

Go through unreviewed songs one at a time with the keyboard: Space plays and pauses,
1/2/3 rates good/neutral/bad and moves on.

![Review tab](pub_docs/review.png)

### Log

The app's own messages and each server's output, merged by time.

![Log tab](pub_docs/log.png)

## Layout

| Crate | What |
|---|---|
| `crates/audiocpp-core` | config, launcher, API client, scheduler, media, library, playback, `CoreHandle` — no GUI deps |
| `crates/audiocpp-gui-core` | `AppState` + pure `update()` reducer — no egui |
| `crates/audiocpp-gui` | egui/eframe view and the `audiocpp-ui` binary |

## Requirements

- Rust 1.88+ (edition 2024)
- `ffmpeg` and `ffprobe` on `PATH` (or `ffmpeg:` in the config)
- Linux: ALSA development files (`libasound2-dev`) for audio output. Build without audio
  with `cargo build -p audiocpp-gui --no-default-features`.

## Run

```sh
cp config.example.ron ~/.config/audiocpp-ui/config.ron   # edit paths, ports, devices
cargo run --release -p audiocpp-gui -- [--config path/to/config.ron]
```

## Test

```sh
cargo test --workspace                 # unit, mock-server integration and headless GUI tests
AUDIOCPP_REQUIRE_FFMPEG=1 cargo test   # fail instead of skip when ffmpeg is missing
# against a real server (generates one short song):
AUDIOCPP_SERVER_URL=http://127.0.0.1:8080 cargo test -p audiocpp-core --test real_server -- --ignored
```

The HAR captures used to derive `tests/fixtures/` are not committed (`*.har` is ignored).
