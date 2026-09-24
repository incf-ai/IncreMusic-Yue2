# audiocpp-ui — Current Status

Status as of 2026-09-24, measured against [DESIGN.md](DESIGN.md).

Most of the design is implemented. The test suite passes: 134 tests, 0 failures, and 2
tests that are ignored on purpose because they need a desktop session or a real server.
Every test the design lists in §9 exists, except the image snapshot tests it marks
optional. The gaps are listed below.

## Not implemented

| Item | Design | Notes |
|---|---|---|
| Drag and drop | §5.2 | You can't drop an `.abc` or audio file onto the Generate panel. Only the file-picker buttons work. |
| Focus on a new form | §5.2 | The ABC section opens expanded, but keyboard focus doesn't move to it. |
| Checking model paths against the server | §1.1 | `/v1/ui/path-status` isn't used to check that the configured model paths exist. Only the real-server smoke test calls it. |
| Model file hashes in the recipe | §6.2 | `model.file_hashes` is always empty. The design marks it optional. |
| GUI image snapshot tests | §9.3 | Not added. The design marks them optional and off in CI. |

## Partly implemented, or changed from the design

| Item | Design | What was built |
|---|---|---|
| Reordering queued runs | §5.3 | Move up/down buttons instead of drag and drop. |
| Log panel | §8 | Shows the core's log messages and headless servers' output, but not the full `tracing` output. |
| Transcription progress | §5.2 | A spinner shows while the audio is converted, not a progress bar. |
| Regenerate as the base for a new batch | §6.2 | Done through *Continue run*, which starts from the next free seed rather than the song's own seed. |
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

## Written, but never run

- **Real-server smoke test** (`crates/audiocpp-core/tests/real_server.rs`): not run,
  because the only real server available was in use. To run it:
  `AUDIOCPP_SERVER_URL=http://127.0.0.1:8080 cargo test -p audiocpp-core --test real_server -- --ignored`
- **Native terminal launch test** (`native_launch_opens_a_terminal`): needs a desktop
  session.
- **Windows and macOS:** the CI workflow (`.github/workflows/ci.yml`) has never run. Only
  Linux has been tested, so some tests may need fixes on other platforms.

## Things to know

- **Server busy timeout:** `audiocpp_server` has `--busy-timeout-ms` (default 300000, i.e.
  5 minutes). Once a model has been busy that long, the server fails the request with 503,
  and songs of 5–6 minutes can exceed it. `config.example.ron` passes
  `--busy-timeout-ms 0`.
- **Truncated YuE2 capture:** `yue2.har` only recorded the first 768 KB of the generation
  response. The 1-second WAV fixture comes from that part, and the fixture's `timing`
  values come from DESIGN.md §1.4.
- **Dev container dependencies:** the build needs ALSA headers (`libasound2-dev`) and
  `ffmpeg`/`ffprobe`. Neither is in the Dockerfile yet. Audio output is a default cargo
  feature; build without it with `--no-default-features`.

## Suggested next steps

1. Add drag and drop, the startup check of model paths, and focus on a new form.
2. Add tests for the untested paths above.
3. Push to run CI on Windows and macOS. Run the real-server smoke test when a server is
   free.
4. Add `ffmpeg` and `libasound2-dev` to the dev container's Dockerfile.
