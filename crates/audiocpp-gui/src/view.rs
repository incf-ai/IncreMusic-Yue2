//! Panels (design §8). Pure drawing: reads `AppState`, pushes `Out`s. Every interactive
//! widget has a unique accessible label so the headless tests can find it.

use std::time::{Duration, SystemTime};

use audiocpp_core::library::{Location, Song};
use audiocpp_core::media::{ExportFormat, Rating};
use audiocpp_core::params::Sampling;
use audiocpp_core::playback::PlayState;
use audiocpp_core::run::{AbcSource, RunStatus};
use audiocpp_core::scheduler::JobState;
use audiocpp_core::service::{LogLevel, ServerState};
use audiocpp_gui_core::{
    AbcChoice, AppState, Blocker, Dialog, Focus, FolderFilter, LibraryFilter, ReviewKey, Tab,
    UiAction, fmt_duration, job_label,
};
use egui::{Color32, RichText, Sense, TextEdit, Ui};
use egui_phosphor::regular as ph;

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
}

pub fn icon_btn(icon: &'static str, text: impl Into<String>) -> IconButton {
    IconButton {
        icon,
        text: text.into(),
        small: false,
        strong: false,
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
}

impl egui::Widget for IconButton {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let label = RichText::new(format!("{} {}", self.icon, self.text));
        let mut b = egui::Button::new(if self.strong { label.strong() } else { label });
        if self.small {
            b = b.small();
        }
        let r = ui.add(b);
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
        let active = s
            .runs
            .iter()
            .filter(|r| r.state.status != RunStatus::Done)
            .count();
        for (t, icon, label) in [
            (Tab::Generate, ph::MAGIC_WAND, "Generate".to_string()),
            (Tab::Queue, ph::QUEUE, format!("Queue ({active})")),
            (
                Tab::Library,
                ph::BOOKS,
                format!("Library ({})", s.library.len()),
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
    });
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
        if let Some(l) = s.log.iter().rev().find(|l| l.level == LogLevel::Error) {
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

fn generate(ui: &mut Ui, s: &AppState, out: &mut O) {
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
            let (_, v) = area(ui, "Lyrics", "lyrics", &f.params.lyrics, 8);
            if let Some(v) = v {
                let mut p = f.params.clone();
                p.lyrics = v;
                act(out, UiAction::SetParams(p));
            }
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
            });
        });
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
                if load.clicked() {
                    out.push(Out::Effect(Effect::PickAbcFile));
                }
                let tr = ui.add_enabled(
                    blocked.is_none() && s.transcribe.project.is_none(),
                    icon_btn(ph::WAVEFORM, "Transcribe audio…").strong(),
                );
                let tr = match blocked {
                    Some(b) => tr.on_disabled_hover_text(b),
                    None => tr,
                };
                if tr.clicked() {
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
                            format!(" · sections: {}", m.sections.join(" → "))
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

fn params_editor(ui: &mut Ui, salt: &str, p: &mut audiocpp_core::params::GenerationParams) -> bool {
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
            && ib(ui, ph::BROOM, "Clear finished runs").clicked()
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
                        ui.label(format!(
                            "{name} seed {} · rev {}{server}: {}{retry}",
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
            });
        }
    });
}

fn run_editor(
    ui: &mut Ui,
    s: &AppState,
    out: &mut O,
    id: audiocpp_core::RunId,
    name: &str,
    ed: &audiocpp_gui_core::RunEditor,
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
                .show(ui, |ui| match s.current_song() {
                    Some(song) => song_detail(ui, s, out, song),
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
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    egui::ScrollArea::vertical()
        .id_salt("songs")
        .show_rows(ui, row_h, songs.len(), |ui, range| {
            for song in &songs[range] {
                song_row(ui, s, out, song);
            }
        });
}

fn song_row(ui: &mut Ui, s: &AppState, out: &mut O, song: &Song) {
    ui.horizontal(|ui| {
        let mut sel = s.selected.contains(&song.id);
        if ui
            .checkbox(&mut sel, "")
            .on_hover_text("Add to selection")
            .changed()
        {
            act(out, UiAction::ToggleSelect(song.id.clone()));
        }
        let current = s.current.as_ref() == Some(&song.id);
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
        let rating = song.rating().map(|r| r.as_str()).unwrap_or("unreviewed");
        let text = format!(
            "{}  ·  {}  ·  seed {}  ·  rev {}  ·  {}{}{}",
            song.title(),
            song.run_name().unwrap_or_default(),
            song.seed()
                .map(|x| x.to_string())
                .unwrap_or_else(|| "?".into()),
            song.revision()
                .map(|x| x.to_string())
                .unwrap_or_else(|| "?".into()),
            rating,
            if song.meta.tags.is_empty() {
                String::new()
            } else {
                format!("  ·  #{}", song.meta.tags.join(" #"))
            },
            if flags.is_empty() {
                String::new()
            } else {
                format!("  ·  {}", flags.join(", "))
            },
        );
        let r = ui.selectable_label(current, text);
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

fn player(ui: &mut Ui, s: &AppState, out: &mut O, song: &Song) {
    let p = &s.player;
    let this = p.song.as_ref() == Some(&song.id);
    ui.horizontal(|ui| {
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
    });
    waveform(ui, s, out, song);
}

/// Waveform strip with a click-to-seek bar.
fn waveform(ui: &mut Ui, s: &AppState, out: &mut O, song: &Song) {
    let p = &s.player;
    let this = p.song.as_ref() == Some(&song.id);
    let width = ui.available_width().max(100.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 56.0), Sense::click_and_drag());
    let dur = p.duration.as_secs_f32();
    let frac = if this && dur > 0.0 {
        (p.position.as_secs_f32() / dur).clamp(0.0, 1.0)
    } else {
        0.0
    };
    resp.widget_info(|| egui::WidgetInfo::slider(this, frac as f64, "Seek"));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let peaks = p
        .peaks
        .as_ref()
        .filter(|(id, _)| id == &song.id)
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
    let x = rect.left() + frac * rect.width();
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        (2.0, ui.visuals().strong_text_color()),
    );
    if (resp.clicked() || resp.dragged())
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
        warn_text(ui, "No WAV master: WAV export decodes the MP4.");
    }
    if song.wav_ok == Some(false) {
        warn_text(ui, "WAV master does not match the recipe's wav_sha256.");
    }
    player(ui, s, out, song);
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
    });
    ui.horizontal_wrapped(|ui| {
        let n = if s.selected.is_empty() {
            1
        } else {
            s.selected.len()
        };
        for (fmt, label) in [
            (ExportFormat::Mp4, "MP4"),
            (ExportFormat::Wav, "WAV"),
            (ExportFormat::Mp3, "MP3"),
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
                UiAction::SetMeta(audiocpp_gui_core::MetaEditor {
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
                UiAction::SetMeta(audiocpp_gui_core::MetaEditor {
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
                UiAction::SetMeta(audiocpp_gui_core::MetaEditor {
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
            UiAction::SetMeta(audiocpp_gui_core::MetaEditor {
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
// Review mode (§7.3)

fn review(ui: &mut Ui, s: &AppState, out: &mut O) {
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
    });
    ui.weak("Space play/pause · ←/→ seek 5 s (Shift 30 s) · 1/2/3 rate good/neutral/bad and next · N/P next/previous · T tag · R rename");
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
            player(ui, s, out, song);
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
            });
            ui.separator();
            metadata_editor(ui, s, out, true);
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

// ---------------------------------------------------------------------------------------
// Log

fn log(ui: &mut Ui, s: &AppState, out: &mut O) {
    ui.horizontal(|ui| {
        ui.heading("Log");
        if ib(ui, ph::TRASH, "Clear log").clicked() {
            act(out, UiAction::ClearLog);
        }
    });
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    let lines: Vec<_> = s.log.iter().collect();
    egui::ScrollArea::vertical()
        .id_salt("log")
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
                ui.label(
                    RichText::new(format!(
                        "{:02}:{:02}:{:02} {:>10}  {}",
                        t / 3600,
                        (t / 60) % 60,
                        t % 60,
                        l.source,
                        l.message
                    ))
                    .monospace()
                    .color(color),
                );
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
                (egui::Key::N, ReviewKey::Next),
                (egui::Key::P, ReviewKey::Prev),
                (egui::Key::T, ReviewKey::Tag),
                (egui::Key::R, ReviewKey::Rename),
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
        Tab::Library if keys.contains(&ReviewKey::PlayPause) => act(out, UiAction::TogglePlay),
        _ => {}
    }
}
