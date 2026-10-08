# Plan: Run history (saving past jobs so they can be reloaded)

Status: Phases 1 and 2 implemented 2026-09-26. See **As built** at the end for where
the code differs from this plan. The optional follow-ups (§5) are not built.

Two requests drive this plan:

- **Keep every queued run for the record**, including runs that never produced a song.
- **A button that loads a queued run into Generate.**

They ship in two phases. Phase 1 (the button) needs no storage, because the GUI already
holds every queued run's full `RunSpec` in memory (`AppState::runs`). Phase 2 (history)
reuses Phase 1's form-filling code for runs loaded from disk.

## Phase 1: Load into Generate from the Queue

A small, self-contained change in `incremusic-gui-core` and the Queue view. It doesn't touch
the core.

- **Button:** each Queue row gets **Load into Generate** (icon `ph::ARROW_SQUARE_IN`),
  next to Edit live and Remove. It works for runs in any status, including Done runs,
  until they are cleared from the queue.
- **Action:** `UiAction::LoadRunIntoForm(RunId)` reads `runs[..].state.spec`. That spec
  holds the latest params revision, because `EditRun` updates it in place.
- **Shared helper:** a `fill_form(s, name, &params, &abc_source)` helper is pulled out of
  `continue_run`, which does the same job for a song's recipe. Both callers use it: it
  sets the name, `load_params`, `abc_source` and `abc_choice`, then switches to the
  Generate tab and focuses the name.
- **Seed and count:**
  - **Seed:** the next free seed for the name (`next_free_seed(seeds_for(name))`), so the
    load continues the run rather than repeating its seeds. The existing collision check
    still offers "continue from seed N".
  - **Count:** copied from the spec. `None` sets *until stopped*.
- **Model:** the form has no model picker; it uses the configured model. If the run's
  `spec.model` differs from `s.model()`, the load goes ahead and a status message names
  both models.
- **Unsaved edits:** loading replaces the form. If the form holds a name or edited ABC
  text, ask first with a cancellable `Dialog::ReplaceForm(RunId)`. An empty or untouched
  form loads without asking.
- **Tests:**
  - **Reducer:** load a queued run and check that the form holds its name, params, ABC
    source and next free seed. Load after a live params edit and check that the form gets
    the new revision. Check that a filled form asks first.
  - **Headless:** click **Load into Generate** on a Queue row, then check that the
    Generate tab shows the run's name.

## What already exists

Much of this is already in place, but only for runs that produced a song:

| Feature | Where | What it covers |
|---|---|---|
| **Recipe** embedded in every MP3 | `media::Recipe`, DESIGN §6.2 | The exact `/v1/tasks/run` body (params and seed), model spec, server/backend/device, ABC source, run id/name/revision, timing |
| **Regenerate** | `service.rs` `regenerate()` | Reads a song's recipe back and queues the same request as a new take (`<stem>-rN`) |
| **Continue run** | `UiAction::ContinueRun` | Fills the form from a song's recipe with the next free seed |
| **Presets** | `params::Preset` (RON) | Params saved by hand. Presets never store the name or seed |
| **Project inputs** | `inputs/<name>/`, `ProjectStore::write_run_inputs` | `<name>.abc`, `lyrics.txt`, `style.txt` (**latest revision only**), plus `project.ron` with the run ids that used the project |

## The gap

A run's settings are lost in these cases:

1. **Runs that produced no song**: stopped before the first job finished, failed on
   every seed, or still queued when the app quit. There's no recipe to reload.
2. **The run as a whole**: its `start_seed`, `count`, model, and status are
   not saved anywhere. A recipe only has the seed of one song. `project.ron` lists run ids
   and nothing else.
3. **Earlier params revisions**: the project files are overwritten on every edit. A
   revision that never finished a song is gone.
4. **Deleted songs**: deleting a song deletes its recipe, which may be the only copy of
   that run's settings.
5. **The queue itself**: it is in memory only, so quitting or a crash loses queued and
   paused runs.

There's also no **Run history** view to browse past runs and reload one.

## Proposal

### 1. One record file per run

Store records at `<library_root>/runs/<run-id>.ron` (the ULID sorts by start time). This
keeps them separate from `inputs/<name>/`, because a record has to survive when its
project is deleted and has to be readable without a project.

```ron
RunRecord(
    schema: 1,
    app_version: "0.1.0",
    id: "01J8…",
    name: "sunny-hook",
    created_at: "2026-09-25T10:02:11Z",
    finished_at: Some("2026-09-25T11:40:57Z"),
    status: Done,                    // Active / Paused / Stopping / Done / Interrupted
    start_seed: 1230,
    count: Some(8),
    next_seed: 1238,
    model: ModelSpec( … ),           // snapshot at submit time
    abc_source: Transcribed( … ),
    regenerate_of: None,
    revisions: [                     // every params edit, in order; index = revision
        Revision(at: "2026-09-25T10:02:11Z", first_seed: 1230, params: GenerationParams( … )),
        Revision(at: "2026-09-25T10:31:40Z", first_seed: 1233, params: GenerationParams( … )),
    ],
    seeds: [                         // one entry per job outcome
        Seed(seed: 1230, revision: 0, outcome: Song("sunny-hook-1230")),
        Seed(seed: 1231, revision: 0, outcome: Failed("503 busy")),
        …
    ],
)
```

- Holds the **full** `GenerationParams` for every revision, including the ABC text, so a
  record can be reloaded on its own. It doesn't depend on the project folder, presets, or songs.
- `first_seed` marks the first seed each revision applied to. The seed entries also
  record their revision, so any seed can be mapped to its exact params.
- The format is RON through the existing `to_ron`/`from_ron` helpers, like presets and
  `project.ron`. Writes go through `fsutil::write_atomic`.
- Size: a record is mostly the ABC and lyrics text, a few KB for each revision.
  Records are never pruned automatically.

### 2. When records are written (core, Phase 2)

All writes go through a new `RunHistory` store in `incremusic-core` (a new
`history.rs`, next to `project.rs`) and run on the blocking pool:

| Event | Hook | Write |
|---|---|---|
| Run starts | `Service::start_run`, right after `write_run_inputs` | Create the record with revision 0 |
| Params / ABC source edited | `Command::EditRun` with `RunEdit::Params` / `AbcSource` | Append a `Revision` |
| `count` / `next_seed` edited | `Command::EditRun` | Update the fields |
| Job finished / failed for good | where the song is committed / the failure is reported | Append a `Seed` |
| Pause / resume / stop / done | scheduler status changes | Update `status`, `finished_at` |
| App starts | startup | Any record still `Active`/`Paused`/`Stopping` → `Interrupted` |

If a write fails, the error is logged as a warning (source `history`) and the run keeps
going. This works the same way as a failed `write_run_inputs` today.

### 3. Reloading

New commands and events, next to the existing `Regenerate`/`Recipe` ones:

- `Command::ListRuns` → `Event::RunHistory(Vec<RunSummary>)` (id, name, created_at,
  status, seeds done/failed, model id). This command only reads a small header, so it
  stays cheap for large histories.
- `Command::LoadRun(RunId, Option<u32 /* revision */>)` → `Event::RunLoaded(...)`, which
  fills the Generate form with that revision's params (the latest one by default), plus
  model, ABC source, `start_seed`, and `count`.
- **Name handling** stays as in DESIGN §5.1.1. Loading a record **pre-fills** the name, and the usual collision check then
  offers "continue from next free seed". "Start again with the same seeds" is a separate
  choice, and it saves new takes as `-2`, `-3`, … as it does today.
- **Resume interrupted run**: for an `Interrupted` record, start a new run that uses the
  same name and params, starting at `next_seed`, with the remaining `count`.
  This is a new run id with `resumed_from: Some(<old id>)`. The old record is never
  changed after it is closed.
- **Save as preset** from a record: reuses `Preset::save`.
- Songs link back: `recipe.run.id` → the record, so a song's context menu can offer
  *Open run*.

### 4. GUI

- A **History** tab next to Queue, with one row per run, newest first. Each row shows the
  name, date, status, number of seeds done or failed, and model. A filter searches by name.
- Selecting a row shows the revisions, each seed's outcome (a song links to the library),
  and a read-only params view like the recipe view.
- Buttons: **Load into Generate** and **Load revision N** (both reuse Phase 1's
  `fill_form`), **Resume** (only for interrupted runs), **Save as preset**, **Reveal
  file**.
- The Queue's **Clear finished runs** only removes runs from the live queue. Their records
  stay in History, and the confirmation text says so.
- The actions go into `incremusic-gui-core` as `UiAction`s so the headless tests can drive
  them.

### 5. Optional follow-ups (not part of the first version)

- **Restoring the queue on startup**: offer to re-queue `Interrupted` runs automatically.
  This is kept separate because it touches server state, and a failed resume shouldn't
  block startup.
- **Import/export** a record as a single file for sharing. The RON file already works for
  this, so it only needs a menu entry.
- **Backfill**: build records for past runs from the recipes of songs already in the
  library, grouped by `recipe.run.id`. They are marked `backfilled: true`, because
  revisions that never finished a song can't be recovered.

## Tests

- `history.rs` unit tests: round-trip a record, add revisions and seeds, mark records
  `Interrupted` at startup, and survive a corrupt file (skip it and log, don't fail the
  list).
- Core integration test (like the ones in `tests/launcher.rs`), against a fake server: start a run, edit params
  mid-run, let one seed fail. The record should show 2 revisions and the right seed
  outcomes, and `LoadRun` should return revision 1's params.
- Stop a run before any job finishes. The record should still exist and reload.
- `incremusic-gui-core` tests: `RunLoaded` fills the form, keeps the name, and triggers the
  collision check. A new form stays unchanged.
- The real-server test `generates_one_song_through_the_core` also asserts that the record
  was written.

## Decisions (2026-09-26)

0. **Phase 1 seed:** continue the run. The form gets the next free seed, never the
   original `start_seed`.
1. **Where records live:** in `runs/` at the library root.
2. **Regenerate:** each regenerate gets its own record, with `regenerate_of` set.
3. **Pruning:** none for now.
4. **DESIGN.md:** not updated yet; this file is the reference.

## As built

- **Code:**
  - **Core:** `incremusic-core/src/history.rs`, which has `RunHistory`, `RunRecord` and
    `Recorder`. `Recorder` wraps the scheduler's event sink, so every run and job update
    is recorded in one place. It doesn't hook into each command.
  - **Commands and events:** `Command::ListRuns`, `LoadRunRecord` and
    `ResumeInterrupted`, plus `Event::RunHistory` and `RunRecord`.
- **Writes:** records go to disk on a `run-history` writer thread, so no file I/O
  happens under the scheduler lock. The service closes the store on shutdown, which
  writes anything still pending.
- **If `runs/` can't be opened:** the error is logged, and runs still work without being
  recorded.
- **Status:** a record can also be `Removed`, when it is deleted from the queue before it
  finished.
- **Seed entries:** a cancelled seed is recorded as `Cancelled`. If the same seed later
  gets a new outcome, the entry is replaced.
- **Resume:** redoes the seeds that were running when the app quit (issued, but with no
  outcome), then the seeds that never started. It starts a new run with `resumed_from`
  set, and never changes the old record.
- **Load into Generate:**
  - **When it asks first:** only when the form's **Name** is filled in
    (`Dialog::ReplaceForm`). ABC text is nearly always present, so asking about it too
    would prompt on every load.
  - **Seed:** the higher of the run's `next_seed` and the library's next free seed for
    the name.
- **Not built:** *Save as preset* on a History record. Load the record into Generate,
  then save the preset from there.
- **Test harness:** `Harness::start` now uses a private `run_dir`. The default run dir
  may hold a real app's pidfile for `gpu1`, which made the mock `gpu1` go down mid-test.
