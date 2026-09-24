# audiocpp-ui

Desktop app (Rust, egui) for batch song generation with one or more
[audio.cpp](docs/DESIGN.md#1-background-the-audiocpp-server-api) servers: launch servers,
transcribe a reference to ABC with SheetSage2, generate batches with YuE2 across all GPUs,
and review/organize the results. See [docs/DESIGN.md](docs/DESIGN.md).

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
