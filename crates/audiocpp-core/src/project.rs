//! Project folders `inputs/<name>/` (design §5.2.1): reference audio, its WAV conversion,
//! transcriptions and the ABC/lyrics/style a run used.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{from_ron, to_ron};
use crate::error::{Error, IoContext, Result};
use crate::fsutil;
use crate::media::Ffmpeg;
use crate::params::GenerationParams;
use crate::run::{AbcSource, ReferenceAudio, RunId, RunName};

pub const PROJECT_FILE: &str = "project.ron";
pub const TRANSCRIPTION_ABC: &str = "transcription.abc";
pub const TRANSCRIPTION_EVENTS: &str = "transcription.events.json";
pub const UPLOAD_WAV: &str = "reference.upload.wav";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Project")]
pub struct Project {
    pub name: String,
    pub created_at: String,
    #[serde(default)]
    pub reference: Option<Reference>,
    #[serde(default)]
    pub transcription: Option<Transcription>,
    #[serde(default)]
    pub abc: Option<AbcFile>,
    #[serde(default)]
    pub runs: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Reference")]
pub struct Reference {
    pub original_name: String,
    pub file: String,
    pub sha256: String,
    /// `None` → the original was uploaded (PCM s16 WAV).
    #[serde(default)]
    pub upload_file: Option<String>,
    pub upload_sha256: String,
    #[serde(default)]
    pub conversion: Option<String>,
    #[serde(default)]
    pub format: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Transcription")]
pub struct Transcription {
    pub file: String,
    pub model: String,
    pub created_at: String,
    pub wall_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "AbcFile")]
pub struct AbcFile {
    pub file: String,
    pub source: AbcFileSource,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum AbcFileSource {
    Transcribed,
    File(String),
    Manual,
}

/// What "Load project inputs" fills into the editors.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct ProjectInputs {
    pub name: String,
    pub abc: Option<String>,
    pub lyrics: Option<String>,
    pub style: Option<String>,
    pub abc_source: Option<AbcSource>,
    pub has_reference: bool,
}

/// One entry of the project list shown in the Generate panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub name: String,
    pub has_reference: bool,
}

/// A reference that has been probed, copied and converted, ready to upload.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedReference {
    pub project: String,
    /// The WAV to upload: the converted file, or the original if it was PCM s16 WAV.
    pub upload_path: PathBuf,
    pub reference: ReferenceAudio,
}

#[derive(Clone, Debug)]
pub struct ProjectStore {
    dir: PathBuf,
}

impl ProjectStore {
    pub fn new(inputs_dir: impl Into<PathBuf>) -> Self {
        ProjectStore {
            dir: inputs_dir.into(),
        }
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.dir(name).join(PROJECT_FILE).is_file()
    }

    pub fn list(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut v: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join(PROJECT_FILE).is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    pub fn summaries(&self) -> Vec<ProjectSummary> {
        self.list()
            .into_iter()
            .map(|name| {
                let has_reference = self
                    .load(&name)
                    .map(|p| p.reference.is_some())
                    .unwrap_or(false);
                ProjectSummary {
                    name,
                    has_reference,
                }
            })
            .collect()
    }

    /// The project's original reference file, if any.
    pub fn reference_path(&self, name: &str) -> Option<PathBuf> {
        let p = self.load(name).ok()?;
        let f = self.dir(name).join(p.reference?.file);
        f.is_file().then_some(f)
    }

    pub fn load(&self, name: &str) -> Result<Project> {
        let p = self.dir(name).join(PROJECT_FILE);
        from_ron(&std::fs::read_to_string(&p).at(&p)?)
    }

    /// Loads the project, or a fresh one (not yet written).
    pub fn load_or_new(&self, name: &RunName) -> Result<Project> {
        if self.exists(name.as_str()) {
            self.load(name.as_str())
        } else {
            Ok(Project {
                name: name.to_string(),
                created_at: fsutil::now_rfc3339(),
                reference: None,
                transcription: None,
                abc: None,
                runs: vec![],
            })
        }
    }

    pub fn save(&self, p: &Project) -> Result<()> {
        fsutil::write_atomic(&self.dir(&p.name).join(PROJECT_FILE), to_ron(p)?.as_bytes())
    }

    fn write(&self, name: &str, file: &str, text: &str) -> Result<()> {
        fsutil::write_atomic(&self.dir(name).join(file), text.as_bytes())
    }

    pub fn inputs(&self, name: &str) -> Result<ProjectInputs> {
        let p = self.load(name)?;
        let dir = self.dir(name);
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
        let abc = p
            .abc
            .as_ref()
            .and_then(|a| read(&a.file))
            .or_else(|| read(TRANSCRIPTION_ABC));
        let abc_source = match (&p.abc, &p.reference) {
            (
                Some(AbcFile {
                    source: AbcFileSource::File(orig),
                    file,
                }),
                _,
            ) => Some(AbcSource::File {
                file_name: orig.clone(),
                sha256: read(file)
                    .map(|t| fsutil::sha256_hex(t.as_bytes()))
                    .unwrap_or_default(),
            }),
            (
                Some(AbcFile {
                    source: AbcFileSource::Manual,
                    ..
                }),
                _,
            ) => Some(AbcSource::Manual),
            (
                Some(AbcFile {
                    source: AbcFileSource::Transcribed,
                    ..
                })
                | None,
                Some(r),
            ) if abc.is_some() => Some(AbcSource::Transcribed(reference_audio(
                r,
                p.transcription.as_ref(),
            ))),
            _ => None,
        };
        Ok(ProjectInputs {
            name: name.to_string(),
            abc,
            lyrics: read("lyrics.txt"),
            style: read("style.txt"),
            abc_source,
            has_reference: p.reference.is_some(),
        })
    }

    /// Finds an existing transcription of audio with this SHA-256 in any project (§5.2).
    pub fn find_transcription(&self, sha256: &str) -> Option<(Project, PathBuf)> {
        self.list().into_iter().find_map(|n| {
            let p = self.load(&n).ok()?;
            let r = p.reference.as_ref()?;
            let t = p.transcription.as_ref()?;
            let path = self.dir(&n).join(&t.file);
            (r.sha256 == sha256 && path.is_file()).then_some((p, path))
        })
    }

    /// Probes, copies and converts a reference into the project (§5.2 steps 1–3).
    /// Unreadable files are rejected before anything is copied. An existing reference and
    /// its transcription go to the system trash (the UI confirms first).
    pub fn prepare_reference(
        &self,
        name: &RunName,
        audio: &Path,
        ffmpeg: &Ffmpeg,
    ) -> Result<PreparedReference> {
        let probe = ffmpeg.probe(audio)?;
        let sha256 = fsutil::sha256_file(audio)?;
        let mut project = self.load_or_new(name)?;
        let dir = self.dir(name.as_str());
        std::fs::create_dir_all(&dir).at(&dir)?;

        let same = project
            .reference
            .as_ref()
            .is_some_and(|r| r.sha256 == sha256 && dir.join(&r.file).is_file());
        if !same {
            if project.reference.is_some() {
                self.trash_reference(name.as_str())?;
                project.transcription = None;
                if project
                    .abc
                    .as_ref()
                    .is_some_and(|a| a.source == AbcFileSource::Transcribed)
                {
                    project.abc = None;
                }
            }
            let ext = fsutil::extension_lower(audio)
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| probe.format_label());
            let file = format!("reference.{ext}");
            fsutil::copy_file(audio, &dir.join(&file))?;
            let (upload_file, conversion) = if probe.is_pcm_s16_wav() {
                (None, None)
            } else {
                let tmp = dir.join(format!(".{UPLOAD_WAV}.tmp.wav"));
                ffmpeg.to_wav_s16(&dir.join(&file), &tmp)?;
                std::fs::rename(&tmp, dir.join(UPLOAD_WAV)).at(dir.join(UPLOAD_WAV))?;
                (
                    Some(UPLOAD_WAV.to_string()),
                    Some(format!("{} -c:a pcm_s16le", ffmpeg.version_short())),
                )
            };
            let upload_sha256 =
                fsutil::sha256_file(&dir.join(upload_file.as_deref().unwrap_or(&file)))?;
            project.reference = Some(Reference {
                original_name: audio
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                file,
                sha256,
                upload_file,
                upload_sha256,
                conversion,
                format: probe.format_label(),
            });
            self.save(&project)?;
        }
        let r = project.reference.as_ref().unwrap();
        Ok(PreparedReference {
            project: name.to_string(),
            upload_path: dir.join(r.upload_file.as_deref().unwrap_or(&r.file)),
            reference: reference_audio(r, project.transcription.as_ref()),
        })
    }

    fn trash_reference(&self, name: &str) -> Result<()> {
        let dir = self.dir(name);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(());
        };
        for e in rd.filter_map(|e| e.ok()) {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("reference.") || n.starts_with("transcription.") {
                trash::delete(e.path()).map_err(|err| Error::Other(format!("trash {n}: {err}")))?;
            }
        }
        Ok(())
    }

    /// Stores a SheetSage2 result (§5.2 step 4).
    pub fn save_transcription(
        &self,
        name: &str,
        abc: &str,
        events: Option<&str>,
        model: &str,
        wall_ms: u64,
    ) -> Result<()> {
        self.write(name, TRANSCRIPTION_ABC, abc)?;
        if let Some(ev) = events {
            self.write(name, TRANSCRIPTION_EVENTS, ev)?;
        }
        let mut p = self.load(name)?;
        p.transcription = Some(Transcription {
            file: TRANSCRIPTION_ABC.into(),
            model: model.into(),
            created_at: fsutil::now_rfc3339(),
            wall_ms,
        });
        self.save(&p)
    }

    /// Copies a transcription from another project (or this one) without a server call.
    pub fn reuse_transcription(&self, name: &str, from: &Project) -> Result<String> {
        let src = self.dir(&from.name);
        let dst = self.dir(name);
        if src != dst {
            for f in [TRANSCRIPTION_ABC, TRANSCRIPTION_EVENTS] {
                if src.join(f).is_file() {
                    fsutil::copy_file(&src.join(f), &dst.join(f))?;
                }
            }
            let mut p = self.load(name)?;
            p.transcription = from.transcription.clone();
            self.save(&p)?;
        }
        std::fs::read_to_string(dst.join(TRANSCRIPTION_ABC)).at(dst.join(TRANSCRIPTION_ABC))
    }

    /// Writes `<name>.abc`, `lyrics.txt`, `style.txt` and records the run (§5.2.1). Called
    /// when a run starts and on every params edit.
    pub fn write_run_inputs(
        &self,
        name: &RunName,
        params: &GenerationParams,
        src: &AbcSource,
        run: Option<RunId>,
    ) -> Result<()> {
        let mut p = self.load_or_new(name)?;
        let abc_file = format!("{name}.abc");
        match (&params.abc, src) {
            (Some(abc), _) if !matches!(src, AbcSource::None) => {
                self.write(name.as_str(), &abc_file, abc)?;
                p.abc = Some(AbcFile {
                    file: abc_file,
                    source: match src {
                        AbcSource::File { file_name, .. } => AbcFileSource::File(file_name.clone()),
                        AbcSource::Transcribed(_) => AbcFileSource::Transcribed,
                        _ => AbcFileSource::Manual,
                    },
                });
            }
            _ => {
                p.abc = None;
                let _ = std::fs::remove_file(self.dir(name.as_str()).join(&abc_file));
            }
        }
        self.write(name.as_str(), "lyrics.txt", &params.lyrics)?;
        self.write(name.as_str(), "style.txt", &params.style)?;
        if let Some(id) = run {
            let id = id.to_string();
            if !p.runs.contains(&id) {
                p.runs.push(id);
            }
        }
        self.save(&p)
    }
}

fn reference_audio(r: &Reference, t: Option<&Transcription>) -> ReferenceAudio {
    ReferenceAudio {
        file_name: r.original_name.clone(),
        format: r.format.clone(),
        sha256: r.sha256.clone(),
        upload_sha256: r.upload_sha256.clone(),
        abc_model: t
            .map(|t| t.model.clone())
            .unwrap_or_else(|| "sheetsage2".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn project_ron_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let s = ProjectStore::new(d.path());
        let p = Project {
            name: "sunny-hook".into(),
            created_at: "2026-09-23T21:40:02Z".into(),
            reference: Some(Reference {
                original_name: "My Demo (final).mp3".into(),
                file: "reference.mp3".into(),
                sha256: "a".into(),
                upload_file: Some(UPLOAD_WAV.into()),
                upload_sha256: "b".into(),
                conversion: Some("ffmpeg 7.1 -c:a pcm_s16le".into()),
                format: "mp3".into(),
            }),
            transcription: Some(Transcription {
                file: TRANSCRIPTION_ABC.into(),
                model: "sheetsage2".into(),
                created_at: "x".into(),
                wall_ms: 10513,
            }),
            abc: Some(AbcFile {
                file: "sunny-hook.abc".into(),
                source: AbcFileSource::File("m.abc".into()),
            }),
            runs: vec!["01J8".into()],
        };
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(d.path().join("sunny-hook/project.ron")).unwrap();
        assert!(text.contains("Project("), "{text}");
        assert_eq!(s.load("sunny-hook").unwrap(), p);
        assert_eq!(s.list(), vec!["sunny-hook"]);
    }

    #[test]
    fn run_inputs_are_written_and_loaded() {
        let d = tempfile::tempdir().unwrap();
        let s = ProjectStore::new(d.path());
        let name = RunName::parse("song").unwrap();
        let params = GenerationParams {
            lyrics: "la".into(),
            style: "pop".into(),
            abc: Some("X:1\nK:C\n".into()),
            ..Default::default()
        };
        let id = RunId::new();
        s.write_run_inputs(&name, &params, &AbcSource::Manual, Some(id))
            .unwrap();
        let dir = d.path().join("song");
        assert_eq!(
            std::fs::read_to_string(dir.join("song.abc")).unwrap(),
            "X:1\nK:C\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("lyrics.txt")).unwrap(),
            "la"
        );
        let inputs = s.inputs("song").unwrap();
        assert_eq!(inputs.abc.as_deref(), Some("X:1\nK:C\n"));
        assert_eq!(inputs.style.as_deref(), Some("pop"));
        assert_eq!(inputs.abc_source, Some(AbcSource::Manual));
        assert_eq!(s.load("song").unwrap().runs, vec![id.to_string()]);

        // None removes the ABC file
        let mut p2 = params.clone();
        p2.abc = None;
        s.write_run_inputs(&name, &p2, &AbcSource::None, Some(id))
            .unwrap();
        assert!(!dir.join("song.abc").exists());
        assert_eq!(s.load("song").unwrap().runs.len(), 1);
    }
}
