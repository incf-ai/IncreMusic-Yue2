//! egui/eframe front end (design §8). A thin view over `audiocpp_gui_core::AppState`: every
//! frame drains core events, draws the state, and turns input into `UiAction`s.
//!
//! The UI thread never blocks (design §2.3.1): commands are fire-and-forget, and file
//! dialogs and file reads run on helper threads that report back through a channel.

#[cfg(feature = "audio")]
pub mod audio;
pub mod view;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use audiocpp_core::service::CoreBackend;
use audiocpp_gui_core::{AppState, UiAction, update};

/// Adds the Phosphor icon font; call once per `egui::Context`.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

/// Side effects the view asks for that need the OS (file dialogs, reading picked files).
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    PickAbcFile,
    PickReferenceAudio,
    SaveAbcAs(String),
    PickPresetToLoad,
    PickPresetToSave,
    PickExportDir {
        format: audiocpp_core::media::ExportFormat,
        strip_metadata: bool,
    },
}

/// Runs effects. The real one spawns threads; tests record them.
pub trait EffectRunner {
    fn run(&self, effect: Effect, reply: mpsc::Sender<UiAction>, wake: egui::Context);
}

/// Opens native dialogs on helper threads so the UI keeps repainting.
pub struct ThreadEffects {
    pub start_dir: Option<PathBuf>,
}

impl EffectRunner for ThreadEffects {
    fn run(&self, effect: Effect, reply: mpsc::Sender<UiAction>, wake: egui::Context) {
        let start = self.start_dir.clone();
        std::thread::spawn(move || {
            let dialog = || {
                let d = rfd::FileDialog::new();
                match &start {
                    Some(s) => d.set_directory(s),
                    None => d,
                }
            };
            let action = match effect {
                Effect::PickAbcFile => dialog()
                    .add_filter("ABC notation", &["abc"])
                    .pick_file()
                    .and_then(|p| match std::fs::read_to_string(&p) {
                        Ok(contents) => Some(UiAction::AbcFileLoaded {
                            file_name: p
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            contents,
                        }),
                        Err(e) => {
                            tracing::error!("reading {}: {e}", p.display());
                            None
                        }
                    }),
                Effect::PickReferenceAudio => dialog()
                    .add_filter(
                        "Audio",
                        &[
                            "mp3", "wav", "flac", "ogg", "m4a", "mp4", "aac", "opus", "aiff", "wma",
                        ],
                    )
                    .add_filter("All files", &["*"])
                    .pick_file()
                    .map(UiAction::TranscribeFile),
                Effect::SaveAbcAs(text) => {
                    if let Some(p) = dialog()
                        .add_filter("ABC notation", &["abc"])
                        .set_file_name("melody.abc")
                        .save_file()
                        && let Err(e) = std::fs::write(&p, text)
                    {
                        tracing::error!("saving {}: {e}", p.display());
                    }
                    None
                }
                Effect::PickPresetToLoad => dialog()
                    .add_filter("Preset", &["ron"])
                    .pick_file()
                    .map(UiAction::LoadPreset),
                Effect::PickPresetToSave => dialog()
                    .add_filter("Preset", &["ron"])
                    .set_file_name("preset.ron")
                    .save_file()
                    .map(UiAction::SavePreset),
                Effect::PickExportDir {
                    format,
                    strip_metadata,
                } => dialog().pick_folder().map(|dest| UiAction::Export {
                    dest,
                    format,
                    strip_metadata,
                }),
            };
            if let Some(a) = action {
                let _ = reply.send(a);
                wake.request_repaint();
            }
        });
    }
}

/// What the view emits.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Ui(UiAction),
    Effect(Effect),
}

pub struct GuiApp {
    pub state: AppState,
    core: Arc<dyn CoreBackend>,
    effects: Box<dyn EffectRunner>,
    replies_tx: mpsc::Sender<UiAction>,
    replies_rx: mpsc::Receiver<UiAction>,
    /// Keeps the audio stream alive.
    #[cfg(feature = "audio")]
    _audio: Option<audio::AudioOut>,
    /// Effects requested so far (inspected by tests).
    pub effects_log: Vec<Effect>,
}

impl GuiApp {
    pub fn new(core: Arc<dyn CoreBackend>, effects: Box<dyn EffectRunner>) -> GuiApp {
        let (tx, rx) = mpsc::channel();
        #[cfg(feature = "audio")]
        let audio = core
            .playback()
            .and_then(|p| match audio::AudioOut::start(p) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!("no audio output: {e}");
                    None
                }
            });
        GuiApp {
            state: AppState::default(),
            core,
            effects,
            replies_tx: tx,
            replies_rx: rx,
            #[cfg(feature = "audio")]
            _audio: audio,
            effects_log: Vec::new(),
        }
    }

    /// Without an audio device (tests, headless).
    pub fn without_audio(core: Arc<dyn CoreBackend>, effects: Box<dyn EffectRunner>) -> GuiApp {
        let (tx, rx) = mpsc::channel();
        GuiApp {
            state: AppState::default(),
            core,
            effects,
            replies_tx: tx,
            replies_rx: rx,
            #[cfg(feature = "audio")]
            _audio: None,
            effects_log: Vec::new(),
        }
    }

    pub fn dispatch(&mut self, action: UiAction) {
        for cmd in update(&mut self.state, action) {
            self.core.send(cmd);
        }
    }

    /// Drains core events and helper-thread replies. Never waits.
    pub fn pump(&mut self) {
        while let Some(e) = self.core.try_recv() {
            update(&mut self.state, e);
        }
        while let Ok(a) = self.replies_rx.try_recv() {
            self.dispatch(a);
        }
    }

    /// Draws one frame into `ui` (used by eframe and by the headless tests).
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.pump();
        let mut out = Vec::new();
        view::draw(ui, &self.state, &mut out);
        for o in out {
            match o {
                Out::Ui(a) => self.dispatch(a),
                Out::Effect(e) => {
                    self.effects_log.push(e.clone());
                    self.effects
                        .run(e, self.replies_tx.clone(), ui.ctx().clone());
                }
            }
        }
        // elapsed times and the playback cursor move on their own
        let busy = self.state.jobs.values().any(|j| {
            matches!(
                j.state,
                audiocpp_core::scheduler::JobState::Running { .. }
                    | audiocpp_core::scheduler::JobState::Cancelling { .. }
            )
        }) || self.state.servers.iter().any(|s| {
            matches!(
                s.state,
                audiocpp_core::ServerState::Stopping { .. }
                    | audiocpp_core::ServerState::Starting { .. }
                    | audiocpp_core::ServerState::Busy { .. }
            )
        });
        if busy {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let close = ui.ctx().input(|i| i.viewport().close_requested());
        if close && self.state.quit.is_none() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dispatch(UiAction::RequestExit);
        }
        self.show(ui);
        if self.state.quit.is_some() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
