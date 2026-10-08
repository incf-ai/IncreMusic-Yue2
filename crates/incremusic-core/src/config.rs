//! RON configuration (design §3).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::params::Preset;

pub const APP_NAME: &str = "incremusic-yue2";
/// The app's name before the rename; its config folder is used until the new one exists.
const LEGACY_APP_NAME: &str = "audiocpp-ui";

/// RON options used for every read and write: all extensions enabled (§3).
pub fn ron_options() -> ron::Options {
    ron::Options::default().with_default_extension(ron::extensions::Extensions::all())
}

pub fn ron_pretty() -> ron::ser::PrettyConfig {
    ron::ser::PrettyConfig::default().struct_names(true)
}

pub fn from_ron<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    ron_options()
        .from_str(text)
        .map_err(|e| Error::Config(e.to_string()))
}

pub fn to_ron<T: Serialize>(value: &T) -> Result<String> {
    ron_options()
        .to_string_pretty(value, ron_pretty())
        .map_err(|e| Error::Config(e.to_string()))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Config")]
pub struct Config {
    #[serde(default)]
    pub server_binary: Option<PathBuf>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub terminal: TerminalMode,
    /// macOS only: terminal app for `open -a` (default "Terminal").
    #[serde(default)]
    pub macos_terminal_app: Option<String>,
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
    /// `ffmpeg` binary; `ffprobe` is looked up next to it. Default: `PATH`.
    #[serde(default)]
    pub ffmpeg: Option<PathBuf>,
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
    pub models: Models,
    pub library: LibraryConfig,
    /// A `Preset` file, relative to the config file's directory.
    #[serde(default)]
    pub defaults: Option<PathBuf>,
}

fn default_timeout() -> u64 {
    1800
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum TerminalMode {
    #[default]
    Native,
    Command(Vec<String>),
    Headless,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Server")]
pub struct ServerConfig {
    pub name: String,
    pub port: u16,
    /// `None` → attach-only.
    #[serde(default)]
    pub launch: Option<Launch>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Launch")]
pub struct Launch {
    pub backend: String,
    #[serde(default)]
    pub device: Option<u32>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub autostart: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Models")]
pub struct Models {
    pub yue2: ModelSpec,
    pub sheetsage2: ModelSpec,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename = "ModelSpec")]
pub struct ModelSpec {
    pub id: String,
    pub family: String,
    pub task: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub path: String,
    #[serde(default)]
    pub load_options: BTreeMap<String, String>,
    #[serde(default)]
    pub session_options: BTreeMap<String, String>,
}

fn default_mode() -> String {
    "offline".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Library")]
pub struct LibraryConfig {
    pub root: PathBuf,
    #[serde(default)]
    pub encoder: Encoder,
}

/// LAME settings for the library MP3 (§6.1).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// Variable bitrate, LAME `-V<quality>`: 0 (best, ~245 kbps) to 9.
    Vbr { quality: u8 },
    /// Constant bitrate, 32–320 kbps.
    Cbr { bitrate_kbps: u32 },
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder::Vbr { quality: 0 }
    }
}

impl Encoder {
    pub fn describe(&self) -> String {
        match self {
            Encoder::Vbr { quality } => format!("mp3 V{quality}"),
            Encoder::Cbr { bitrate_kbps } => format!("mp3 {bitrate_kbps}k"),
        }
    }
}

impl Config {
    /// Default location from `directories::ProjectDirs`, or the pre-rename one if only that
    /// exists.
    pub fn default_path() -> Option<PathBuf> {
        let path = |name| {
            directories::ProjectDirs::from("", "", name).map(|d| d.config_dir().join("config.ron"))
        };
        let current = path(APP_NAME)?;
        match path(LEGACY_APP_NAME) {
            Some(legacy) if !current.exists() && legacy.exists() => Some(legacy),
            _ => Some(current),
        }
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        let mut cfg: Config = from_ron(&text)?;
        cfg.library.root = expand_tilde(&cfg.library.root);
        if let (Some(d), Some(parent)) = (&cfg.defaults, path.parent())
            && d.is_relative()
        {
            cfg.defaults = Some(parent.join(d));
        }
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn parse(text: &str) -> Result<Config> {
        let mut cfg: Config = from_ron(text)?;
        cfg.library.root = expand_tilde(&cfg.library.root);
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_ron(&self) -> Result<String> {
        to_ron(self)
    }

    pub fn validate(&self) -> Result<()> {
        let mut problems = Vec::new();
        let mut names = std::collections::HashSet::new();
        let mut ports = std::collections::HashSet::new();
        for s in &self.servers {
            if s.name.trim().is_empty() {
                problems.push("server with empty name".to_string());
            }
            if !names.insert(&s.name) {
                problems.push(format!("duplicate server name `{}`", s.name));
            }
            if !ports.insert(s.port) {
                problems.push(format!("duplicate port {}", s.port));
            }
            if s.port == 0 {
                problems.push(format!("server `{}`: port must not be 0", s.name));
            }
            if s.launch.is_some() && self.server_binary.is_none() {
                problems.push(format!(
                    "server `{}` has `launch` but `server_binary` is not set",
                    s.name
                ));
            }
        }
        if let TerminalMode::Command(argv) = &self.terminal {
            if argv.is_empty() {
                problems.push("terminal: Command([...]) must not be empty".into());
            } else if !argv.iter().any(|a| a.contains("{script}")) {
                problems.push("terminal: Command([...]) must contain `{script}`".into());
            }
        }
        if self.request_timeout_secs == 0 {
            problems.push("request_timeout_secs must be > 0".into());
        }
        for (what, m) in [
            ("yue2", &self.models.yue2),
            ("sheetsage2", &self.models.sheetsage2),
        ] {
            if m.id.is_empty() || m.path.is_empty() {
                problems.push(format!("models.{what}: `id` and `path` are required"));
            }
        }
        match self.library.encoder {
            Encoder::Vbr { quality } if quality > 9 => problems.push(format!(
                "library.encoder: VBR quality {quality} out of range 0–9"
            )),
            Encoder::Cbr { bitrate_kbps } if !(32..=320).contains(&bitrate_kbps) => problems.push(
                format!("library.encoder: bitrate {bitrate_kbps} kbps out of range 32–320"),
            ),
            _ => {}
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Config(problems.join("; ")))
        }
    }

    pub fn load_default_preset(&self) -> Result<Option<Preset>> {
        match &self.defaults {
            Some(p) if p.exists() => Preset::load(p).map(Some),
            _ => Ok(None),
        }
    }

    pub fn ffmpeg_path(&self) -> PathBuf {
        self.ffmpeg
            .clone()
            .unwrap_or_else(|| PathBuf::from("ffmpeg"))
    }

    pub fn ffprobe_path(&self) -> PathBuf {
        match &self.ffmpeg {
            Some(p) => {
                let name = if cfg!(windows) {
                    "ffprobe.exe"
                } else {
                    "ffprobe"
                };
                p.with_file_name(name)
            }
            None => PathBuf::from("ffprobe"),
        }
    }
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if (s == "~" || s.starts_with("~/") || s.starts_with("~\\"))
        && let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
    {
        return if s.len() <= 2 {
            home
        } else {
            home.join(&s[2..])
        };
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    pub const EXAMPLE: &str = r#"
Config(
    server_binary: "/opt/audiocpp/audiocpp_server",
    working_dir: "/opt/audiocpp",
    terminal: Native,
    request_timeout_secs: 1800,
    servers: [
        Server(
            name: "gpu1",
            port: 9123,
            launch: Launch(
                backend: "vulkan",
                device: 1,
                extra_args: ["--ui", "--ui-management", "--log"],
                autostart: true,
            ),
        ),
        Server(
            name: "gpu2",
            port: 9124,
            launch: Launch(backend: "vulkan", device: 2,
                           extra_args: ["--ui", "--ui-management", "--log"], autostart: true),
        ),
        Server(name: "attached", port: 9125),
    ],
    models: Models(
        yue2: ModelSpec(
            id: "yue2", family: "yue2", task: "gen", mode: "offline",
            path: "/mnt/storage/audiocpp/models/Yue2-3B-GGUF",
            session_options: {
                "yue2.ar_lora_scale": "1",
                "yue2.nar_lora_scale": "1",
                "yue2.model_gguf": "yue2-3b-bf16.gguf",
                "yue2.vae_gguf": "yue2-vae-f32.gguf",
            },
        ),
        sheetsage2: ModelSpec(
            id: "sheetsage2", family: "sheetsage2", task: "midi", mode: "offline",
            path: "/mnt/storage/audiocpp/models/SheetSage2-GGUF/sheetsage2-orig.gguf",
            session_options: {},
        ),
    ),
    library: Library(
        root: "/music/incremusic",
        encoder: Vbr(quality: 0),
    ),
    defaults: "presets/default.ron",
)
"#;

    #[test]
    fn parses_design_example() {
        let cfg = Config::parse(EXAMPLE).unwrap();
        assert_eq!(cfg.servers.len(), 3);
        assert_eq!(cfg.servers[0].launch.as_ref().unwrap().device, Some(1));
        assert!(cfg.servers[2].launch.is_none());
        assert_eq!(cfg.terminal, TerminalMode::Native);
        assert_eq!(
            cfg.models.yue2.session_options["yue2.model_gguf"],
            "yue2-3b-bf16.gguf"
        );
        assert_eq!(cfg.library.encoder, Encoder::Vbr { quality: 0 });
        assert_eq!(cfg.defaults, Some(PathBuf::from("presets/default.ron")));
    }

    #[test]
    fn struct_names_are_required() {
        let text = EXAMPLE.replace(
            "Server(\n            name: \"gpu1\"",
            "(\n            name: \"gpu1\"",
        );
        let err = Config::parse(&text).unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("struct") || err.contains("Server"),
            "{err}"
        );
    }

    #[test]
    fn enable_lines_are_harmless() {
        let text = format!("#![enable(implicit_some)]\n{EXAMPLE}");
        Config::parse(&text).unwrap();
    }

    #[test]
    fn round_trips() {
        let cfg = Config::parse(EXAMPLE).unwrap();
        let text = cfg.to_ron().unwrap();
        assert!(text.contains("Server("), "{text}");
        let back = Config::parse(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn terminal_variants() {
        for (src, want) in [
            ("Headless", TerminalMode::Headless),
            (
                r#"Command(["kitty", "--title", "{name}", "--", "{script}"])"#,
                TerminalMode::Command(vec![
                    "kitty".into(),
                    "--title".into(),
                    "{name}".into(),
                    "--".into(),
                    "{script}".into(),
                ]),
            ),
        ] {
            let text = EXAMPLE.replace("terminal: Native", &format!("terminal: {src}"));
            assert_eq!(Config::parse(&text).unwrap().terminal, want);
        }
    }

    #[test]
    fn validation_errors() {
        let dup = EXAMPLE.replace("port: 9124", "port: 9123");
        assert!(
            Config::parse(&dup)
                .unwrap_err()
                .to_string()
                .contains("duplicate port 9123")
        );
        let no_script = EXAMPLE.replace("terminal: Native", r#"terminal: Command(["kitty"])"#);
        assert!(
            Config::parse(&no_script)
                .unwrap_err()
                .to_string()
                .contains("{script}")
        );
        let no_bin = EXAMPLE.replace(r#"server_binary: "/opt/audiocpp/audiocpp_server","#, "");
        assert!(
            Config::parse(&no_bin)
                .unwrap_err()
                .to_string()
                .contains("server_binary")
        );
        let bad_rate = EXAMPLE.replace("Vbr(quality: 0)", "Cbr(bitrate_kbps: 5)");
        assert!(
            Config::parse(&bad_rate)
                .unwrap_err()
                .to_string()
                .contains("bitrate")
        );
        let bad_quality = EXAMPLE.replace("Vbr(quality: 0)", "Vbr(quality: 10)");
        assert!(
            Config::parse(&bad_quality)
                .unwrap_err()
                .to_string()
                .contains("quality")
        );
    }

    #[test]
    fn zero_servers_is_valid() {
        let text = r#"Config(
            models: Models(
                yue2: ModelSpec(id: "yue2", family: "yue2", task: "gen", path: "/m/y"),
                sheetsage2: ModelSpec(id: "sheetsage2", family: "sheetsage2", task: "midi", path: "/m/s"),
            ),
            library: Library(root: "/tmp/lib", encoder: Cbr(bitrate_kbps: 320)),
        )"#;
        let cfg = Config::parse(text).unwrap();
        assert!(cfg.servers.is_empty());
        assert_eq!(cfg.request_timeout_secs, 1800);
        assert_eq!(cfg.models.yue2.mode, "offline");
    }

    #[test]
    fn shipped_example_and_preset_parse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cfg = Config::load(&root.join("config.example.ron")).unwrap();
        assert_eq!(cfg.servers.len(), 2);
        let preset = cfg
            .load_default_preset()
            .unwrap()
            .expect("defaults resolve relative to the config");
        assert_eq!(
            preset.params,
            crate::params::GenerationParams {
                lyrics: preset.params.lyrics.clone(),
                style: preset.params.style.clone(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn tilde_expands() {
        let p = expand_tilde(Path::new("~/Music/x"));
        assert!(!p.to_string_lossy().starts_with('~'));
        assert!(p.ends_with("Music/x"));
    }
}
