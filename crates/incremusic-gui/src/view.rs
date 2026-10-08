//! Panels (design §8). Pure drawing: reads `AppState`, pushes `Out`s. Every interactive
//! widget has a unique accessible label so the headless tests can find it.

use std::time::{Duration, SystemTime};

use egui::{Color32, RichText, Sense, TextEdit, Ui};
use egui_phosphor::regular as ph;
use incremusic_core::history::{RecordStatus, RunSummary};
use incremusic_core::library::{Location, Song, SongId};
use incremusic_core::media::{ExportFormat, Rating};
use incremusic_core::params::Sampling;
use incremusic_core::playback::PlayState;
use incremusic_core::project::{Keypoints, reference_play_id};
use incremusic_core::run::{AbcSource, RunStatus};
use incremusic_core::scheduler::JobState;
use incremusic_core::service::{LogLevel, ServerState};
use incremusic_gui_core::{
    AbcChoice, AppState, Blocker, Dialog, DropKind, Focus, FolderFilter, FormLoad, HistorySort,
    LibraryFilter, LogSource, ProjectSort, ReferenceSort, ReviewKey, SongSort, SortColumn, Sorting,
    Tab, UiAction, fmt_duration, fmt_keypoint, job_label, parse_keypoint,
};

use crate::{Effect, Out};

type O = Vec<Out>;

fn act(out: &mut O, a: UiAction) {
    out.push(Out::Ui(a));
}

pub fn draw(ui: &mut Ui, s: &AppState, out: &mut O) {
    egui::Panel::top("servers").show(ui, |ui| servers_bar(ui, s, out));
    egui::Panel::top("tabs").show(ui, |ui| tabs(ui, s, out));
    egui::Panel::bottom("status").show(ui, |ui| status_bar(ui, s, out));
    egui::CentralPanel::default_margins().show(ui, |ui| match s.tab {
        Tab::Generate => generate(ui, s, out),
        Tab::Queue => queue(ui, s, out),
        Tab::History => history(ui, s, out),
        Tab::Projects => projects(ui, s, out),
        Tab::Inputs => inputs(ui, s, out),
        Tab::Library => library(ui, s, out),
        Tab::Review => review(ui, s, out),
        Tab::Log => log(ui, s, out),
    });
    dialogs(ui.ctx(), s, out);
    keyboard(ui.ctx(), s, out);
}

// ---------------------------------------------------------------------------------------
// Helpers

/// A button that shows a Phosphor icon before its text. The accessible label is the text
/// alone, so labels stay stable for screen readers and the headless tests.
pub struct IconButton {
    icon: &'static str,
    text: String,
    small: bool,
    strong: bool,
    bare: bool,
}

pub fn icon_btn(icon: &'static str, text: impl Into<String>) -> IconButton {
    IconButton {
        icon,
        text: text.into(),
        small: false,
        strong: false,
        bare: false,
    }
}

impl IconButton {
    /// Bold text, for primary actions.
    pub fn strong(mut self) -> Self {
        self.strong = true;
        self
    }

    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// The icon alone, with the text as its tooltip.
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }
}

impl egui::Widget for IconButton {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let label = RichText::new(if self.bare {
            self.icon.to_string()
        } else {
            format!("{} {}", self.icon, self.text)
        });
        let mut b = egui::Button::new(if self.strong { label.strong() } else { label });
        if self.small {
            b = b.small();
        }
        let mut r = ui.add(b);
        if self.bare {
            r = r.on_hover_text(&self.text);
        }
        let enabled = ui.is_enabled();
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &self.text));
        r
    }
}

fn ib(ui: &mut Ui, icon: &'static str, text: &str) -> egui::Response {
    ui.add(icon_btn(icon, text))
}

fn ib_small(ui: &mut Ui, icon: &'static str, text: &str) -> egui::Response {
    ui.add(icon_btn(icon, text).small())
}

/// A tab (selectable label) with an icon; accessible label is the text alone.
fn icon_tab(ui: &mut Ui, selected: bool, icon: &'static str, text: &str) -> egui::Response {
    let r = ui.selectable_label(selected, format!("{icon} {text}"));
    let enabled = ui.is_enabled();
    r.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, selected, text)
    });
    r
}

fn rating_icon(r: Rating) -> &'static str {
    match r {
        Rating::Good => ph::THUMBS_UP,
        Rating::Neutral => ph::SMILEY_MEH,
        Rating::Bad => ph::THUMBS_DOWN,
    }
}

/// A labelled single-line text field; returns the new value when edited.
fn field(
    ui: &mut Ui,
    label: &str,
    salt: &str,
    value: &str,
    width: f32,
) -> (egui::Response, Option<String>) {
    let l = ui.label(label);
    let mut v = value.to_string();
    let r = ui
        .add(
            TextEdit::singleline(&mut v)
                .id_salt(salt)
                .desired_width(width),
        )
        .labelled_by(l.id);
    let changed = r.changed().then_some(v);
    (r, changed)
}

fn area(
    ui: &mut Ui,
    label: &str,
    salt: &str,
    value: &str,
    rows: usize,
) -> (egui::Response, Option<String>) {
    let l = ui.label(label);
    let mut v = value.to_string();
    let r = ui
        .add(
            TextEdit::multiline(&mut v)
                .id_salt(salt)
                .desired_rows(rows)
                .desired_width(f32::INFINITY)
                .code_editor(),
        )
        .labelled_by(l.id);
    let changed = r.changed().then_some(v);
    (r, changed)
}

fn warn_text(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(ui.visuals().warn_fg_color));
}

fn since(t: SystemTime) -> String {
    fmt_duration(SystemTime::now().duration_since(t).unwrap_or_default())
}

// ---------------------------------------------------------------------------------------
// Servers (top bar)

fn servers_bar(ui: &mut Ui, s: &AppState, out: &mut O) {
    ui.horizontal_wrapped(|ui| {
        ui.strong("Servers");
        if s.servers.is_empty() {
            ui.label("none configured — library browser and player only");
        }
        for v in &s.servers {
            let id = v.info.id;
            let name = &v.info.name;
            ui.separator();
            let color = match &v.state {
                ServerState::Ready { .. } => Color32::from_rgb(80, 180, 80),
                ServerState::Busy { .. } => Color32::from_rgb(90, 150, 230),
                ServerState::Starting { .. } | ServerState::Stopping { .. } => {
                    Color32::from_rgb(220, 170, 50)
                }
                ServerState::Down(_) => Color32::from_rgb(220, 80, 80),
                ServerState::Stopped => Color32::GRAY,
            };
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, color);
            let mut text = format!("{name} :{} — {}", v.info.port, v.state.label());
            match &v.state {
                ServerState::Ready {
                    backend,
                    loaded_models,
                } => {
                    if let Some(b) = backend {
                        text += &format!(" [{b}]");
                    }
                    if !loaded_models.is_empty() {
                        text += &format!(" loaded: {}", loaded_models.join(", "));
                    }
                }
                ServerState::Busy { job, since: t } => {
                    let seed = job
                        .and_then(|j| s.jobs.get(&j))
                        .map(|j| format!(" seed {}", j.seed))
                        .unwrap_or_default();
                    text += &format!("{seed}, {}", since(*t));
                }
                ServerState::Stopping { since: t } | ServerState::Starting { since: t } => {
                    text += &format!(" ({})", since(*t))
                }
                _ => {}
            }
            ui.label(text);
            if !v.path_problems.is_empty() {
                let n = v.path_problems.len();
                ui.label(
                    RichText::new(format!(
                        "{} {n} model path{} missing on {name}",
                        ph::WARNING,
                        if n == 1 { "" } else { "s" }
                    ))
                    .color(ui.visuals().warn_fg_color),
                )
                .on_hover_text(v.path_problems.join("\n"));
            }
            if v.info.launchable {
                let can = v.state.can_launch();
                let r = ui.add_enabled(can, icon_btn(ph::POWER, format!("Launch {name}")));
                let r = if matches!(v.state, ServerState::Stopping { .. }) {
                    r.on_disabled_hover_text("Waiting for the old process to exit")
                } else {
                    r
                };
                if r.clicked() {
                    act(out, UiAction::LaunchServer(id));
                }
            }
            let can_stop = matches!(
                v.state,
                ServerState::Ready { .. } | ServerState::Busy { .. } | ServerState::Starting { .. }
            );
            if ui
                .add_enabled(can_stop, icon_btn(ph::STOP_CIRCLE, format!("Stop {name}")))
                .clicked()
            {
                act(out, UiAction::StopServer(id));
            }
            if matches!(v.state, ServerState::Down(_) | ServerState::Stopped)
                && ib(ui, ph::ARROWS_CLOCKWISE, &format!("Recheck {name}")).clicked()
            {
                act(out, UiAction::RecheckServer(id));
            }
        }
        if let Some(init) = &s.init {
            ui.separator();
            match &init.opener {
                Ok(o) => ui.weak(format!("terminal: {o}")),
                Err(e) => ui.label(RichText::new(e).color(ui.visuals().warn_fg_color)),
            };
            if let Err(e) = &init.ffmpeg {
                ui.label(
                    RichText::new(format!("ffmpeg missing: {e}"))
                        .color(ui.visuals().error_fg_color),
                );
            }
        }
    });
}

fn tabs(ui: &mut Ui, s: &AppState, out: &mut O) {
    ui.horizontal(|ui| {
        let unreviewed = s
            .library
            .values()
            .filter(|x| x.location == Location::Unreviewed)
            .count();
        let (runs, songs, open_ended) = s.queued_totals();
        let more = if open_ended { "+" } else { "" };
        for (t, icon, label) in [
            (Tab::Generate, ph::MAGIC_WAND, "Generate".to_string()),
            (
                Tab::Queue,
                ph::QUEUE,
                format!("Queue ({runs} · {songs}{more})"),
            ),
            (
                Tab::History,
                ph::CLOCK_COUNTER_CLOCKWISE,
                "History".to_string(),
            ),
            (
                Tab::Projects,
                ph::FOLDERS,
                format!("Projects ({})", s.projects.len()),
            ),
            (
                Tab::Inputs,
                ph::FILE_AUDIO,
                format!("Inputs ({})", s.reference_rows().len()),
            ),
            (
                Tab::Library,
                ph::BOOKS,
                format!("Audio Library ({})", s.library.len()),
            ),
            (
                Tab::Review,
                ph::HEADPHONES,
                format!("Review ({unreviewed})"),
            ),
            (Tab::Log, ph::SCROLL, "Log".to_string()),
        ] {
            if icon_tab(ui, s.tab == t, icon, &label).clicked() && s.tab != t {
                act(out, UiAction::SelectTab(t));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            now_playing(ui, s, out);
        });
    });
}

fn tab_name(t: Tab) -> &'static str {
    match t {
        Tab::Generate => "Generate",
        Tab::Queue => "Queue",
        Tab::History => "History",
        Tab::Projects => "Projects",
        Tab::Inputs => "Inputs",
        Tab::Library => "Audio Library",
        Tab::Review => "Review",
        Tab::Log => "Log",
    }
}

/// Global transport at the right of the tab bar, so playback can be paused or resumed from
/// any tab. Names the tab it was started from; clicking that name goes back there.
/// Laid out right to left.
fn now_playing(ui: &mut Ui, s: &AppState, out: &mut O) {
    let p = &s.player;
    let Some(state @ (PlayState::Playing | PlayState::Paused | PlayState::Loading)) = p.state
    else {
        return;
    };
    let Some(id) = &p.song else { return };
    if ib_small(ui, ph::STOP, "Stop now playing").clicked() {
        act(out, UiAction::StopPlayback);
    }
    let r = match state {
        PlayState::Playing => ib_small(ui, ph::PAUSE, "Pause playback"),
        _ => ui.add_enabled(
            state == PlayState::Paused,
            icon_btn(ph::PLAY, "Resume playback").small(),
        ),
    };
    if r.clicked() {
        act(out, UiAction::PauseResume);
    }
    if state == PlayState::Loading {
        ui.spinner();
    }
    ui.monospace(format!(
        "{} / {}",
        fmt_duration(p.position),
        fmt_duration(p.duration)
    ));
    let title = match s.library.get(id) {
        Some(song) => song.title().to_string(),
        None => s
            .reference_rows()
            .into_iter()
            .find(|r| s.playing_reference(&r.project.name))
            .map(|r| r.reference.original_name.clone())
            .unwrap_or_else(|| id.0.clone()),
    };
    ui.add(egui::Label::new(&title).truncate())
        .on_hover_text(&title);
    if let Some(t) = p.tab {
        let r = ui
            .link(format!("{} {}", ph::SPEAKER_HIGH, tab_name(t)))
            .on_hover_text(format!(
                "Playing from the {} tab — click to go there",
                tab_name(t)
            ));
        if r.clicked() && s.tab != t {
            act(out, UiAction::SelectTab(t));
        }
    }
    ui.separator();
}

fn status_bar(ui: &mut Ui, s: &AppState, out: &mut O) {
    ui.horizontal(|ui| {
        if let Some(p) = &s.transcribe.project {
            ui.spinner();
            ui.label(format!("Transcribing “{p}” (waits for an idle server)…"));
        }
        if let Some(msg) = &s.status {
            ui.label(msg);
            if ib_small(ui, ph::X, "Dismiss").clicked() {
                act(out, UiAction::DismissStatus);
            }
        }
        if let Some(l) = s.last_error() {
            ui.separator();
            ui.label(
                RichText::new(format!("last error: {}: {}", l.source, l.message))
                    .color(ui.visuals().error_fg_color),
            );
        }
    });
}

// ---------------------------------------------------------------------------------------
// Generate

/// Files dropped onto the Generate panel (§5.2). While files hover, an overlay says
/// what a drop would do.
fn file_drop(ui: &mut Ui, s: &AppState, out: &mut O) {
    let (hovered, dropped) = ui.input(|i| {
        (
            i.raw
                .hovered_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect::<Vec<_>>(),
        )
    });
    if !dropped.is_empty() {
        match s.drop_target(&dropped) {
            Ok((DropKind::Abc, p)) => out.push(Out::Effect(Effect::ReadAbcFile(p))),
            Ok((DropKind::Audio, p)) => act(out, UiAction::TranscribeFile(p)),
            Err(why) => act(out, UiAction::ShowStatus(why)),
        }
    }
    if hovered.is_empty() {
        return;
    }
    let (text, color) = match s.drop_target(&hovered) {
        Ok((DropKind::Abc, _)) => (
            "Drop to load this ABC file".to_string(),
            ui.visuals().selection.bg_fill,
        ),
        Ok((DropKind::Audio, _)) => (
            "Drop to transcribe this audio".to_string(),
            ui.visuals().selection.bg_fill,
        ),
        Err(why) => (why, ui.visuals().warn_fg_color),
    };
    let rect = ui.max_rect();
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("file-drop"),
    ));
    painter.rect_filled(rect, 6.0, color.gamma_multiply(0.25));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(22.0),
        ui.visuals().strong_text_color(),
    );
}

fn generate(ui: &mut Ui, s: &AppState, out: &mut O) {
    file_drop(ui, s, out);
    egui::ScrollArea::vertical()
        .id_salt("generate")
        .show(ui, |ui| {
            let f = &s.form;
            // name first (§5.2.1)
            ui.horizontal(|ui| {
                let (r, v) = field(ui, "Name", "name", &f.name, 260.0);
                if let Some(v) = v {
                    act(out, UiAction::SetName(v));
                }
                if s.focus == Some(Focus::Name) {
                    r.request_focus();
                    act(out, UiAction::FocusHandled);
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    act(out, UiAction::SubmitName);
                }
                if s.offers_project() && ib(ui, ph::FOLDER_OPEN, "Load project inputs").clicked() {
                    act(out, UiAction::LoadProjectInputs);
                }
            });
            if let Err(e) = f.name_result() {
                warn_text(
                    ui,
                    format!("Name: {e}. It names the project and the songs (<name>-<seed>)."),
                );
            }
            ui.horizontal(|ui| {
                ui.label("Preset:");
                match &f.preset_path {
                    Some(p) => ui.weak(p.display().to_string()),
                    None => ui.weak("(none)"),
                };
                if ib(ui, ph::FOLDER_OPEN, "Load preset…").clicked() {
                    out.push(Out::Effect(Effect::PickPresetToLoad));
                }
                if ib(ui, ph::FLOPPY_DISK, "Save preset…").clicked() {
                    out.push(Out::Effect(Effect::PickPresetToSave));
                }
            });
            ui.separator();

            abc_section(ui, s, out);

            ui.separator();
            lyrics_section(ui, s, out);
            let (_, v) = area(ui, "Style", "style", &f.params.style, 2);
            if let Some(v) = v {
                let mut p = f.params.clone();
                p.style = v;
                act(out, UiAction::SetParams(p));
            }

            let open = egui::CollapsingHeader::new("Advanced sampling")
                .id_salt("advanced")
                .open(Some(f.advanced_open))
                .show(ui, |ui| {
                    let mut p = f.params.clone();
                    if params_editor(ui, "form", &mut p) {
                        act(out, UiAction::SetParams(p));
                    }
                });
            if open.header_response.clicked() {
                act(out, UiAction::SetAdvancedOpen(!f.advanced_open));
            }

            ui.separator();
            ui.horizontal(|ui| {
                let (_, v) = field(ui, "Starting seed", "seed", &f.seed, 110.0);
                if let Some(v) = v {
                    act(out, UiAction::SetSeed(v));
                }
                if ib(ui, ph::DICE_FIVE, "Random seed").clicked() {
                    act(out, UiAction::RandomSeed(random_u32()));
                }
                ui.separator();
                ui.add_enabled_ui(!f.until_stopped, |ui| {
                    let (_, v) = field(ui, "Count", "count", &f.count, 60.0);
                    if let Some(v) = v {
                        act(out, UiAction::SetCount(v));
                    }
                });
                let mut until = f.until_stopped;
                if ui.checkbox(&mut until, "until stopped").changed() {
                    act(out, UiAction::SetUntilStopped(until));
                }
            });

            let blocker = s.start_blocker();
            ui.horizontal(|ui| {
                let r = ui.add_enabled(
                    blocker.is_none(),
                    icon_btn(ph::ROCKET_LAUNCH, "Start run").strong(),
                );
                if let Some(b) = &blocker {
                    let r = r.on_disabled_hover_text(b.message());
                    let _ = r;
                    warn_text(ui, b.message());
                } else if r.clicked() {
                    act(out, UiAction::StartRun);
                }
                if let Some(Blocker::Collision(c)) = &blocker
                    && let Some(n) = c.continue_from
                    && ib(ui, ph::FAST_FORWARD, &format!("Continue from seed {n}")).clicked()
                {
                    act(out, UiAction::ContinueFromSeed(n));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ib(ui, ph::ERASER, "Clear")
                        .on_hover_text(
                            "Reset the form: name, ABC, lyrics, style, seed, count and parameters",
                        )
                        .clicked()
                    {
                        act(out, UiAction::ClearForm);
                    }
                });
            });
        });
}

fn lyrics_section(ui: &mut Ui, s: &AppState, out: &mut O) {
    let f = &s.form;
    let lines = f
        .params
        .lyrics
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let title = if f.lyrics_open || lines == 0 {
        "Lyrics".to_string()
    } else {
        format!("Lyrics ({lines} lines)")
    };
    let open = egui::CollapsingHeader::new(title)
        .id_salt("lyrics-section")
        .open(Some(f.lyrics_open))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ib(ui, ph::FOLDER_OPEN, "Load lyrics…")
                    .on_hover_text("Replace the lyrics with a text file's contents")
                    .clicked()
                {
                    out.push(Out::Effect(Effect::PickLyricsFile));
                }
                let has_lyrics = !f.params.lyrics.trim().is_empty();
                if ui
                    .add_enabled(has_lyrics, icon_btn(ph::FLOPPY_DISK, "Save lyrics…"))
                    .clicked()
                {
                    let file_name = match f.name_result() {
                        Ok(n) => format!("{n}-lyrics.txt"),
                        Err(_) => "lyrics.txt".into(),
                    };
                    out.push(Out::Effect(Effect::SaveLyricsAs(
                        f.params.lyrics.clone(),
                        file_name,
                    )));
                }
            });
            let (_, v) = area(ui, "Lyrics", "lyrics", &f.params.lyrics, 8);
            if let Some(v) = v {
                let mut p = f.params.clone();
                p.lyrics = v;
                act(out, UiAction::SetParams(p));
            }
        });
    if open.header_response.clicked() {
        act(out, UiAction::SetLyricsOpen(!f.lyrics_open));
    }
}

fn abc_section(ui: &mut Ui, s: &AppState, out: &mut O) {
    let f = &s.form;
    let open = egui::CollapsingHeader::new("ABC melody (recommended)")
        .id_salt("abc-section")
        .open(Some(f.abc_section_open))
        .show(ui, |ui| {
            let blocked = s.inputs_blocker();
            ui.horizontal(|ui| {
                for (c, label) in [
                    (AbcChoice::LoadFile, "Load .abc file"),
                    (AbcChoice::Transcribe, "Transcribe from audio"),
                    (AbcChoice::Paste, "Paste / edit"),
                    (AbcChoice::None, "None: let YuE2 compose"),
                ] {
                    if ui.radio(f.abc_choice == c, label).clicked() && f.abc_choice != c {
                        act(out, UiAction::SetAbcChoice(c));
                    }
                }
            });
            ui.horizontal(|ui| {
                let load = ui.add_enabled(
                    blocked.is_none(),
                    icon_btn(ph::FILE_TEXT, "Load .abc…").strong(),
                );
                let load = match blocked {
                    Some(b) => load.on_disabled_hover_text(b),
                    None => load,
                };
                if s.focus == Some(Focus::AbcSection) && blocked.is_none() {
                    load.request_focus();
                    load.scroll_to_me(None);
                    act(out, UiAction::FocusHandled);
                }
                if load.clicked() {
                    out.push(Out::Effect(Effect::PickAbcFile));
                }
                // no name needed: picking a file asks for one if it's missing
                if ui
                    .add_enabled(
                        s.transcribe.project.is_none(),
                        icon_btn(ph::WAVEFORM, "Transcribe audio…").strong(),
                    )
                    .clicked()
                {
                    out.push(Out::Effect(Effect::PickReferenceAudio));
                }
                let has_ref = s.project(&f.name).is_some_and(|p| p.has_reference);
                if ui
                    .add_enabled(
                        has_ref && s.transcribe.project.is_none(),
                        icon_btn(ph::ARROW_CLOCKWISE, "Re-transcribe"),
                    )
                    .clicked()
                {
                    act(out, UiAction::Retranscribe);
                }
                if ui
                    .add_enabled(
                        !f.abc_text.trim().is_empty(),
                        icon_btn(ph::FLOPPY_DISK, "Save as .abc…"),
                    )
                    .clicked()
                {
                    out.push(Out::Effect(Effect::SaveAbcAs(f.abc_text.clone())));
                }
            });
            if let Some(p) = &s.transcribe.project {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Transcribing reference for “{p}”…"));
                });
            } else if let Some(e) = &s.transcribe.error {
                ui.label(
                    RichText::new(format!("Transcription failed: {e}"))
                        .color(ui.visuals().error_fg_color),
                );
            } else if s.transcribe.reused {
                ui.weak("Reused an existing transcription of this audio (no server call).");
            }
            let src = match &f.abc_source {
                AbcSource::File { file_name, .. } => format!("from file {file_name}"),
                AbcSource::Transcribed(r) => {
                    format!("transcribed from {} ({})", r.file_name, r.format)
                }
                AbcSource::Manual => "typed or pasted".into(),
                AbcSource::None => "none".into(),
            };
            ui.weak("Or drop an .abc file or any audio file onto this panel.");
            if f.abc_choice == AbcChoice::None {
                ui.weak("No ABC will be sent; YuE2 writes its own melody.");
            } else {
                ui.weak(format!("Source: {src}"));
                let (r, v) = area(ui, "ABC", "abc", &f.abc_text, 10);
                if let Some(v) = v {
                    act(out, UiAction::SetAbcText(v));
                }
                if s.focus == Some(Focus::Abc) {
                    r.request_focus();
                    act(out, UiAction::FocusHandled);
                }
                let m = &f.abc_summary;
                if !f.abc_text.trim().is_empty() {
                    let d = |o: &Option<String>| o.clone().unwrap_or_else(|| "?".into());
                    ui.label(format!(
                        "Meter {} · key {} · tempo {} · voices {} · {} bars{}",
                        d(&m.meter),
                        d(&m.key),
                        d(&m.tempo),
                        if m.voices.is_empty() {
                            "1".into()
                        } else {
                            m.voices.join(", ")
                        },
                        m.bars,
                        if m.sections.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " · sections: {}",
                                m.sections.join(&format!(" {} ", ph::ARROW_RIGHT))
                            )
                        }
                    ));
                    for w in &m.warnings {
                        warn_text(ui, format!("{} {w} (you can still submit)", ph::WARNING));
                    }
                }
            }
        });
    if open.header_response.clicked() {
        act(out, UiAction::SetAbcSectionOpen(!f.abc_section_open));
    }
}

fn sampling_editor(ui: &mut Ui, salt: &str, title: &str, sm: &mut Sampling) -> bool {
    let mut c = false;
    ui.label(RichText::new(title).strong());
    egui::Grid::new(format!("{salt}-{title}"))
        .num_columns(2)
        .show(ui, |ui| {
            let mut row =
                |ui: &mut Ui, name: &str, w: &mut dyn FnMut(&mut Ui) -> egui::Response| {
                    let l = ui.label(name);
                    let r = w(ui).labelled_by(l.id);
                    c |= r.changed();
                    ui.end_row();
                };
            row(ui, &format!("{title} temperature"), &mut |ui| {
                ui.add(
                    egui::DragValue::new(&mut sm.temperature)
                        .range(0.01..=5.0)
                        .speed(0.01),
                )
            });
            row(ui, &format!("{title} top p"), &mut |ui| {
                ui.add(
                    egui::DragValue::new(&mut sm.top_p)
                        .range(0.01..=1.0)
                        .speed(0.01),
                )
            });
            row(ui, &format!("{title} top k"), &mut |ui| {
                ui.add(egui::DragValue::new(&mut sm.top_k).range(0..=10000))
            });
            row(ui, &format!("{title} repetition penalty"), &mut |ui| {
                ui.add(
                    egui::DragValue::new(&mut sm.repetition_penalty)
                        .range(0.5..=3.0)
                        .speed(0.001),
                )
            });
            row(ui, &format!("{title} penalty window"), &mut |ui| {
                ui.add(egui::DragValue::new(&mut sm.penalty_window).range(0..=100000))
            });
            row(ui, &format!("{title} min tokens"), &mut |ui| {
                ui.add(egui::DragValue::new(&mut sm.min_tokens).range(0..=100000))
            });
            row(ui, &format!("{title} max tokens"), &mut |ui| {
                ui.add(egui::DragValue::new(&mut sm.max_tokens).range(1..=100000))
            });
        });
    c
}

fn params_editor(
    ui: &mut Ui,
    salt: &str,
    p: &mut incremusic_core::params::GenerationParams,
) -> bool {
    let mut c = false;
    egui::Grid::new(format!("{salt}-gen"))
        .num_columns(2)
        .show(ui, |ui| {
            let l = ui.label("Guidance scale");
            c |= ui
                .add(
                    egui::DragValue::new(&mut p.guidance_scale)
                        .range(0.01..=20.0)
                        .speed(0.01),
                )
                .labelled_by(l.id)
                .changed();
            ui.end_row();
            let l = ui.label("Inference steps");
            c |= ui
                .add(egui::DragValue::new(&mut p.num_inference_steps).range(1..=200))
                .labelled_by(l.id)
                .changed();
            ui.end_row();
            let l = ui.label("CoT");
            c |= ui
                .add(
                    TextEdit::singleline(&mut p.cot)
                        .id_salt(format!("{salt}-cot"))
                        .desired_width(80.0),
                )
                .labelled_by(l.id)
                .changed();
            ui.end_row();
        });
    ui.columns(2, |cols| {
        c |= sampling_editor(&mut cols[0], salt, "ABC", &mut p.abc_sampling);
        c |= sampling_editor(&mut cols[1], salt, "Semantic", &mut p.semantic_sampling);
    });
    c
}

fn random_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    h.finish() as u32
}

// ---------------------------------------------------------------------------------------
// Queue

fn queue(ui: &mut Ui, s: &AppState, out: &mut O) {
    ui.horizontal(|ui| {
        ui.heading("Queue");
        if s.runs.iter().any(|r| r.state.status == RunStatus::Done)
            && ib(ui, ph::BROOM, "Clear finished runs")
                .on_hover_text("Removes them from the queue; they stay in History")
                .clicked()
        {
            act(out, UiAction::ClearFinishedRuns);
        }
    });
    if s.runs.is_empty() {
        ui.weak("No runs yet. Start one from the Generate tab.");
        return;
    }
    let n = s.runs.len();
    egui::ScrollArea::vertical().id_salt("queue").show(ui, |ui| {
        for (i, r) in s.runs.iter().enumerate() {
            let id = r.state.id;
            let name = r.state.spec.name.to_string();
            let st = &r.state;
            ui.group(|ui| {
                ui.horizontal_wrapped(|ui| {
                    let status = match st.status {
                        RunStatus::Active => "active",
                        RunStatus::Paused => "paused",
                        RunStatus::Stopping => "stopping",
                        RunStatus::Done => "done",
                    };
                    let count = st.spec.count.map(|c| c.to_string()).unwrap_or_else(|| "∞".into());
                    ui.label(
                        RichText::new(format!(
                            "{name} — {status} — {} done, {} running, {} failed of {count} · next seed {} · rev {}",
                            r.done, r.running, r.failed, st.next_seed, st.revision
                        ))
                        .strong(),
                    );
                    let live = st.status != RunStatus::Done;
                    if live {
                        if st.status == RunStatus::Paused {
                            if ib(ui, ph::PLAY, &format!("Resume {name}")).clicked() {
                                act(out, UiAction::ResumeRun(id));
                            }
                        } else if st.status == RunStatus::Active && ib(ui, ph::PAUSE, &format!("Pause {name}")).clicked() {
                            act(out, UiAction::PauseRun(id));
                        }
                        if st.status != RunStatus::Stopping
                            && ib(ui, ph::STOP, &format!("Stop {name}")).on_hover_text("No new seeds; running jobs finish and are kept").clicked()
                        {
                            act(out, UiAction::StopRun(id));
                        }
                    }
                    if ui.add_enabled(i > 0, icon_btn(ph::ARROW_UP, format!("Move {name} up"))).clicked() {
                        act(out, UiAction::MoveRun(id, i - 1));
                    }
                    if ui.add_enabled(i + 1 < n, icon_btn(ph::ARROW_DOWN, format!("Move {name} down"))).clicked() {
                        act(out, UiAction::MoveRun(id, i + 1));
                    }
                    if ib(ui, ph::PENCIL_SIMPLE, &format!("Edit {name}")).clicked() {
                        act(out, UiAction::ToggleRunEditor(id));
                    }
                    if ib(ui, ph::ARROW_SQUARE_IN, &format!("Load {name} into Generate"))
                        .on_hover_text("Fill the Generate form with this run's settings, continuing after its last seed")
                        .clicked()
                    {
                        act(out, UiAction::LoadIntoForm(FormLoad::Queue(id)));
                    }
                    if st.status != RunStatus::Active
                        && ib(ui, ph::TRASH, &format!("Delete {name}"))
                            .on_hover_text("Remove from the queue; unfinished jobs are cancelled")
                            .clicked()
                    {
                        act(out, UiAction::RemoveRun(id));
                    }
                });
                if let Some(ed) = s.editors.get(&id).filter(|e| e.open) {
                    run_editor(ui, s, out, id, &name, ed);
                }
                for j in s.jobs_of(id) {
                    ui.horizontal(|ui| {
                        let server = match &j.state {
                            JobState::Running { server, .. } | JobState::Cancelling { server, .. } => format!(" on {}", s.server_name(*server)),
                            _ => String::new(),
                        };
                        let retry = if j.attempts > 0 { format!(" (attempt {})", j.attempts + 1) } else { String::new() };
                        let took = match (&j.state, &j.timing) {
                            (JobState::Done(_), Some(t)) => format!(" in {:.1} s", t.wall_ms as f64 / 1000.0),
                            _ => String::new(),
                        };
                        ui.label(format!(
                            "{name} seed {} · rev {}{server}: {}{took}{retry}",
                            j.seed,
                            j.revision,
                            job_label(&j.state, SystemTime::now())
                        ));
                        let cancellable = matches!(j.state, JobState::Queued | JobState::Running { .. });
                        if cancellable && ib_small(ui, ph::X, &format!("Cancel {name} seed {}", j.seed)).clicked() {
                            act(out, UiAction::CancelJob(j.id));
                        }
                    });
                }
                if let Some(rate) = s.song_rate(id) {
                    let text = match rate.overall {
                        Some(all) => format!(
                            "{all:.1} s/song across servers · {:.1} s/song per server · {} finished",
                            rate.per_server, rate.finished
                        ),
                        None => format!("{:.1} s/song per server · {} finished", rate.per_server, rate.finished),
                    };
                    let eta = rate
                        .eta_minutes
                        .map(|m| format!(" · ~{} min left", m.ceil() as u64))
                        .unwrap_or_default();
                    ui.weak(format!("{text}{eta}")).on_hover_text(
                        "Across servers: time any server spent generating this run, divided by finished songs (pauses and waits excluded).\nPer server: average generation time of one song.\nMinutes left: songs still to make times the across-servers rate.",
                    );
                }
            });
        }
    });
}

fn run_editor(
    ui: &mut Ui,
    s: &AppState,
    out: &mut O,
    id: incremusic_core::RunId,
    name: &str,
    ed: &incremusic_gui_core::RunEditor,
) {
    ui.indent(format!("editor-{id}"), |ui| {
        ui.weak("Changes apply to jobs that haven't started yet.");
        let mut e = ed.clone();
        let mut changed = false;
        let (_, v) = area(
            ui,
            &format!("{name} lyrics"),
            &format!("{id}-lyrics"),
            &e.params.lyrics,
            4,
        );
        if let Some(v) = v {
            e.params.lyrics = v;
            changed = true;
        }
        let (_, v) = area(
            ui,
            &format!("{name} style"),
            &format!("{id}-style"),
            &e.params.style,
            2,
        );
        if let Some(v) = v {
            e.params.style = v;
            changed = true;
        }
        let (_, v) = area(
            ui,
            &format!("{name} ABC"),
            &format!("{id}-abc"),
            &e.abc_text,
            4,
        );
        if let Some(v) = v {
            e.abc_text = v;
            changed = true;
        }
        egui::CollapsingHeader::new(format!("{name} sampling"))
            .id_salt(format!("{id}-adv"))
            .show(ui, |ui| {
                changed |= params_editor(ui, &format!("{id}"), &mut e.params);
            });
        if ib(ui, ph::CHECK, &format!("Apply params to {name}")).clicked() {
            act(out, UiAction::ApplyRunParams(id));
        }
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!e.until_stopped, |ui| {
                let (_, v) = field(
                    ui,
                    &format!("{name} count"),
                    &format!("{id}-count"),
                    &e.count,
                    60.0,
                );
                if let Some(v) = v {
                    e.count = v;
                    changed = true;
                }
            });
            changed |= ui
                .checkbox(&mut e.until_stopped, format!("{name} until stopped"))
                .changed();
            if ib(ui, ph::CHECK, &format!("Apply count to {name}")).clicked() {
                act(out, UiAction::ApplyRunCount(id));
            }
        });
        ui.horizontal(|ui| {
            let (_, v) = field(
                ui,
                &format!("{name} next seed"),
                &format!("{id}-next"),
                &e.next_seed,
                110.0,
            );
            if let Some(v) = v {
                e.next_seed = v;
                changed = true;
            }
            if ib(ui, ph::CHECK, &format!("Apply next seed to {name}")).clicked() {
                act(out, UiAction::ApplyRunNextSeed(id));
            }
        });
        if let Some(c) = s.editor_collision(id) {
            warn_text(
                ui,
                format!(
                    "Seeds {:?} already exist for {name}",
                    c.seeds.iter().take(5).collect::<Vec<_>>()
                ),
            );
            if let Some(n) = c.continue_from
                && ui
                    .add(icon_btn(
                        ph::FAST_FORWARD,
                        format!("Use next free seed {n} for {name}"),
                    ))
                    .clicked()
            {
                let mut e2 = e.clone();
                e2.next_seed = n.to_string();
                act(out, UiAction::SetRunEditor(id, e2));
            }
        }
        if changed {
            act(out, UiAction::SetRunEditor(id, e));
        }
    });
}

// ---------------------------------------------------------------------------------------
// Library

fn library(ui: &mut Ui, s: &AppState, out: &mut O) {
    filters(ui, s, out);
    ui.separator();
    let songs = s.filtered_songs();
    egui::Panel::right("song-detail")
        .resizable(true)
        .default_size(420.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("detail")
                .auto_shrink([false; 2])
                .show(ui, |ui| match s.current_song() {
                    Some(song) => {
                        song_detail(ui, s, out, song);
                        if s.library_lyrics {
                            lyrics_popup(ui.ctx(), s, out, song, LyricsPopup::Library);
                        }
                    }
                    None => {
                        ui.weak("Select a song.");
                    }
                });
        });
    ui.horizontal(|ui| {
        ui.label(format!("{} songs", songs.len()));
        if ib(ui, ph::CHECK_SQUARE, "Select all").clicked() {
            act(out, UiAction::SelectAllFiltered);
        }
        if !s.selected.is_empty() {
            ui.label(format!("{} selected", s.selected.len()));
            if ib(ui, ph::SQUARE, "Clear selection").clicked() {
                act(out, UiAction::ClearSelection);
            }
        }
    });
    songs_table(ui, s, out, &songs);
}

/// The song list: click a row to select it, double-click to play it, right-click for more,
/// and click a header to sort by its column (again to reverse).
fn songs_table(ui: &mut Ui, s: &AppState, out: &mut O, songs: &[&Song]) {
    use egui_extras::{Column, TableBuilder};
    // labels would take the clicks meant for the row
    ui.style_mut().interaction.selectable_labels = false;
    let row_h = ui.spacing().interact_size.y + 4.0;
    TableBuilder::new(ui)
        .id_salt("songs")
        .striped(true)
        .sense(Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto())
        .column(Column::auto().at_least(160.0).resizable(true))
        .column(Column::auto().at_least(100.0).resizable(true))
        .column(Column::auto().at_least(50.0).resizable(true))
        .column(Column::auto().at_least(50.0).resizable(true))
        .column(Column::auto().at_least(80.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::remainder().at_least(80.0))
        .header(row_h, |mut header| {
            header.col(|_| {});
            for &col in SongSort::ALL {
                if sort_header(&mut header, &s.song_sort, col) {
                    act(out, UiAction::SortSongs(col));
                }
            }
        })
        .body(|body| {
            body.rows(row_h, songs.len(), |mut row| {
                let song = songs[row.index()];
                song_row(&mut row, s, out, song);
            });
        });
}

fn song_row(tr: &mut egui_extras::TableRow<'_, '_>, s: &AppState, out: &mut O, song: &Song) {
    tr.set_selected(s.current.as_ref() == Some(&song.id));
    tr.col(|ui| {
        let mut sel = s.selected.contains(&song.id);
        if ui
            .checkbox(&mut sel, "")
            .on_hover_text("Add to selection")
            .changed()
        {
            act(out, UiAction::ToggleSelect(song.id.clone()));
        }
    });
    let mut flags = Vec::new();
    if song.has_abc() == Some(false) {
        flags.push("no ABC");
    }
    if song.wav.is_none() {
        flags.push("no master");
    }
    if song.wav_ok == Some(false) {
        flags.push("master hash mismatch");
    }
    let opt = |x: Option<u32>| x.map(|x| x.to_string()).unwrap_or_else(|| "?".into());
    tr.col(|ui| {
        ui.label(song.title());
        if !flags.is_empty() {
            ui.label(RichText::new(ph::WARNING).color(ui.visuals().warn_fg_color))
                .on_hover_text(flags.join(", "));
        }
    });
    tr.col(|ui| {
        ui.label(song.run_name().unwrap_or_default());
    });
    tr.col(|ui| {
        ui.label(opt(song.seed()));
    });
    tr.col(|ui| {
        ui.label(opt(song.revision()));
    });
    tr.col(|ui| match song.rating() {
        Some(r) => {
            ui.label(r.as_str());
        }
        None => {
            ui.weak("unreviewed");
        }
    });
    tr.col(|ui| {
        let created = song.created_at();
        if created.is_empty() {
            ui.weak("unknown");
        } else {
            ui.label(short_time(&created)).on_hover_text(created);
        }
    });
    tr.col(|ui| {
        if !song.meta.tags.is_empty() {
            ui.label(format!("#{}", song.meta.tags.join(" #")));
        }
    });
    let r = tr.response();
    if r.clicked() {
        act(out, UiAction::SelectSong(song.id.clone()));
    }
    if r.double_clicked() {
        act(out, UiAction::Play(song.id.clone()));
    }
    r.context_menu(|ui| {
        if ib(ui, ph::PLAY, "Play").clicked() {
            act(out, UiAction::Play(song.id.clone()));
            ui.close();
        }
        if ib(ui, ph::FAST_FORWARD, "Continue run").clicked() {
            act(out, UiAction::ContinueRun(song.id.clone()));
            ui.close();
        }
        if ib(ui, ph::ARROWS_CLOCKWISE, "Regenerate").clicked() {
            act(out, UiAction::Regenerate(song.id.clone()));
            ui.close();
        }
    });
}

fn filters(ui: &mut Ui, s: &AppState, out: &mut O) {
    let mut f: LibraryFilter = s.filter.clone();
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("folder-filter")
            .selected_text(match f.folder {
                FolderFilter::All => "All folders",
                FolderFilter::Unreviewed => "Unreviewed",
                FolderFilter::Rated(Rating::Good) => "Good",
                FolderFilter::Rated(Rating::Neutral) => "Neutral",
                FolderFilter::Rated(Rating::Bad) => "Bad",
            })
            .show_ui(ui, |ui| {
                for (v, l) in [
                    (FolderFilter::All, "All folders"),
                    (FolderFilter::Unreviewed, "Unreviewed"),
                    (FolderFilter::Rated(Rating::Good), "Good"),
                    (FolderFilter::Rated(Rating::Neutral), "Neutral"),
                    (FolderFilter::Rated(Rating::Bad), "Bad"),
                ] {
                    changed |= ui.selectable_value(&mut f.folder, v, l).changed();
                }
            });
        for (label, salt, value, w) in [
            ("Search", "f-text", &mut f.text, 140.0),
            ("Tag", "f-tag", &mut f.tag, 80.0),
            ("Run name", "f-run", &mut f.run_name, 100.0),
            ("Seed from", "f-smin", &mut f.seed_min, 70.0),
            ("Seed to", "f-smax", &mut f.seed_max, 70.0),
            ("Revision", "f-rev", &mut f.revision, 40.0),
        ] {
            let (_, v) = field(ui, label, salt, value, w);
            if let Some(v) = v {
                *value = v;
                changed = true;
            }
        }
        changed |= ui.checkbox(&mut f.only_no_abc, "only no-ABC").changed();
        if ib(ui, ph::ARROW_U_UP_LEFT, "Undo (Ctrl+Z)").clicked() {
            act(out, UiAction::Undo);
        }
    });
    if changed {
        act(out, UiAction::SetFilter(f));
    }
}

/// `keypoints` marks the song's keypoints and pre-roll starts on the waveform.
fn player(ui: &mut Ui, s: &AppState, out: &mut O, song: &Song, keypoints: Option<&Keypoints>) {
    let p = &s.player;
    let this = p.song.as_ref() == Some(&song.id);
    let dur = ui
        .horizontal(|ui| {
            let playing = this && p.state == Some(PlayState::Playing);
            if if playing {
                ib(ui, ph::PAUSE, "Pause")
            } else {
                ib(ui, ph::PLAY, "Play")
            }
            .clicked()
            {
                if this && matches!(p.state, Some(PlayState::Playing | PlayState::Paused)) {
                    act(out, UiAction::TogglePlay);
                } else {
                    act(out, UiAction::Play(song.id.clone()));
                }
            }
            if ui
                .add_enabled(this, icon_btn(ph::STOP, "Stop playback"))
                .clicked()
            {
                act(out, UiAction::StopPlayback);
            }
            if this && p.state == Some(PlayState::Loading) {
                ui.spinner();
            }
            let (pos, dur) = if this {
                (p.position, p.duration)
            } else {
                (
                    Duration::ZERO,
                    Duration::from_millis(song.meta.duration_ms.unwrap_or(0)),
                )
            };
            ui.monospace(format!("{} / {}", fmt_duration(pos), fmt_duration(dur)));
            dur
        })
        .inner;
    waveform(ui, s, out, &song.id, keypoints.map(|k| (k, dur)));
}

/// Waveform strip with a click-to-seek bar. `marks` draws keypoints (blue, flagged and
/// standing proud of the strip; the last one played thicker) and where their pre-roll starts
/// playback (green) over a song of the given length. Hovering a keypoint names it; clicking
/// it plays it as *Review next keypoint* would.
fn waveform(
    ui: &mut Ui,
    s: &AppState,
    out: &mut O,
    id: &SongId,
    marks: Option<(&Keypoints, Duration)>,
) {
    // Room above the strip for keypoint flags, and below for their lines to overhang.
    const FLAG: f32 = 8.0;
    const OVERHANG: f32 = 4.0;
    let p = &s.player;
    let this = p.song.as_ref() == Some(id);
    let width = ui.available_width().max(100.0);
    let (outer, resp) = ui.allocate_exact_size(
        egui::vec2(width, 56.0 + FLAG + OVERHANG),
        Sense::click_and_drag(),
    );
    let rect = egui::Rect::from_min_max(
        outer.min + egui::vec2(0.0, FLAG),
        outer.max - egui::vec2(0.0, OVERHANG),
    );
    let dur = p.duration.as_secs_f32();
    let frac = if this && dur > 0.0 {
        (p.position.as_secs_f32() / dur).clamp(0.0, 1.0)
    } else {
        0.0
    };
    resp.widget_info(|| egui::WidgetInfo::slider(this, frac as f64, "Seek"));
    let painter = ui.painter_at(outer);
    let bg = ui.visuals().extreme_bg_color;
    painter.rect_filled(rect, 4.0, bg);
    let peaks = p
        .peaks
        .as_ref()
        .filter(|(p, _)| p == id)
        .map(|(_, v)| v.as_slice());
    let mid = rect.center().y;
    let color = ui.visuals().widgets.inactive.fg_stroke.color;
    let played = ui.visuals().selection.bg_fill;
    if let Some(peaks) = peaks {
        let n = peaks.len().max(1) as f32;
        for (i, v) in peaks.iter().enumerate() {
            let x = rect.left() + (i as f32 + 0.5) * rect.width() / n;
            let h = v * rect.height() * 0.48;
            let c = if (i as f32 / n) < frac { played } else { color };
            painter.line_segment([egui::pos2(x, mid - h), egui::pos2(x, mid + h)], (1.0, c));
        }
    } else {
        painter.line_segment(
            [egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)],
            (1.0, color),
        );
    }
    let x_of = |at: Duration, len: Duration| {
        let f = (at.as_secs_f32() / len.as_secs_f32()).clamp(0.0, 1.0);
        rect.left() + f * rect.width()
    };
    // A coloured line over a wider one in the strip's background, so it reads against
    // played and unplayed peaks alike.
    let line = |x: f32, top: f32, bottom: f32, w: f32, c: Color32| {
        let seg = [egui::pos2(x, top), egui::pos2(x, bottom)];
        painter.line_segment(seg, (w + 2.0, bg));
        painter.line_segment(seg, (w, c));
    };
    // The keypoint under the pointer, if within a few pixels of one.
    let mut hovered = None;
    if let Some((k, len)) = marks.filter(|(_, len)| !len.is_zero()) {
        for i in 0..k.points.len() {
            if let Some(at) = k.start(i) {
                line(
                    x_of(at, len),
                    rect.top(),
                    rect.bottom(),
                    1.5,
                    Color32::from_rgb(80, 180, 80),
                );
            }
        }
        let blue = Color32::from_rgb(90, 150, 230);
        for (i, kp) in k.points.iter().enumerate() {
            let x = x_of(Duration::from_millis(kp.at_ms), len);
            let current = s.review.keypoint == Some(i);
            let (w, half) = if current { (3.5, 6.0) } else { (2.0, 4.5) };
            line(x, outer.top() + 1.0, outer.bottom(), w, blue);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(x - half, outer.top()),
                    egui::pos2(x + half, outer.top()),
                    egui::pos2(x, outer.top() + FLAG + 1.0),
                ],
                blue,
                egui::Stroke::NONE,
            ));
            if let Some(pos) = resp.hover_pos() {
                let d = (pos.x - x).abs();
                if d <= 5.0 && hovered.is_none_or(|(_, best)| d < best) {
                    hovered = Some((i, d));
                }
            }
        }
    }
    let x = rect.left() + frac * rect.width();
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        (2.0, ui.visuals().strong_text_color()),
    );
    let hovered = hovered.map(|(i, _)| i);
    if let (Some(i), Some((k, _))) = (hovered, marks) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            act(out, UiAction::PlayKeypoint(i));
        }
        let kp = &k.points[i];
        let name = if kp.name.is_empty() {
            String::new()
        } else {
            format!(" · {}", kp.name)
        };
        resp.clone().on_hover_text_at_pointer(format!(
            "Keypoint {}{name} · {}\nClick to play from its pre-roll",
            i + 1,
            fmt_keypoint(kp.at_ms)
        ));
    } else if (resp.clicked() || resp.dragged())
        && this
        && dur > 0.0
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        act(out, UiAction::Seek(Duration::from_secs_f32(f * dur)));
    }
}

fn song_detail(ui: &mut Ui, s: &AppState, out: &mut O, song: &Song) {
    ui.heading(song.title());
    ui.weak(format!("{} · {}", song.stem, song.location.label()));
    if song.wav.is_none() {
        warn_text(ui, "No WAV master: WAV export decodes the MP3.");
    }
    if song.wav_ok == Some(false) {
        warn_text(ui, "WAV master does not match the recipe's wav_sha256.");
    }
    player(ui, s, out, song, None);
    ui.separator();
    metadata_editor(ui, s, out, false);
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        for (r, l) in [
            (Rating::Good, "Rate good"),
            (Rating::Neutral, "Rate neutral"),
            (Rating::Bad, "Rate bad"),
        ] {
            if ib(ui, rating_icon(r), l).clicked() {
                act(out, UiAction::Rate(r));
            }
        }
        if song.location != Location::Unreviewed
            && ib(ui, ph::ARROW_COUNTER_CLOCKWISE, "Unreview").clicked()
        {
            act(out, UiAction::Unreview);
        }
        if ib(ui, ph::TRASH, "Delete (to trash)").clicked() {
            act(out, UiAction::Delete);
        }
    });
    ui.horizontal_wrapped(|ui| {
        let has_recipe = song.meta.recipe.is_some();
        if ui
            .add_enabled(has_recipe, icon_btn(ph::FAST_FORWARD, "Continue run"))
            .clicked()
        {
            act(out, UiAction::ContinueRun(song.id.clone()));
        }
        if ui
            .add_enabled(has_recipe, icon_btn(ph::ARROWS_CLOCKWISE, "Regenerate"))
            .clicked()
        {
            act(out, UiAction::Regenerate(song.id.clone()));
        }
        if ui
            .add_enabled(
                song.lyrics().is_some(),
                icon_btn(ph::TEXT_ALIGN_LEFT, "View lyrics"),
            )
            .on_disabled_hover_text("No lyrics in this song's recipe")
            .clicked()
        {
            act(out, UiAction::SetLibraryLyrics(true));
        }
    });
    ui.horizontal_wrapped(|ui| {
        let n = if s.selected.is_empty() {
            1
        } else {
            s.selected.len()
        };
        for (fmt, label) in [
            (ExportFormat::Mp3, "MP3"),
            (ExportFormat::Wav, "WAV"),
            (ExportFormat::Flac, "FLAC"),
        ] {
            if ib(ui, ph::EXPORT, &format!("Export {n} as {label}…")).clicked() {
                out.push(Out::Effect(Effect::PickExportDir {
                    format: fmt,
                    strip_metadata: s.export_strip,
                }));
            }
        }
        let mut strip = s.export_strip;
        if ui.checkbox(&mut strip, "strip metadata").changed() {
            act(out, UiAction::SetExportStrip(strip));
        }
    });
    if let Some(r) = &song.meta.recipe {
        egui::CollapsingHeader::new("Recipe (read-only)")
            .id_salt("recipe")
            .show(ui, |ui| {
                let mut text = serde_json::to_string_pretty(r).unwrap_or_default();
                ui.add(
                    TextEdit::multiline(&mut text)
                        .id_salt("recipe-json")
                        .code_editor()
                        .interactive(false)
                        .desired_width(f32::INFINITY),
                );
            });
    }
}

fn metadata_editor(ui: &mut Ui, s: &AppState, out: &mut O, review: bool) {
    let m = &s.meta;
    ui.horizontal(|ui| {
        let (r, v) = field(ui, "Title", "meta-title", &m.title, 220.0);
        if let Some(v) = v {
            act(
                out,
                UiAction::SetMeta(incremusic_gui_core::MetaEditor {
                    title: v,
                    ..m.clone()
                }),
            );
        }
        if r.lost_focus() {
            act(out, UiAction::CommitTitle);
        }
    });
    ui.horizontal(|ui| {
        let (r, v) = field(ui, "Rename to", "meta-rename", &m.rename, 160.0);
        if let Some(v) = v {
            act(
                out,
                UiAction::SetMeta(incremusic_gui_core::MetaEditor {
                    rename: v,
                    ..m.clone()
                }),
            );
        }
        if review && s.focus == Some(Focus::Rename) {
            r.request_focus();
            act(out, UiAction::FocusHandled);
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui
            .add(icon_btn(ph::PENCIL_SIMPLE, "Rename"))
            .on_hover_text("Renames folder and files to <name>-<seed>")
            .clicked()
            || enter
        {
            act(out, UiAction::CommitRename);
        }
    });
    if let Some(song) = s.current_song() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Tags:");
            for t in &song.meta.tags {
                if ui
                    .add(icon_btn(ph::X, t.clone()).small())
                    .on_hover_text(format!("Remove tag {t}"))
                    .clicked()
                {
                    act(out, UiAction::RemoveTag(t.clone()));
                }
            }
        });
    }
    ui.horizontal(|ui| {
        let (r, v) = field(ui, "Add tag", "meta-tag", &m.new_tag, 140.0);
        if let Some(v) = v {
            act(
                out,
                UiAction::SetMeta(incremusic_gui_core::MetaEditor {
                    new_tag: v,
                    ..m.clone()
                }),
            );
        }
        if review && s.focus == Some(Focus::Tag) {
            r.request_focus();
            act(out, UiAction::FocusHandled);
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            act(out, UiAction::AddTag(m.new_tag.clone()));
        }
        // autocomplete from tags already in the library
        let typed = m.new_tag.trim().to_lowercase();
        if !typed.is_empty() {
            for t in s
                .all_tags()
                .into_iter()
                .filter(|t| t.to_lowercase().starts_with(&typed) && t.to_lowercase() != typed)
                .take(5)
            {
                if ib_small(ui, ph::TAG, &t).clicked() {
                    act(out, UiAction::AddTag(t));
                }
            }
        }
    });
    let (r, v) = area(ui, "Notes", "meta-notes", &m.notes, 3);
    if let Some(v) = v {
        act(
            out,
            UiAction::SetMeta(incremusic_gui_core::MetaEditor {
                notes: v,
                ..m.clone()
            }),
        );
    }
    if r.lost_focus() {
        act(out, UiAction::CommitNotes);
    }
}

// ---------------------------------------------------------------------------------------
// Projects (§5.2.1)

fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

/// `2026-09-23T21:40:02Z` → `2026-09-23 21:40`.
fn short_time(t: &str) -> String {
    t.get(..16).unwrap_or(t).replace('T', " ")
}

fn projects(ui: &mut Ui, s: &AppState, out: &mut O) {
    ui.horizontal(|ui| {
        let (_, v) = field(
            ui,
            "Search projects",
            "p-search",
            &s.projects_view.search,
            180.0,
        );
        if let Some(v) = v {
            act(out, UiAction::SetProjectSearch(v));
        }
        if ib(ui, ph::ARROWS_CLOCKWISE, "Refresh projects").clicked() {
            act(out, UiAction::RefreshProjects);
        }
    });
    ui.separator();
    let list = s.filtered_projects();
    egui::Panel::right("project-detail")
        .resizable(true)
        .default_size(440.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("project-detail")
                .show(ui, |ui| match s.current_project() {
                    Some(p) => project_detail(ui, s, out, p),
                    None => {
                        ui.weak("Select a project.");
                    }
                });
        });
    ui.label(format!("{} projects", list.len()));
    if s.projects.is_empty() {
        ui.weak("No projects yet. A project is made when a run starts, or when you load an ABC file or transcribe audio in Generate.");
    }
    projects_table(ui, s, out, &list);
}

/// A column header that sorts by its column across the whole cell, showing the direction on
/// the column sorted by. True when clicked.
fn sort_header<K: SortColumn>(
    header: &mut egui_extras::TableRow<'_, '_>,
    sort: &Sorting<K>,
    col: K,
) -> bool {
    let active = sort.by == col;
    let mut clicked = false;
    header.col(|ui| {
        if active {
            let arrow = if sort.ascending() {
                ph::CARET_UP
            } else {
                ph::CARET_DOWN
            };
            ui.strong(format!("{} {arrow}", col.label()));
        } else {
            ui.strong(col.label());
        }
        // over the whole cell, not just the label; `Response::interact` on the cell's
        // response would re-register it with last frame's rect
        clicked = ui
            .interact(ui.max_rect(), ui.id().with("sort"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(if active {
                "Reverse the order".to_string()
            } else {
                format!("Sort by {}", col.label().to_lowercase())
            })
            .clicked();
    });
    clicked
}

/// The project list: click a row to select it, double-click to load it into Generate, and
/// click a header to sort by its column (again to reverse).
fn projects_table(
    ui: &mut Ui,
    s: &AppState,
    out: &mut O,
    list: &[&incremusic_core::project::ProjectSummary],
) {
    use egui_extras::{Column, TableBuilder};
    let pv = &s.projects_view;
    // labels would take the clicks meant for the row
    ui.style_mut().interaction.selectable_labels = false;
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    TableBuilder::new(ui)
        .id_salt("projects")
        .striped(true)
        .sense(Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(140.0).resizable(true))
        .column(Column::auto().at_least(60.0).resizable(true))
        .column(Column::auto().at_least(140.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::remainder().at_least(120.0))
        .header(row_h + 4.0, |mut header| {
            for &col in ProjectSort::ALL {
                if sort_header(&mut header, &pv.sort, col) {
                    act(out, UiAction::SortProjects(col));
                }
            }
        })
        .body(|body| {
            body.rows(row_h, list.len(), |mut row| {
                let p = list[row.index()];
                row.set_selected(pv.current.as_deref() == Some(p.name.as_str()));
                row.col(|ui| {
                    ui.label(&p.name);
                    if p.error.is_some() {
                        ui.label(RichText::new(ph::WARNING).color(ui.visuals().warn_fg_color))
                            .on_hover_text("Unreadable project.ron");
                    }
                });
                row.col(|ui| {
                    ui.label(s.project_songs(&p.name).to_string());
                });
                row.col(|ui| {
                    match &p.reference {
                        Some(r) => ui.label(&r.original_name),
                        None => ui.weak("none"),
                    };
                });
                row.col(|ui| {
                    ui.label(short_time(&p.created_at));
                });
                row.col(|ui| {
                    ui.label(short_time(&p.modified_at));
                });
                let r = row.response();
                if r.clicked() {
                    act(out, UiAction::SelectProject(p.name.clone()));
                }
                if r.double_clicked() {
                    act(out, UiAction::OpenProject(p.name.clone()));
                }
            });
        });
}

fn history(ui: &mut Ui, s: &AppState, out: &mut O) {
    let h = &s.history;
    ui.horizontal(|ui| {
        let (_, v) = field(ui, "Search runs", "h-search", &h.search, 180.0);
        if let Some(v) = v {
            act(out, UiAction::SetHistorySearch(v));
        }
        if ib(ui, ph::ARROWS_CLOCKWISE, "Refresh history").clicked() {
            act(out, UiAction::RefreshHistory);
        }
        if let Some(root) = s
            .init
            .as_ref()
            .map(|i| i.library_root.join(incremusic_core::library::RUNS))
            && ib(ui, ph::FOLDER_OPEN, "Open runs folder").clicked()
        {
            out.push(Out::Effect(Effect::OpenFolder(root)));
        }
    });
    ui.separator();
    let list = s.history_rows();
    egui::Panel::right("history-detail")
        .resizable(true)
        .default_size(520.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("history-detail")
                .show(ui, |ui| match (&h.selected, &h.record) {
                    (None, _) => {
                        ui.weak("Select a run.");
                    }
                    (Some(_), None) => {
                        ui.spinner();
                    }
                    (Some(_), Some(Err(e))) => {
                        warn_text(ui, format!("Couldn't read the record: {e}"))
                    }
                    (Some(_), Some(Ok(r))) => history_detail(ui, s, out, r),
                });
        });
    ui.label(format!("{} runs", list.len()));
    if h.runs.is_empty() {
        ui.weak("No runs recorded yet. Every run started from Generate, Regenerate or Resume is kept here.");
    }
    history_table(ui, s, out, &list);
}

/// The recorded runs: click a row to show its record, and click a header to sort by its
/// column (again to reverse).
fn history_table(ui: &mut Ui, s: &AppState, out: &mut O, list: &[&RunSummary]) {
    use egui_extras::{Column, TableBuilder};
    let h = &s.history;
    // labels would take the clicks meant for the row
    ui.style_mut().interaction.selectable_labels = false;
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    TableBuilder::new(ui)
        .id_salt("history")
        .striped(true)
        .sense(Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(140.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::auto().at_least(160.0).resizable(true))
        .column(Column::auto().at_least(80.0).resizable(true))
        .column(Column::remainder().at_least(100.0))
        .header(row_h + 4.0, |mut header| {
            for &col in HistorySort::ALL {
                if sort_header(&mut header, &h.sort, col) {
                    act(out, UiAction::SortHistory(col));
                }
            }
        })
        .body(|body| {
            body.rows(row_h, list.len(), |mut row| {
                let r = list[row.index()];
                row.set_selected(h.selected == Some(r.id));
                row.col(|ui| {
                    ui.label(&r.name);
                    if r.regenerate_of.is_some() {
                        ui.weak("regenerate");
                    }
                });
                row.col(|ui| {
                    ui.label(fmt_time(&r.created_at));
                });
                row.col(|ui| {
                    let (status, done, failed) = s.run_status(r);
                    ui.label(format!("{} · {done} done, {failed} failed", status.label()));
                });
                row.col(|ui| {
                    let count = r.count.map(|c| c.to_string()).unwrap_or_else(|| "∞".into());
                    ui.label(format!("{}+{count}", r.start_seed));
                });
                row.col(|ui| {
                    ui.label(&r.model_id);
                });
                if row.response().clicked() {
                    act(out, UiAction::SelectHistoryRun(r.id));
                }
            });
        });
}

/// `2026-09-25T10:02:11Z` → `2026-09-25 10:02` (UTC).
fn fmt_time(rfc3339: &str) -> String {
    rfc3339
        .get(..16)
        .map(|t| t.replace('T', " "))
        .unwrap_or_else(|| rfc3339.to_string())
}

fn history_detail(ui: &mut Ui, s: &AppState, out: &mut O, r: &incremusic_core::history::RunRecord) {
    use incremusic_core::history::SeedOutcome;
    ui.heading(r.name.as_str());
    let finished = r
        .finished_at
        .as_deref()
        .map(|t| format!(" {} {}", ph::ARROW_RIGHT, fmt_time(t)))
        .unwrap_or_default();
    ui.weak(format!(
        "{}{finished} UTC · {} · model {}",
        fmt_time(&r.created_at),
        r.status.label(),
        r.model.id
    ));
    let count = r
        .count
        .map(|c| c.to_string())
        .unwrap_or_else(|| "until stopped".into());
    ui.label(format!(
        "Seeds from {} ({count}) · next seed {} · {} done, {} failed",
        r.start_seed,
        r.next_seed,
        r.done(),
        r.failed()
    ));
    if let Some(of) = &r.regenerate_of {
        ui.label(format!("Regenerate of {of}"));
    }
    if let Some(from) = r.resumed_from {
        ui.label(format!("Resumes run {from}"));
    }
    ui.horizontal_wrapped(|ui| {
        if ib(ui, ph::ARROW_SQUARE_IN, "Load into Generate")
            .on_hover_text("Latest params; the seed continues after this run's last one")
            .clicked()
        {
            act(out, UiAction::LoadIntoForm(FormLoad::Record(None)));
        }
        if r.status == RecordStatus::Interrupted
            && let Some(spec) = r.resume_spec().filter(|p| p.count != Some(0))
            && ib(ui, ph::PLAY, "Resume run")
                .on_hover_text(format!(
                    "Queue the seeds it didn't finish, from seed {}",
                    spec.start_seed
                ))
                .clicked()
        {
            act(out, UiAction::ResumeInterrupted(r.id));
        }
        if let Some(root) = s
            .init
            .as_ref()
            .map(|i| i.library_root.join(incremusic_core::library::RUNS))
            && ib(ui, ph::FILE_TEXT, "Reveal record")
                .on_hover_text(format!("{}.ron", r.id))
                .clicked()
        {
            out.push(Out::Effect(Effect::OpenFolder(root)));
        }
    });
    ui.separator();
    ui.strong(format!("Revisions ({})", r.revisions.len()));
    for v in r.revisions.iter().rev() {
        egui::CollapsingHeader::new(format!(
            "Revision {} · from seed {} · {}",
            v.revision,
            v.first_seed,
            fmt_time(&v.at)
        ))
        .id_salt(("rev", v.revision))
        .show(ui, |ui| {
            if r.revisions.len() > 1
                && ib_small(
                    ui,
                    ph::ARROW_SQUARE_IN,
                    &format!("Load revision {}", v.revision),
                )
                .clicked()
            {
                act(
                    out,
                    UiAction::LoadIntoForm(FormLoad::Record(Some(v.revision))),
                );
            }
            let p = &v.params;
            egui::Grid::new(("rev-grid", v.revision))
                .num_columns(2)
                .show(ui, |ui| {
                    ui.weak("Style");
                    ui.label(&p.style);
                    ui.end_row();
                    ui.weak("ABC");
                    ui.label(match &v.abc_source {
                        AbcSource::None => "none (YuE2 composes)".to_string(),
                        AbcSource::File { file_name, .. } => format!("file {file_name}"),
                        AbcSource::Transcribed(a) => format!("transcribed from {}", a.file_name),
                        AbcSource::Manual => "typed".to_string(),
                    });
                    ui.end_row();
                    ui.weak("Guidance / steps");
                    ui.label(format!("{} / {}", p.guidance_scale, p.num_inference_steps));
                    ui.end_row();
                });
            if !p.lyrics.trim().is_empty() {
                egui::CollapsingHeader::new("Lyrics")
                    .id_salt(("lyrics", v.revision))
                    .show(ui, |ui| {
                        ui.label(&p.lyrics);
                    });
            }
            if let Some(abc) = &p.abc {
                egui::CollapsingHeader::new("ABC text")
                    .id_salt(("abc", v.revision))
                    .show(ui, |ui| {
                        ui.monospace(abc);
                    });
            }
        });
    }
    ui.separator();
    ui.strong(format!("Seeds ({})", r.seeds.len()));
    if r.seeds.is_empty() {
        ui.weak("No seed finished.");
    }
    for e in &r.seeds {
        ui.horizontal(|ui| {
            ui.monospace(format!("{:>10}", e.seed));
            ui.weak(format!("rev {}", e.revision));
            match &e.outcome {
                SeedOutcome::Song(id) => {
                    let id = SongId(id.clone());
                    match s.library.get(&id) {
                        Some(song) => {
                            if ui.link(song.title()).clicked() {
                                act(out, UiAction::SelectTab(Tab::Library));
                                act(out, UiAction::SelectSong(id));
                            }
                        }
                        None => {
                            ui.weak(format!("{} (no longer in the library)", id.0));
                        }
                    }
                }
                SeedOutcome::Failed(why) => {
                    ui.colored_label(ui.visuals().error_fg_color, format!("failed: {why}"));
                }
                SeedOutcome::Cancelled => {
                    ui.weak("cancelled");
                }
            }
        });
    }
}

fn project_detail(
    ui: &mut Ui,
    s: &AppState,
    out: &mut O,
    p: &incremusic_core::project::ProjectSummary,
) {
    let name = &p.name;
    ui.heading(name);
    ui.weak(p.dir.display().to_string());
    if let Some(e) = &p.error {
        warn_text(ui, format!("project.ron: {e}"));
    }
    ui.horizontal_wrapped(|ui| {
        if ib(ui, ph::MAGIC_WAND, "Load into Generate")
            .on_hover_text("Fill the Generate form with this project's ABC, lyrics and style")
            .clicked()
        {
            act(out, UiAction::OpenProject(name.clone()));
        }
        let songs = s.project_songs(name);
        if ui
            .add_enabled(
                songs > 0,
                icon_btn(ph::BOOKS, format!("Show {songs} songs")),
            )
            .clicked()
        {
            act(out, UiAction::ShowProjectSongs(name.clone()));
        }
        if ib(ui, ph::FOLDER_OPEN, "Open folder").clicked() {
            out.push(Out::Effect(Effect::OpenFolder(p.dir.clone())));
        }
    });
    ui.separator();
    egui::Grid::new("project-facts")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.label("Created");
            ui.label(short_time(&p.created_at));
            ui.end_row();
            ui.label("Modified");
            ui.label(short_time(&p.modified_at));
            ui.end_row();
            ui.label("Runs");
            ui.label(p.runs.to_string());
            ui.end_row();
            ui.label("Reference");
            match &p.reference {
                Some(r) => {
                    let size = p.reference_bytes.map(bytes).unwrap_or("missing".into());
                    ui.label(format!("{} ({}, {size})", r.original_name, r.format));
                }
                None => {
                    ui.weak("none");
                }
            }
            ui.end_row();
            ui.label("Transcription");
            match &p.transcription {
                Some(t) => ui.label(format!(
                    "{} · {} · {:.1} s",
                    t.model,
                    short_time(&t.created_at),
                    t.wall_ms as f64 / 1000.0
                )),
                None => ui.weak("none"),
            };
            ui.end_row();
            ui.label("ABC");
            match &p.abc {
                Some(a) => ui.label(match &a.source {
                    incremusic_core::project::AbcFileSource::Transcribed => {
                        format!("{} (transcribed)", a.file)
                    }
                    incremusic_core::project::AbcFileSource::File(f) => {
                        format!("{} (from {f})", a.file)
                    }
                    incremusic_core::project::AbcFileSource::Manual => {
                        format!("{} (typed)", a.file)
                    }
                }),
                None => ui.weak("none"),
            };
            ui.end_row();
        });
    ui.separator();
    let busy = s.project_busy(name);
    ui.horizontal(|ui| {
        let (r, v) = field(ui, "Rename to", "p-rename", &s.projects_view.rename, 200.0);
        if let Some(v) = v {
            act(out, UiAction::SetProjectRename(v));
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let changed = s.projects_view.rename.trim() != name;
        let b = ui.add_enabled(
            busy.is_none() && changed,
            icon_btn(ph::PENCIL_SIMPLE, "Rename project"),
        );
        let b = match busy {
            Some(why) => b.on_disabled_hover_text(why),
            None => b,
        };
        if (b.clicked() || enter) && busy.is_none() && changed {
            act(out, UiAction::RenameProject);
        }
    });
    let songs = s.project_songs(name);
    if songs > 0 && s.projects_view.rename.trim() != name {
        warn_text(
            ui,
            format!(
                "Its {songs} songs keep the run name “{name}”, so they won't be linked to the renamed project."
            ),
        );
    }
    let d = ui.add_enabled(busy.is_none(), icon_btn(ph::TRASH, "Delete project"));
    let d = match busy {
        Some(why) => d.on_disabled_hover_text(why),
        None => d,
    };
    if d.clicked() {
        act(out, UiAction::DeleteProject(name.clone()));
    }
}

// ---------------------------------------------------------------------------------------
// Inputs: reference audio across projects (§5.2)

fn inputs(ui: &mut Ui, s: &AppState, out: &mut O) {
    let rows = s.reference_rows();
    ui.horizontal(|ui| {
        ui.label(format!("{} reference files", rows.len()));
        if ib(ui, ph::ARROWS_CLOCKWISE, "Refresh inputs").clicked() {
            act(out, UiAction::RefreshProjects);
        }
        ui.separator();
        match s.form.name_result() {
            Ok(n) => ui.weak(format!(
                "“Use” loads the audio into the Generate project “{n}”."
            )),
            Err(_) => ui.weak("Enter a name in Generate to use a reference there."),
        };
    });
    if let Some(p) = &s.transcribe.project {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Transcribing “{p}”…"));
        });
    }
    ui.separator();
    if rows.is_empty() {
        ui.weak("No reference audio yet. Transcribe from audio in Generate, or drop an audio file onto it.");
        return;
    }
    if let Some(row) = rows.iter().find(|r| s.playing_reference(&r.project.name)) {
        reference_player(ui, s, out, row);
        ui.separator();
    }
    references_table(ui, s, out, &rows);
}

/// The reference files, one row per distinct audio; click a header to sort by its column
/// (again to reverse).
fn references_table(
    ui: &mut Ui,
    s: &AppState,
    out: &mut O,
    rows: &[incremusic_gui_core::ReferenceRow<'_>],
) {
    use egui_extras::{Column, TableBuilder};
    let busy = s.transcribe.project.is_some();
    let row_h = ui.spacing().interact_size.y + 4.0;
    TableBuilder::new(ui)
        .id_salt("references")
        .striped(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto())
        .column(Column::auto().at_least(160.0).resizable(true))
        .column(Column::auto().at_least(70.0).resizable(true))
        .column(Column::auto().at_least(70.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::auto().at_least(160.0).resizable(true))
        .column(Column::auto().at_least(120.0).resizable(true))
        .column(Column::remainder().at_least(60.0))
        .header(row_h, |mut header| {
            header.col(|_| {});
            for &col in ReferenceSort::ALL {
                if sort_header(&mut header, &s.reference_sort, col) {
                    act(out, UiAction::SortReferences(col));
                }
            }
            header.col(|_| {});
        })
        .body(|body| {
            body.rows(row_h, rows.len(), |mut row| {
                let r = &rows[row.index()];
                reference_row(&mut row, s, out, r, busy);
            });
        });
}

/// Transport and waveform for the reference audio the player holds.
fn reference_player(
    ui: &mut Ui,
    s: &AppState,
    out: &mut O,
    row: &incremusic_gui_core::ReferenceRow<'_>,
) {
    let p = &s.player;
    let src = &row.project.name;
    let file = &row.reference.original_name;
    ui.horizontal(|ui| {
        let playing = p.state == Some(PlayState::Playing);
        let (icon, what) = if playing {
            (ph::PAUSE, "Pause reference")
        } else {
            (ph::PLAY, "Play reference")
        };
        if ib(ui, icon, what).clicked() {
            act(out, UiAction::PlayReference(src.clone()));
        }
        if ib(ui, ph::STOP, "Stop reference").clicked() {
            act(out, UiAction::StopPlayback);
        }
        if p.state == Some(PlayState::Loading) {
            ui.spinner();
        }
        ui.monospace(format!(
            "{} / {}",
            fmt_duration(p.position),
            fmt_duration(p.duration)
        ));
        ui.label(file);
    });
    waveform(ui, s, out, &reference_play_id(src), None);
}

fn reference_row(
    tr: &mut egui_extras::TableRow<'_, '_>,
    s: &AppState,
    out: &mut O,
    row: &incremusic_gui_core::ReferenceRow<'_>,
    busy: bool,
) {
    let r = row.reference;
    let src = &row.project.name;
    let file = &r.original_name;
    let playing = s.playing_reference(src)
        && matches!(
            s.player.state,
            Some(PlayState::Playing | PlayState::Loading)
        );
    tr.col(|ui| {
        let (icon, what) = if playing {
            (ph::PAUSE, "Pause")
        } else {
            (ph::PLAY, "Play")
        };
        if ib_small(ui, icon, &format!("{what} {file}")).clicked() {
            act(out, UiAction::PlayReference(src.clone()));
        }
        if s.playing_reference(src) {
            if ib_small(ui, ph::STOP, &format!("Stop {file}")).clicked() {
                act(out, UiAction::StopPlayback);
            }
        }
    });
    tr.col(|ui| {
        ui.label(file).on_hover_text(format!(
            "SHA-256 {}\nstored as {}/{}",
            r.sha256,
            row.project.dir.display(),
            r.file
        ));
    });
    tr.col(|ui| {
        let conv = match &r.conversion {
            Some(_) => "converted to WAV",
            None => "uploaded as is",
        };
        ui.label(&r.format)
            .on_hover_text(r.conversion.clone().unwrap_or(conv.into()));
    });
    tr.col(|ui| match row.project.reference_bytes {
        Some(n) => {
            ui.label(bytes(n));
        }
        None => warn_text(ui, "missing"),
    });
    tr.col(|ui| {
        for p in &row.projects {
            if ui.link(&p.name).on_hover_text("Show in Projects").clicked() {
                act(out, UiAction::SelectProject(p.name.clone()));
                act(out, UiAction::SelectTab(Tab::Projects));
            }
        }
    });
    tr.col(|ui| {
        match row.transcribed.and_then(|p| p.transcription.as_ref()) {
            Some(t) => ui.label(format!("{} · {}", t.model, short_time(&t.created_at))),
            None => ui.weak("not transcribed"),
        };
    });
    tr.col(|ui| {
        ui.label(short_time(row.created_at()))
            .on_hover_text("When the audio was added to a project");
    });
    tr.col(|ui| {
        let blocked = s.inputs_blocker();
        let u = ui.add_enabled(
            blocked.is_none() && !busy,
            icon_btn(ph::MUSIC_NOTES, format!("Use {file}")).small(),
        );
        let u = match (blocked, busy) {
            (Some(b), _) => u.on_disabled_hover_text(b),
            (_, true) => u.on_disabled_hover_text("A transcription is already running."),
            _ => u.on_hover_text(if row.transcribed.is_some() {
                "Load into the Generate project; the existing transcription is reused"
            } else {
                "Load into the Generate project and transcribe it"
            }),
        };
        if u.clicked() {
            act(out, UiAction::UseReference(src.clone()));
        }
        let t = ui.add_enabled(
            !busy,
            icon_btn(ph::ARROWS_CLOCKWISE, format!("Re-transcribe {file}")).small(),
        );
        let t = if busy {
            t.on_disabled_hover_text("A transcription is already running.")
        } else {
            t.on_hover_text(format!("Run SheetSage2 again for project “{src}”"))
        };
        if t.clicked() {
            act(out, UiAction::RetranscribeProject(src.clone()));
        }
    });
}

// ---------------------------------------------------------------------------------------
// Review mode (§7.3)

fn review(ui: &mut Ui, s: &AppState, out: &mut O) {
    egui::Panel::right("keypoints")
        .resizable(true)
        .default_size(340.0)
        .show(ui, |ui| keypoints_pane(ui, s, out));
    let total = s.review.queue.len();
    ui.horizontal(|ui| {
        ui.heading("Review");
        ui.label(format!("{} of {total}", (s.review.index + 1).min(total)));
        if ib(ui, ph::SKIP_BACK, "Previous song").clicked() {
            act(out, UiAction::ReviewKey(ReviewKey::Prev));
        }
        if ib(ui, ph::SKIP_FORWARD, "Next song").clicked() {
            act(out, UiAction::ReviewKey(ReviewKey::Next));
        }
        if ib(ui, ph::ARROW_COUNTER_CLOCKWISE, "Restart review").clicked() {
            act(out, UiAction::EnterReview);
        }
        let mut autoplay = !s.review.no_autoplay;
        if ui
            .checkbox(&mut autoplay, "Autoplay new songs")
            .on_hover_text("When every song is reviewed, play the next one to arrive at once")
            .changed()
        {
            act(out, UiAction::SetReviewAutoplay(autoplay));
        }
    });
    ui.weak(format!(
        "Space play/pause · {}/{} seek 5 s (Shift 30 s) · 1/2/3 rate good/neutral/bad and next · N/P next/previous · X/Z next/previous keypoint · T tag · R rename · L lyrics",
        ph::ARROW_SQUARE_LEFT,
        ph::ARROW_SQUARE_RIGHT
    ));
    ui.separator();
    let song = s.review_song().and_then(|id| s.song(id));
    match song {
        Some(song) => {
            ui.heading(song.title());
            ui.weak(format!("{} · {}", song.stem, song.location.label()));
            if let Some(r) = &song.meta.recipe {
                let style = r
                    .request
                    .pointer("/request/options/style")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                ui.label(format!("style: {style}"));
            }
            let keypoints = s.keypoints.project.as_ref().map(|_| &s.keypoints.keypoints);
            player(ui, s, out, song, keypoints);
            ui.horizontal(|ui| {
                for (r, l) in [
                    (Rating::Good, "1 Good"),
                    (Rating::Neutral, "2 Neutral"),
                    (Rating::Bad, "3 Bad"),
                ] {
                    if ib(ui, rating_icon(r), l).clicked() {
                        act(out, UiAction::ReviewKey(ReviewKey::Rate(r)));
                    }
                }
                if ui
                    .add_enabled(
                        song.lyrics().is_some(),
                        icon_btn(ph::TEXT_ALIGN_LEFT, "Lyrics"),
                    )
                    .on_disabled_hover_text("No lyrics in this song's recipe")
                    .clicked()
                {
                    act(out, UiAction::ReviewKey(ReviewKey::Lyrics));
                }
            });
            ui.separator();
            metadata_editor(ui, s, out, true);
            if s.review.lyrics {
                lyrics_popup(ui.ctx(), s, out, song, LyricsPopup::Review);
            }
        }
        None => {
            ui.label(if total == 0 {
                "Nothing to review."
            } else {
                "All caught up — no more unreviewed songs."
            });
        }
    }
}

/// The review song's project's keypoints (§7.3.1), played in list order with *Review next
/// keypoint* (X). Each starts its pre-roll early; the highlighted one was played last.
fn keypoints_pane(ui: &mut Ui, s: &AppState, out: &mut O) {
    ui.heading("Keypoints");
    let ed = &s.keypoints;
    let Some(project) = &ed.project else {
        ui.weak(if s.review_song().is_some() {
            "This song has no project to keep keypoints in."
        } else {
            "No song to review."
        });
        return;
    };
    ui.weak(format!("Project {project} · shared by all its songs"));
    let k = &ed.keypoints;
    let n = k.points.len();
    let at = s.review.keypoint;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(at.is_some(), icon_btn(ph::SKIP_BACK, "Previous keypoint"))
            .on_hover_text("Z")
            .clicked()
        {
            act(out, UiAction::ReviewKey(ReviewKey::PrevKeypoint));
        }
        let next = at.map_or(0, |c| c + 1) < n;
        if ui
            .add_enabled(
                next,
                icon_btn(ph::SKIP_FORWARD, "Review next keypoint").strong(),
            )
            .on_hover_text("X")
            .clicked()
        {
            act(out, UiAction::ReviewKey(ReviewKey::NextKeypoint));
        }
    });
    let loaded = s.review_song().is_some() && s.player.song.as_ref() == s.review_song();
    egui::Panel::bottom("keypoints-footer").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(loaded, icon_btn(ph::MAP_PIN, "Add at playhead"))
                .on_hover_text(format!(
                    "Adds a keypoint at {}",
                    fmt_keypoint(ms(s.player.position))
                ))
                .on_disabled_hover_text("Play the song first")
                .clicked()
            {
                act(out, UiAction::AddKeypoint);
            }
        });
        ui.horizontal(|ui| {
            let l = ui.label("Default pre-roll");
            let mut secs = k.preroll_ms as f64 / 1000.0;
            let r = ui
                .add(seconds_drag(&mut secs))
                .labelled_by(l.id)
                .on_hover_text("How much to play before a keypoint without its own pre-roll");
            if r.changed() {
                let mut k = k.clone();
                k.preroll_ms = (secs * 1000.0).round() as u64;
                act(out, UiAction::SetKeypoints(k));
            }
            commit_when_done(&r, out);
        });
        ui.add_space(4.0);
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("keypoints")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            if n == 0 {
                ui.weak("None yet. Play the song and press Add at playhead where it matters.");
            }
            for i in 0..n {
                keypoint_row(ui, s, out, i);
            }
        });
}

fn keypoint_row(ui: &mut Ui, s: &AppState, out: &mut O, i: usize) {
    let k = &s.keypoints.keypoints;
    let kp = &k.points[i];
    let num = i + 1;
    let current = s.review.keypoint == Some(i);
    let mut frame = egui::Frame::group(ui.style());
    if current {
        frame = frame
            .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.35))
            .stroke(ui.visuals().selection.stroke);
    }
    let edit = |f: &dyn Fn(&mut incremusic_core::project::Keypoint)| {
        let mut k = k.clone();
        f(&mut k.points[i]);
        UiAction::SetKeypoints(k)
    };
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.monospace(format!("{num:>2}"));
            if ui
                .add(
                    icon_btn(ph::PLAY, format!("Play keypoint {num}"))
                        .small()
                        .bare(),
                )
                .clicked()
            {
                act(out, UiAction::PlayKeypoint(i));
            }
            let mut secs = kp.at_ms as f64 / 1000.0;
            let r = ui
                .add(
                    egui::DragValue::new(&mut secs)
                        .range(0.0..=36_000.0)
                        .speed(0.05)
                        .custom_formatter(|v, _| fmt_keypoint((v * 1000.0).round() as u64))
                        .custom_parser(|t| parse_keypoint(t).map(|ms| ms as f64 / 1000.0))
                        .update_while_editing(false),
                )
                .on_hover_text("Drag or type m:ss.s");
            r.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::DragValue,
                    true,
                    format!("Keypoint {num} time"),
                )
            });
            if r.changed() {
                let ms = (secs * 1000.0).round() as u64;
                act(out, edit(&|p| p.at_ms = ms));
            }
            commit_when_done(&r, out);
            let mut name = kp.name.clone();
            let r = ui.add(
                TextEdit::singleline(&mut name)
                    .id_salt(("keypoint-name", i))
                    .hint_text("name (optional)")
                    .desired_width(f32::INFINITY),
            );
            r.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::TextEdit,
                    true,
                    format!("Keypoint {num} name"),
                )
            });
            if r.changed() {
                act(out, edit(&|p| p.name = name.clone()));
            }
            if r.lost_focus() {
                act(out, UiAction::CommitKeypoints);
            }
        });
        ui.horizontal(|ui| {
            let own = kp.preroll_ms.is_some();
            ui.weak("Pre-roll");
            let mut secs = kp.preroll_ms.unwrap_or(k.preroll_ms) as f64 / 1000.0;
            let r = ui.add(seconds_drag(&mut secs)).on_hover_text(if own {
                "This keypoint's own pre-roll"
            } else {
                "The default pre-roll; change it to set this keypoint's own"
            });
            r.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::DragValue,
                    true,
                    format!("Keypoint {num} pre-roll"),
                )
            });
            if r.changed() {
                let ms = (secs * 1000.0).round() as u64;
                act(out, edit(&|p| p.preroll_ms = Some(ms)));
            }
            commit_when_done(&r, out);
            if own {
                if ui
                    .add(
                        icon_btn(
                            ph::ARROW_COUNTER_CLOCKWISE,
                            format!("Default pre-roll for keypoint {num}"),
                        )
                        .small()
                        .bare(),
                    )
                    .clicked()
                {
                    act(out, edit(&|p| p.preroll_ms = None));
                    act(out, UiAction::CommitKeypoints);
                }
            } else {
                ui.weak("(default)");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        icon_btn(ph::TRASH, format!("Remove keypoint {num}"))
                            .small()
                            .bare(),
                    )
                    .clicked()
                {
                    act(out, UiAction::RemoveKeypoint(i));
                }
                let last = i + 1 == k.points.len();
                if ui
                    .add_enabled(
                        !last,
                        icon_btn(ph::ARROW_DOWN, format!("Move keypoint {num} down"))
                            .small()
                            .bare(),
                    )
                    .clicked()
                {
                    act(
                        out,
                        UiAction::MoveKeypoint {
                            index: i,
                            up: false,
                        },
                    );
                }
                if ui
                    .add_enabled(
                        i > 0,
                        icon_btn(ph::ARROW_UP, format!("Move keypoint {num} up"))
                            .small()
                            .bare(),
                    )
                    .clicked()
                {
                    act(out, UiAction::MoveKeypoint { index: i, up: true });
                }
            });
        });
    });
}

/// A 0–60 s field in tenths, for pre-rolls.
fn seconds_drag(secs: &mut f64) -> egui::DragValue<'_> {
    egui::DragValue::new(secs)
        .range(0.0..=60.0)
        .speed(0.05)
        .fixed_decimals(1)
        .suffix(" s")
        .update_while_editing(false)
}

/// Saves keypoint edits once a drag lets go or typing leaves the field.
fn commit_when_done(r: &egui::Response, out: &mut O) {
    if r.drag_stopped() || r.lost_focus() {
        act(out, UiAction::CommitKeypoints);
    }
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// Where a [`lyrics_popup`] is shown, which decides how it closes and which keys it lists.
#[derive(Clone, Copy, PartialEq)]
enum LyricsPopup {
    Review,
    Library,
}

impl LyricsPopup {
    fn close(self) -> UiAction {
        match self {
            LyricsPopup::Review => UiAction::CloseLyrics,
            LyricsPopup::Library => UiAction::SetLibraryLyrics(false),
        }
    }

    fn closing(self, a: &UiAction) -> bool {
        match self {
            LyricsPopup::Review => matches!(
                a,
                UiAction::CloseLyrics | UiAction::ReviewKey(ReviewKey::Lyrics)
            ),
            LyricsPopup::Library => matches!(a, UiAction::SetLibraryLyrics(false)),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            LyricsPopup::Review => {
                "Space play/pause · 1/2/3 rate and next · N/P next/previous · L or Esc close"
            }
            LyricsPopup::Library => "Space play/pause · L or Esc close",
        }
    }
}

/// A song's lyrics in a scrolling popup. In Review the review keys keep working behind it,
/// so it follows the song through Rate/Next/Prev until closed; in the Library it follows
/// the current song.
fn lyrics_popup(ctx: &egui::Context, s: &AppState, out: &mut O, song: &Song, at: LyricsPopup) {
    let resp = egui::Modal::new(egui::Id::new("lyrics")).show(ctx, |ui| {
        ui.set_width(520.0_f32.min(ctx.content_rect().width() - 48.0));
        ui.heading(song.title());
        let this = s.player.song.as_ref() == Some(&song.id);
        if this {
            ui.weak(format!(
                "{} / {}",
                fmt_duration(s.player.position),
                fmt_duration(s.player.duration)
            ));
        }
        ui.separator();
        match song.lyrics() {
            Some(lyrics) => {
                egui::ScrollArea::vertical()
                    .id_salt(("lyrics-scroll", &song.id.0))
                    .max_height(ctx.content_rect().height() * 0.6)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for line in lyrics.lines() {
                            let t = line.trim();
                            // section markers such as `[Verse]` / `[Chorus]`
                            if t.starts_with('[') && t.ends_with(']') {
                                ui.add_space(4.0);
                                ui.strong(t);
                            } else {
                                ui.label(line);
                            }
                        }
                    });
            }
            None => {
                ui.weak("This song's recipe has no lyrics.");
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ib(ui, ph::X, "Close lyrics").clicked() {
                act(out, at.close());
            }
            if let Some(lyrics) = song.lyrics()
                && ib(ui, ph::COPY, "Copy")
                    .on_hover_text("Copy the lyrics to the clipboard")
                    .clicked()
            {
                ui.ctx().copy_text(lyrics.to_string());
                act(
                    out,
                    UiAction::ShowStatus("Copied lyrics to the clipboard".into()),
                );
            }
            ui.weak(at.hint());
        });
    });
    if resp.should_close() && !out.iter().any(|o| matches!(o, Out::Ui(a) if at.closing(a))) {
        act(out, at.close());
    }
}

// ---------------------------------------------------------------------------------------
// Log

fn log(ui: &mut Ui, s: &AppState, out: &mut O) {
    let f = &s.log_filter;
    ui.horizontal(|ui| {
        ui.heading("Log");
        if icon_tab(ui, f.shows_all(), ph::LIST, "All").clicked() {
            act(out, UiAction::ShowAllLogs);
        }
        if icon_tab(ui, f.shows(LogSource::App), ph::APP_WINDOW, "App").clicked() {
            act(out, UiAction::ToggleLogSource(LogSource::App));
        }
        for v in &s.servers {
            let errors = v.log.iter().filter(|l| l.level == LogLevel::Error).count();
            let label = match errors {
                0 => v.info.name.clone(),
                1 => format!("{} (1 error)", v.info.name),
                n => format!("{} ({n} errors)", v.info.name),
            };
            let src = LogSource::Server(v.info.id);
            if icon_tab(ui, f.shows(src), ph::TERMINAL_WINDOW, &label).clicked() {
                act(out, UiAction::ToggleLogSource(src));
            }
        }
        let what = if f.shows_all() {
            "Clear log"
        } else {
            "Clear the shown sources"
        };
        if ib(ui, ph::TRASH, what).clicked() {
            act(out, UiAction::ClearLog);
        }
    });
    let shown: Vec<_> = s
        .servers
        .iter()
        .filter(|v| f.shows(LogSource::Server(v.info.id)))
        .collect();
    for v in &shown {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} on port {}: {}",
                v.info.name,
                v.info.port,
                v.state.label()
            ));
            if let Some(file) = &v.info.log_file {
                ui.separator();
                ui.label(RichText::new(format!("full output: {}", file.display())).weak());
                if ib_small(ui, ph::COPY, "Copy path").clicked() {
                    ui.ctx().copy_text(file.display().to_string());
                }
            }
        });
    }
    // one server on its own needs no source column
    let single = shown.len() == 1 && !f.shows(LogSource::App);
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    let lines = s.log_lines();
    if shown.is_empty() && !f.shows(LogSource::App) {
        ui.label(RichText::new("No sources selected.").weak());
    } else if lines.is_empty() {
        ui.label(RichText::new("Nothing logged yet.").weak());
    }
    egui::ScrollArea::vertical()
        .id_salt(("log", &f.hidden))
        .stick_to_bottom(true)
        .show_rows(ui, row_h, lines.len(), |ui, range| {
            for l in &lines[range] {
                let color = match l.level {
                    LogLevel::Info => ui.visuals().text_color(),
                    LogLevel::Warn => ui.visuals().warn_fg_color,
                    LogLevel::Error => ui.visuals().error_fg_color,
                };
                let t = l
                    .time
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    % 86400;
                // `│` marks what the server printed, `:` the app's own messages
                let mark = if l.output { '│' } else { ':' };
                let text = if single {
                    format!(
                        "{:02}:{:02}:{:02} {mark} {}",
                        t / 3600,
                        (t / 60) % 60,
                        t % 60,
                        l.message
                    )
                } else {
                    format!(
                        "{:02}:{:02}:{:02} {:>10}{mark} {}",
                        t / 3600,
                        (t / 60) % 60,
                        t % 60,
                        l.source,
                        l.message
                    )
                };
                ui.label(RichText::new(text).monospace().color(color));
            }
        });
}

// ---------------------------------------------------------------------------------------
// Dialogs and keyboard

fn dialogs(ctx: &egui::Context, s: &AppState, out: &mut O) {
    let Some(d) = &s.dialog else { return };
    let resp = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
        ui.set_max_width(460.0);
        match d {
            Dialog::NoAbc { dont_ask } => {
                ui.heading("No ABC melody");
                ui.label("No ABC melody: YuE2 will compose its own. Continue?");
                let mut v = *dont_ask;
                if ui.checkbox(&mut v, "Don't ask again for this preset").changed() {
                    act(out, UiAction::SetDontAsk(v));
                }
                buttons(ui, out, "Continue without ABC", "Go back");
            }
            Dialog::StopBusy { server, minutes } => {
                ui.heading(format!("Stop {}?", s.server_name(*server)));
                let about = minutes.map(|m| format!(" (about {m} min)")).unwrap_or_default();
                ui.label(format!("The running job can't be interrupted. The server will exit when it finishes{about}."));
                buttons(ui, out, "Stop when done", "Keep running");
            }
            Dialog::ReplaceReference { audio, .. } => {
                ui.heading("Replace the reference?");
                ui.label(format!(
                    "Project “{}” already has a reference. Its reference and transcription files go to the trash; songs made from them keep their hashes. Use {}?",
                    s.form.name.trim(),
                    audio.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                ));
                buttons(ui, out, "Replace reference", "Keep the old one");
            }
            Dialog::NameProject { audio, name } => {
                ui.heading("Name the project");
                ui.label(format!(
                    "Transcribing {} needs a project to save into (inputs/<name>/).",
                    audio.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                ));
                let (r, v) = field(ui, "Project name", "dialog-name", name, 300.0);
                if let Some(v) = v {
                    act(out, UiAction::SetDialogName(v));
                }
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                let valid = incremusic_core::run::RunName::parse(name);
                if let Err(e) = &valid {
                    warn_text(ui, e.to_string());
                } else if s.project(name.trim()).is_some() {
                    ui.weak("A project with this name exists; the audio is added to it.");
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let ok = ui.add_enabled(valid.is_ok(), icon_btn(ph::CHECK, "Transcribe"));
                    if ok.clicked() || (enter && valid.is_ok()) {
                        act(out, UiAction::ConfirmDialog);
                    }
                    if ib(ui, ph::X, "Cancel").clicked() {
                        act(out, UiAction::CancelDialog);
                    }
                });
            }
            Dialog::ReplaceForm(_) => {
                ui.heading("Replace the Generate form?");
                ui.label(format!(
                    "Generate already has “{}” filled in. Loading this run replaces its name, ABC, lyrics, style and parameters.",
                    s.form.name.trim()
                ));
                buttons(ui, out, "Replace form", "Keep the form");
            }
            Dialog::ClearForm => {
                ui.heading("Clear the Generate form?");
                ui.label(
                    "This resets the name, ABC, lyrics, style, seed, count and parameters to the default preset. Projects, runs and songs are not touched.",
                );
                buttons(ui, out, "Clear form", "Keep the form");
            }
            Dialog::DeleteProject(name) => {
                ui.heading("Delete project?");
                let songs = s.project_songs(name);
                ui.label(format!(
                    "Move the project folder inputs/{name}/ (reference, transcription, ABC, lyrics and style) to the system trash?"
                ));
                if songs > 0 {
                    ui.label(format!(
                        "Its {songs} song(s) stay in the library; their recipes still hold the exact inputs."
                    ));
                }
                buttons(ui, out, "Move project to trash", "Keep");
            }
            Dialog::ShortListen { listened, .. } => {
                ui.heading("Rate bad?");
                ui.label(format!(
                    "This song has only played for {:.0} s of the {} s it should get. Rate it bad anyway?",
                    listened.as_secs_f32(),
                    incremusic_gui_core::MIN_LISTEN.as_secs()
                ));
                buttons(ui, out, "Rate bad", "Keep listening");
            }
            Dialog::ConfirmDelete(ids) => {
                ui.heading("Delete songs?");
                ui.label(format!("Move {} song folder(s) to the system trash?", ids.len()));
                buttons(ui, out, "Move to trash", "Keep");
            }
            Dialog::Exit => {
                ui.heading("Quit");
                ui.label("Stop the servers this app launched? They keep running otherwise.");
                ui.horizontal(|ui| {
                    if ib(ui, ph::POWER, "Leave servers running").clicked() {
                        act(out, UiAction::RequestExit);
                    }
                    if ib(ui, ph::STOP_CIRCLE, "Stop servers and quit").clicked() {
                        act(out, UiAction::ConfirmDialog);
                    }
                    if ib(ui, ph::X, "Don't quit").clicked() {
                        act(out, UiAction::CancelDialog);
                    }
                });
            }
        }
    });
    if resp.should_close()
        && !out
            .iter()
            .any(|o| matches!(o, Out::Ui(UiAction::ConfirmDialog | UiAction::RequestExit)))
    {
        act(out, UiAction::CancelDialog);
    }
}

fn buttons(ui: &mut Ui, out: &mut O, ok: &str, cancel: &str) {
    ui.horizontal(|ui| {
        if ib(ui, ph::CHECK, ok).clicked() {
            act(out, UiAction::ConfirmDialog);
        }
        if ib(ui, ph::X, cancel).clicked() {
            act(out, UiAction::CancelDialog);
        }
    });
}

fn keyboard(ctx: &egui::Context, s: &AppState, out: &mut O) {
    if s.dialog.is_some() || ctx.egui_wants_keyboard_input() {
        return;
    }
    let (undo, keys) = ctx.input(|i| {
        let undo = i.modifiers.command && i.key_pressed(egui::Key::Z);
        let shift = i.modifiers.shift;
        let mut keys = Vec::new();
        if !i.modifiers.command {
            for (k, rk) in [
                (egui::Key::Space, ReviewKey::PlayPause),
                (egui::Key::ArrowLeft, ReviewKey::Back { big: shift }),
                (egui::Key::ArrowRight, ReviewKey::Forward { big: shift }),
                (egui::Key::Num1, ReviewKey::Rate(Rating::Good)),
                (egui::Key::Num2, ReviewKey::Rate(Rating::Neutral)),
                (egui::Key::Num3, ReviewKey::Rate(Rating::Bad)),
                (egui::Key::C, ReviewKey::RateBadChecked),
                (egui::Key::N, ReviewKey::Next),
                (egui::Key::P, ReviewKey::Prev),
                (egui::Key::T, ReviewKey::Tag),
                (egui::Key::R, ReviewKey::Rename),
                (egui::Key::L, ReviewKey::Lyrics),
                (egui::Key::X, ReviewKey::NextKeypoint),
                (egui::Key::Z, ReviewKey::PrevKeypoint),
            ] {
                if i.key_pressed(k) {
                    keys.push(rk);
                }
            }
        }
        (undo, keys)
    });
    if undo {
        act(out, UiAction::Undo);
    }
    match s.tab {
        Tab::Review => {
            for k in keys {
                act(out, UiAction::ReviewKey(k));
            }
        }
        Tab::Library => {
            if keys.contains(&ReviewKey::PlayPause) {
                act(out, UiAction::TogglePlay);
            }
            if keys.contains(&ReviewKey::Lyrics)
                && (s.library_lyrics || s.current_song().is_some_and(|x| x.lyrics().is_some()))
            {
                act(out, UiAction::SetLibraryLyrics(!s.library_lyrics));
            }
        }
        _ => {}
    }
}
