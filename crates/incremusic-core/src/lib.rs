//! incremusic-core: config, launcher, API client, scheduler, media, library and playback.
//! No GUI dependencies (design §2).

pub mod abc;
pub mod api;
pub mod config;
pub mod error;
pub mod fsutil;
pub mod history;
pub mod launcher;
pub mod library;
pub mod media;
pub mod models;
pub mod params;
pub mod playback;
pub mod project;
pub mod run;
pub mod scheduler;
pub mod service;
pub mod transcribe;
pub mod wav;

pub use error::{Error, Result};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub use library::{LibraryCommand, LibraryDelta, SongId};
pub use run::{JobId, RunId};
pub use scheduler::ServerId;
pub use service::{Command, CoreBackend, CoreHandle, CoreOptions, Event, ServerState};
