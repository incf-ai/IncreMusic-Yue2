# IncreMusic-Yue2 — Current Status

Status as of 2026-09-24, measured against [DESIGN.md](DESIGN.md).

Most of the design is implemented. The test suite passes: 147 tests, 0 failures, and 4
tests that are ignored on purpose because they need a desktop session or a real server.
Every test the design lists in §9 exists, except the image snapshot tests it marks
optional. The gaps are listed below.

## Added after the design

| Item | Where | Notes |
|---|---|---|
| Run history | [JOB_HISTORY_PLAN.md](JOB_HISTORY_PLAN.md) | Every run is recorded in `runs/<run-id>.ron`, including revisions and each seed's outcome. The **History** tab lists records and loads one or a chosen revision into Generate. Runs that were still going when the app quit become *Interrupted* and can be resumed. |
| Load into Generate from the Queue | Queue tab | Fills Generate with a queued run's current settings. The seed continues after the run's last seed. |

## Not implemented

| Item | Design | Notes |
|---|---|---|
| Model file hashes in the recipe | §6.2 | `model.file_hashes` is always empty. The design marks it optional. |
| GUI image snapshot tests | §9.3 | Not added. The design marks them optional and off by default. |

## Partly implemented, or changed from the design

| Item | Design | What was built |
|---|---|---|
| Reordering queued runs | §5.3 | Move up/down buttons instead of drag and drop. |
| Log panel | §8 | Toggles at the top show or hide the app's own messages (*App*) and each server's lines; *All* turns every source back on. Shown sources are merged by time, and each server keeps its own buffer so a chatty source can't push out the others' lines. On Linux and macOS, the launcher script copies everything the server prints to `<run dir>/<name>.log` (through `tee`, so the terminal still shows it), and the core tails that file into the log in every terminal mode. If the terminal opener fails (e.g. `gio launch` finds no terminal emulator), the opener's output is logged and the server goes down right away. On Windows only headless output is captured. The full `tracing` output isn't shown. |
| Transcription progress | §5.2 | A spinner shows while the audio is converted, not a progress bar. |
| Regenerate as the base for a new batch | §6.2 | Done through *Continue run*, which starts from the next free seed rather than the song's own seed. |
| Focus on a new form | §5.2 | A new form focuses the **Name** field, because *Load .abc…* and *Transcribe* stay disabled until there is a name (§5.2.1). Pressing Enter on a valid name moves focus to *Load .abc…*, unless the form already has an ABC or **None** is chosen. |
| Checking model paths | §1.1 | Each time a server becomes ready, the app checks each model's `path`, and every relative `*.gguf` session option inside it, with `/v1/ui/path-status`. Problems show as a warning in the server bar and in the Log. It is only a warning, so jobs still run. A server without `--ui-management` is skipped, and the Log says so. |
| Quit dialog | §4.2 | Asks about any server that has a launch config and isn't stopped, including ones the app only attached to. |

## Implemented, but with no end-to-end test

- **File watcher:** rescanning after the index changes is tested. Noticing songs moved or
  deleted outside the app while it runs is not.
- **Playing through the core:** the `Play` command (decode, waveform peaks, playback
  events) isn't tested. Actual sound output can't be tested in the dev container, which
  has no audio device.
- **Other core commands:** Retranscribe, loading and saving presets, and quitting with
  "stop servers" have no test.
- **Launching:** autostart and the `terminal: Command([...])` mode are untested. Only
  attaching to a running server and headless launch are tested.

## Real-server tests

`crates/incremusic-core/tests/real_server.rs` passed on 2026-09-24 against audio.cpp v0.8.2
(Vulkan, AMD Radeon AI PRO R9700). `generates_one_short_song` covers the API client, and
`generates_one_song_through_the_core` covers the whole pipeline: attach, model path check,
scheduler, MP4 encoding, recipe and library. `transcribes_through_the_core` turned a
generated AAC song into ABC in 29 s. That covered WAV conversion, upload, SheetSage2, the
project files, and unloading SheetSage2 afterwards. That run predates the switch of the
library format from MP4/AAC to MP3/ID3 (2026-09-25) and has not been repeated since. To run them:
`AUDIOCPP_SERVER_URL=http://127.0.0.1:9123 cargo test -p incremusic-core --test real_server -- --ignored`

## Written, but never run

- **Native terminal launch test** (`native_launch_opens_a_terminal`): needs a desktop
  session.
- **Windows and macOS:** never built or tested. Only Linux has been tested, so some tests may need fixes on other platforms.

## Things to know

- **Server busy timeout:** `audiocpp_server` has `--busy-timeout-ms` (default 300000, i.e.
  5 minutes). Once a model has been busy that long, the server fails the request with 503,
  and songs of 5–6 minutes can exceed it. `config.example.ron` passes
  `--busy-timeout-ms 0`.
- **Float timings:** audio.cpp v0.8.2 sends `timing.wall_ms` with fractions
  (`69719.4`), while the HAR captures had integers. `Timing` accepts both and rounds to
  whole milliseconds, so a recipe's `timing` is no longer exactly what the server sent.
- **Truncated YuE2 capture:** `yue2.har` only recorded the first 768 KB of the generation
  response. The 1-second WAV fixture comes from that part, and the fixture's `timing`
  values come from DESIGN.md §1.4.
- **Dev container dependencies:** the build needs ALSA headers (`libasound2-dev`) and
  `ffmpeg`/`ffprobe`. Both are installed in `.devcontainer/Dockerfile`; rebuild the
  container to pick them up. Audio output is a default cargo feature; build without it
  with `--no-default-features`.

## Suggested next steps

1. Add tests for the untested paths above.
