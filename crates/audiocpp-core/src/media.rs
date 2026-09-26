//! WAV → MP3 via `ffmpeg`, ID3v2 metadata frames, the recipe record and waveform peaks
//! (design §6).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::Timing;
use crate::config::{Encoder, ModelSpec};
use crate::error::{Error, Result};
use crate::run::{AbcSource, RunId};

// ---------------------------------------------------------------------------------------
// Recipe (§6.2)

pub const RECIPE_SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub schema: u32,
    pub app_version: String,
    pub song_id: String,
    pub run: RecipeRun,
    pub created_at: String,
    pub server: RecipeServer,
    pub model: RecipeModel,
    /// The exact `/v1/tasks/run` body sent, including the seed.
    pub request: Value,
    /// `inputs/<project>/` (§5.2.1).
    pub project: String,
    pub abc_source: AbcSource,
    /// Verbatim from the server.
    pub timing: Timing,
    pub output: RecipeOutput,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeRun {
    pub id: RunId,
    pub name: String,
    pub seed: u32,
    pub start_seed: u32,
    pub index: u32,
    pub revision: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeServer {
    pub name: String,
    pub port: u16,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub device: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeModel {
    #[serde(flatten)]
    pub spec: ModelSpec,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub file_hashes: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeOutput {
    pub sample_rate: u32,
    pub channels: u16,
    pub wav_sha256: String,
    pub encoder: String,
}

impl Recipe {
    pub fn has_abc(&self) -> bool {
        self.request.pointer("/request/options/abc").is_some()
    }
}

// ---------------------------------------------------------------------------------------
// Ratings and song metadata (§6.2)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Rating {
    Good,
    Neutral,
    Bad,
}

impl Rating {
    pub const ALL: [Rating; 3] = [Rating::Good, Rating::Neutral, Rating::Bad];

    pub fn as_str(&self) -> &'static str {
        match self {
            Rating::Good => "good",
            Rating::Neutral => "neutral",
            Rating::Bad => "bad",
        }
    }

    pub fn parse(s: &str) -> Option<Rating> {
        Rating::ALL.into_iter().find(|r| r.as_str() == s.trim())
    }
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct SongMeta {
    pub title: Option<String>,
    pub comment: Option<String>,
    pub encoder: Option<String>,
    pub tags: Vec<String>,
    pub rating: Option<Rating>,
    pub recipe: Option<Recipe>,
    pub duration_ms: Option<u64>,
}

/// Prefix of the app's `TXXX` frame descriptions, e.g. `org.audiocpp-ui:recipe`.
const MEAN: &str = "org.audiocpp-ui";
const ID3_VERSION: id3::Version = id3::Version::Id3v24;

fn txxx(name: &str) -> String {
    format!("{MEAN}:{name}")
}

fn meta_err(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Metadata(format!("{}: {e}", path.display()))
}

/// The file's ID3v2 tag, or an empty one if it has none (e.g. a loose MP3 from elsewhere).
fn read_tag(path: &Path) -> Result<id3::Tag> {
    id3::no_tag_ok(id3::Tag::read_from_path(path))
        .map(Option::unwrap_or_default)
        .map_err(|e| meta_err(path, e))
}

pub fn read_meta(path: &Path) -> Result<SongMeta> {
    use id3::TagLike;
    let tag = read_tag(path)?;
    let first = |name: &str| {
        let d = txxx(name);
        tag.extended_texts()
            .find(|t| t.description == d)
            .map(|t| t.value.clone())
    };
    let recipe = match first("recipe") {
        Some(json) => match serde_json::from_str(&json) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("{}: unreadable recipe: {e}", path.display());
                None
            }
        },
        None => None,
    };
    let tags = first("tags")
        .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
        .unwrap_or_default();
    Ok(SongMeta {
        title: tag.title().map(str::to_string),
        comment: tag
            .comments()
            .find(|c| c.description.is_empty())
            .map(|c| c.text.clone()),
        encoder: tag
            .get("TSSE")
            .and_then(|f| f.content().text())
            .map(str::to_string),
        tags,
        rating: first("rating").as_deref().and_then(Rating::parse),
        recipe,
        duration_ms: mp3_duration_ms(path),
    })
}

/// Playable length from the stream headers (the Xing/LAME frame for VBR); no full decode.
fn mp3_duration_ms(path: &Path) -> Option<u64> {
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let format = symphonia::default::get_probe()
        .probe(
            Hint::new().with_extension("mp3"),
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;
    let track = format.default_track(TrackType::Audio)?;
    let rate = track.codec_params.as_ref()?.audio()?.sample_rate?;
    let frames = track.num_frames?;
    (frames > 0).then(|| frames * 1000 / u64::from(rate))
}

/// Writes all app-managed ID3v2.4 frames in place (no re-encode).
pub fn write_meta(path: &Path, meta: &SongMeta) -> Result<()> {
    use id3::TagLike;
    use id3::frame::{Comment, ExtendedText};
    let mut tag = read_tag(path)?;
    match &meta.title {
        Some(t) => tag.set_title(t.clone()),
        None => tag.remove_title(),
    }
    tag.remove_comment(Some(""), None);
    if let Some(c) = meta.comment.as_ref().filter(|c| !c.is_empty()) {
        tag.add_frame(Comment {
            lang: "eng".into(),
            description: String::new(),
            text: c.clone(),
        });
    }
    tag.set_text(
        "TSSE",
        meta.encoder
            .clone()
            .unwrap_or_else(|| format!("audiocpp-ui {}", crate::APP_VERSION)),
    );
    let mut set = |name: &str, v: Option<String>| {
        let d = txxx(name);
        tag.remove_extended_text(Some(&d), None);
        if let Some(value) = v {
            tag.add_frame(ExtendedText {
                description: d,
                value,
            });
        }
    };
    let recipe = meta
        .recipe
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| meta_err(path, e))?;
    set("recipe", recipe);
    set(
        "tags",
        Some(serde_json::to_string(&meta.tags).expect("tags json")),
    );
    set("rating", meta.rating.map(|r| r.as_str().to_string()));
    tag.write_to_path(path, ID3_VERSION)
        .map_err(|e| meta_err(path, e))
}

/// Removes the ID3v2 tag entirely (for exports with "strip metadata").
pub fn strip_meta(path: &Path) -> Result<()> {
    id3::Tag::remove_from_path(path)
        .map(drop)
        .map_err(|e| meta_err(path, e))
}

// ---------------------------------------------------------------------------------------
// ffmpeg / ffprobe (§6.1, §5.2)

#[derive(Clone, Debug)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeInfo {
    /// ffprobe `format_name`, e.g. `mp3`, `wav`, `mov,mp4,m4a,3gp,3g2,mj2`.
    pub format_name: String,
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_s: Option<f64>,
}

impl ProbeInfo {
    /// PCM s16 WAV is uploaded as-is (§5.2).
    pub fn is_pcm_s16_wav(&self) -> bool {
        self.format_name == "wav" && self.codec_name == "pcm_s16le"
    }

    /// Short format label for the recipe, e.g. `mp3`, `flac`, `wav`.
    pub fn format_label(&self) -> String {
        self.format_name
            .split(',')
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFormat {
    Mp3,
    Wav,
    Flac,
}

impl ExportFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Mp3 => "mp3",
            ExportFormat::Wav => "wav",
            ExportFormat::Flac => "flac",
        }
    }
}

impl Ffmpeg {
    pub fn new(ffmpeg: PathBuf, ffprobe: PathBuf) -> Self {
        Ffmpeg { ffmpeg, ffprobe }
    }

    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Ffmpeg::new(cfg.ffmpeg_path(), cfg.ffprobe_path())
    }

    /// Startup check (§6.1). Returns the version line.
    pub fn check(&self) -> Result<String> {
        let out = Command::new(&self.ffmpeg)
            .arg("-version")
            .output()
            .map_err(|e| Error::Ffmpeg(format!("`{}` not runnable: {e}", self.ffmpeg.display())))?;
        let v = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        Command::new(&self.ffprobe)
            .arg("-version")
            .output()
            .map_err(|e| {
                Error::Ffmpeg(format!("`{}` not runnable: {e}", self.ffprobe.display()))
            })?;
        Ok(v)
    }

    /// Short version for provenance, e.g. `ffmpeg 7.1`.
    pub fn version_short(&self) -> String {
        self.check()
            .ok()
            .and_then(|l| {
                l.split_whitespace()
                    .nth(2)
                    .map(|v| format!("ffmpeg {}", v.split('-').next().unwrap_or(v)))
            })
            .unwrap_or_else(|| "ffmpeg".into())
    }

    fn run(&self, args: Vec<OsString>) -> Result<()> {
        let out = Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
            .args(args)
            .output()
            .map_err(|e| Error::Ffmpeg(format!("`{}`: {e}", self.ffmpeg.display())))?;
        if !out.status.success() {
            return Err(Error::Ffmpeg(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }

    /// Detects the type by probing, not from the extension. Unreadable files are an error.
    pub fn probe(&self, path: &Path) -> Result<ProbeInfo> {
        let out = Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-select_streams",
                "a:0",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .map_err(|e| Error::Ffmpeg(format!("`{}`: {e}", self.ffprobe.display())))?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(Error::Ffmpeg(format!(
                "{} is not readable audio: {msg}",
                path.display()
            )));
        }
        let v: Value =
            serde_json::from_slice(&out.stdout).map_err(|e| Error::Ffmpeg(e.to_string()))?;
        let s = v["streams"]
            .get(0)
            .ok_or_else(|| Error::Ffmpeg(format!("{} has no audio stream", path.display())))?;
        Ok(ProbeInfo {
            format_name: v["format"]["format_name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            codec_name: s["codec_name"].as_str().unwrap_or_default().to_string(),
            sample_rate: s["sample_rate"]
                .as_str()
                .and_then(|r| r.parse().ok())
                .unwrap_or(0),
            channels: s["channels"].as_u64().unwrap_or(0) as u32,
            duration_s: v["format"]["duration"]
                .as_str()
                .and_then(|d| d.parse().ok()),
        })
    }

    /// `ffmpeg -i <src> -vn -map_metadata -1 -c:a pcm_s16le <dst>` (§5.2). Keeps the rate and
    /// channel count.
    pub fn to_wav_s16(&self, src: &Path, dst: &Path) -> Result<()> {
        let mut a = args(&["-i"]);
        a.push(src.into());
        a.extend(args(&["-vn", "-map_metadata", "-1", "-c:a", "pcm_s16le"]));
        a.push(dst.into());
        self.run(a)
    }

    /// Encodes the master to MP3 with LAME (§6.1). ffmpeg writes the Xing/LAME header, so
    /// players get the exact length and gapless trim.
    pub fn encode(&self, wav: &Path, out: &Path, encoder: Encoder) -> Result<()> {
        let mut a = args(&["-i"]);
        a.push(wav.into());
        a.extend(args(&["-vn", "-map_metadata", "-1", "-c:a", "libmp3lame"]));
        match encoder {
            Encoder::Vbr { quality } => {
                a.push("-q:a".into());
                a.push(quality.to_string().into());
            }
            Encoder::Cbr { bitrate_kbps } => {
                a.push("-b:a".into());
                a.push(format!("{bitrate_kbps}k").into());
            }
        }
        a.extend(args(&["-f", "mp3"]));
        a.push(out.into());
        self.run(a)
    }

    /// Encodes the master (or, without one, the MP3) to a lossless export format (§7.2).
    pub fn export(&self, src: &Path, out: &Path, format: ExportFormat, strip: bool) -> Result<()> {
        let mut a = args(&["-i"]);
        a.push(src.into());
        a.push("-vn".into());
        if strip {
            a.extend(args(&["-map_metadata", "-1"]));
        }
        a.extend(args(match format {
            ExportFormat::Mp3 => &["-c:a", "copy"],
            ExportFormat::Flac => &["-c:a", "flac"],
            ExportFormat::Wav => &["-c:a", "pcm_s16le"],
        }));
        a.push(out.into());
        self.run(a)
    }
}

fn args(a: &[&str]) -> Vec<OsString> {
    a.iter().map(OsString::from).collect()
}

// ---------------------------------------------------------------------------------------
// Waveform peaks

/// Max-abs peaks over `buckets` equal slices of interleaved samples, in 0..=1.
pub fn peaks(samples: &[f32], channels: usize, buckets: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let frames = samples.len() / channels;
    if frames == 0 || buckets == 0 {
        return vec![0.0; buckets];
    }
    (0..buckets)
        .map(|b| {
            let start = b * frames / buckets;
            let end = ((b + 1) * frames / buckets).max(start + 1).min(frames);
            samples[start * channels..end * channels]
                .iter()
                .fold(0f32, |m, s| m.max(s.abs()))
                .min(1.0)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    pub fn sample_recipe() -> Recipe {
        Recipe {
            schema: RECIPE_SCHEMA,
            app_version: "0.1.0".into(),
            song_id: "01J8TEST".into(),
            run: RecipeRun {
                id: RunId(ulid::Ulid::from_string("01J8ZZZZZZZZZZZZZZZZZZZZZZ").unwrap()),
                name: "sunny-hook".into(),
                seed: 1233,
                start_seed: 1230,
                index: 3,
                revision: 2,
            },
            created_at: "2026-09-23T22:05:11Z".into(),
            server: RecipeServer {
                name: "gpu2".into(),
                port: 9124,
                backend: Some("vulkan".into()),
                device: Some(2),
            },
            model: RecipeModel {
                spec: ModelSpec {
                    id: "yue2".into(),
                    family: "yue2".into(),
                    task: "gen".into(),
                    mode: "offline".into(),
                    path: "/m/Yue2".into(),
                    load_options: Default::default(),
                    session_options: [("yue2.model_gguf".to_string(), "x.gguf".to_string())].into(),
                },
                file_hashes: Default::default(),
            },
            request: serde_json::json!({"model":"yue2","request":{"lyrics":"l","seed":1233,"options":{"abc":"X:1"}}}),
            project: "sunny-hook".into(),
            abc_source: AbcSource::Transcribed(crate::run::ReferenceAudio {
                file_name: "My Demo (final).mp3".into(),
                format: "mp3".into(),
                sha256: "aa".into(),
                upload_sha256: "bb".into(),
                abc_model: "sheetsage2".into(),
            }),
            timing: Timing {
                wall_ms: 155962,
                audio_duration_ms: Some(278439),
                rtf: Some(0.56013),
            },
            output: RecipeOutput {
                sample_rate: 48000,
                channels: 2,
                wav_sha256: "cc".into(),
                encoder: "aac 256k".into(),
            },
        }
    }

    #[test]
    fn recipe_json_shape() {
        let v = serde_json::to_value(sample_recipe()).unwrap();
        assert_eq!(v["run"]["id"], "01J8ZZZZZZZZZZZZZZZZZZZZZZ");
        assert_eq!(v["model"]["family"], "yue2", "model spec is flattened");
        assert_eq!(v["abc_source"]["Transcribed"]["format"], "mp3");
        assert_eq!(v["timing"]["rtf"], 0.56013);
        let manual = serde_json::to_value(AbcSource::Manual).unwrap();
        assert_eq!(manual, "Manual");
        assert!(sample_recipe().has_abc());
    }

    fn tiny_copy(dir: &Path) -> PathBuf {
        let p = dir.join("t.mp3");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/tiny.mp3"),
            &p,
        )
        .unwrap();
        p
    }

    #[test]
    fn metadata_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = tiny_copy(d.path());
        let meta = SongMeta {
            title: Some("sunny-hook-1233".into()),
            comment: Some("nice bridge".into()),
            encoder: None,
            tags: vec!["upbeat".into(), "keeper".into()],
            rating: Some(Rating::Good),
            recipe: Some(sample_recipe()),
            duration_ms: None,
        };
        write_meta(&p, &meta).unwrap();
        let back = read_meta(&p).unwrap();
        assert_eq!(back.title, meta.title);
        assert_eq!(back.comment, meta.comment);
        assert_eq!(back.tags, meta.tags);
        assert_eq!(back.rating, Some(Rating::Good));
        assert_eq!(back.recipe, meta.recipe);
        assert_eq!(
            back.encoder.as_deref(),
            Some(concat!("audiocpp-ui ", env!("CARGO_PKG_VERSION")))
        );
        assert!(back.duration_ms.unwrap() > 100);
        let tag = id3::Tag::read_from_path(&p).unwrap();
        assert_eq!(tag.version(), id3::Version::Id3v24);
        assert!(
            tag.extended_texts()
                .any(|t| t.description == "org.audiocpp-ui:rating" && t.value == "good")
        );

        // edit in place: clear rating, change tags
        let mut m2 = back.clone();
        m2.rating = None;
        m2.tags.clear();
        write_meta(&p, &m2).unwrap();
        let back2 = read_meta(&p).unwrap();
        assert_eq!(back2.rating, None);
        assert!(back2.tags.is_empty());
        assert_eq!(back2.recipe, meta.recipe);

        strip_meta(&p).unwrap();
        let s = read_meta(&p).unwrap();
        assert_eq!(s.recipe, None);
        assert_eq!(s.title, None);
        assert_eq!(s.duration_ms, back.duration_ms, "audio untouched");
    }

    #[test]
    fn peaks_basic() {
        let s = [0.1, -0.5, 0.2, 0.9, -1.5, 0.0, 0.0, 0.0];
        assert_eq!(peaks(&s, 2, 2), vec![0.9, 1.0]);
        assert_eq!(peaks(&[], 2, 3), vec![0.0; 3]);
    }

    #[test]
    fn rating_strings() {
        for r in Rating::ALL {
            assert_eq!(Rating::parse(r.as_str()), Some(r));
        }
        assert_eq!(Rating::parse("meh"), None);
    }
}
