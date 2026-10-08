//! IncreMusic-Yue2 (`incremusic-yue2`): batch song generation with audio.cpp servers.

use std::path::PathBuf;
use std::sync::Arc;

use incremusic_core::config::Config;
use incremusic_core::service::{CoreHandle, CoreOptions};
use incremusic_gui::{GuiApp, ThreadEffects};

fn usage() -> ! {
    eprintln!("usage: incremusic-yue2 [--config <path>]");
    std::process::exit(2);
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let mut args = std::env::args().skip(1);
    let mut config_path: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "-c" => {
                config_path = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage()))
            }
            "--help" | "-h" => usage(),
            other => {
                eprintln!("unknown argument {other}");
                usage()
            }
        }
    }
    let path = config_path
        .or_else(Config::default_path)
        .unwrap_or_else(|| PathBuf::from("config.ron"));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("IncreMusic-Yue2")
            .with_inner_size([1280.0, 860.0])
            // .abc and audio files can be dropped onto the Generate panel (§5.2)
            .with_drag_and_drop(true),
        ..Default::default()
    };
    let config = Config::load(&path);
    eframe::run_native(
        "incremusic-yue2",
        options,
        Box::new(move |cc| {
            incremusic_gui::install_fonts(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            match config {
                Ok(cfg) => {
                    let start_dir = Some(cfg.library.root.clone());
                    let core = CoreHandle::start(
                        cfg,
                        CoreOptions {
                            wake: Some(Arc::new(move || ctx.request_repaint())),
                            ..Default::default()
                        },
                    )?;
                    Ok(Box::new(GuiApp::new(
                        Arc::new(core),
                        Box::new(ThreadEffects { start_dir }),
                    )))
                }
                Err(e) => Ok(Box::new(ConfigError {
                    path,
                    message: e.to_string(),
                })),
            }
        }),
    )
}

/// Shown instead of the app when the config can't be loaded.
struct ConfigError {
    path: PathBuf,
    message: String,
}

impl eframe::App for ConfigError {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.heading("Configuration problem");
            ui.label(format!("Config file: {}", self.path.display()));
            ui.label(egui::RichText::new(&self.message).color(ui.visuals().error_fg_color));
            ui.label("Fix the file (see docs/DESIGN.md §3 or config.example.ron) and restart. Use --config <path> for another file.");
        });
    }
}
