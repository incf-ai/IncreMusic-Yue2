//! Library folder layout, scanning, rename/tag/rate/move, export and undo (design §7).
//!
//! The song folder is the unit: every move, rename and delete acts on the whole folder so
//! the MP3 and its WAV master never drift apart.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::config::{from_ron, to_ron};
use crate::error::{Error, IoContext, Result};
use crate::fsutil;
use crate::media::{self, ExportFormat, Ffmpeg, Rating, SongMeta};
use crate::run::{RunName, parse_stem};

pub const UNREVIEWED: &str = "unreviewed";
pub const REVIEWED: &str = "reviewed";
pub const INPUTS: &str = "inputs";
pub const EXPORTS: &str = "exports";
pub const CACHE: &str = ".cache";
/// Run history records (`history.rs`).
pub const RUNS: &str = "runs";
pub const PART_SUFFIX: &str = ".part";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SongId(pub String);

impl fmt::Display for SongId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Location {
    Unreviewed,
    Reviewed(Rating),
}

impl Location {
    pub const ALL: [Location; 4] = [
        Location::Unreviewed,
        Location::Reviewed(Rating::Good),
        Location::Reviewed(Rating::Neutral),
        Location::Reviewed(Rating::Bad),
    ];

    pub fn rel_dir(&self) -> PathBuf {
        match self {
            Location::Unreviewed => PathBuf::from(UNREVIEWED),
            Location::Reviewed(r) => Path::new(REVIEWED).join(r.as_str()),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Location::Unreviewed => "unreviewed",
            Location::Reviewed(r) => r.as_str(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Song {
    pub id: SongId,
    /// Folder name and file stem, `<name>-<seed>`.
    pub stem: String,
    /// The song folder; `None` for a loose MP3 dropped into a rating folder.
    pub dir: Option<PathBuf>,
    pub mp3: PathBuf,
    /// Lossless master; `None` → "no master".
    pub wav: Option<PathBuf>,
    pub location: Location,
    pub meta: SongMeta,
    /// `Some(false)` → the WAV doesn't match `output.wav_sha256`.
    pub wav_ok: Option<bool>,
    pub modified: SystemTime,
    pub size: u64,
}

impl Song {
    /// The rating atom wins over the folder if the file was moved by hand (§6.2).
    pub fn rating(&self) -> Option<Rating> {
        self.meta.rating.or(match self.location {
            Location::Reviewed(r) => Some(r),
            Location::Unreviewed => None,
        })
    }

    pub fn run_name(&self) -> Option<String> {
        self.meta
            .recipe
            .as_ref()
            .map(|r| r.run.name.clone())
            .or_else(|| parse_stem(&self.stem).map(|(n, _)| n))
    }

    pub fn seed(&self) -> Option<u32> {
        self.meta
            .recipe
            .as_ref()
            .map(|r| r.run.seed)
            .or_else(|| parse_stem(&self.stem).map(|(_, s)| s))
    }

    pub fn revision(&self) -> Option<u32> {
        self.meta.recipe.as_ref().map(|r| r.run.revision)
    }

    pub fn has_abc(&self) -> Option<bool> {
        self.meta.recipe.as_ref().map(|r| r.has_abc())
    }

    /// The lyrics the song was generated from, per its recipe. `None` without a recipe or
    /// when the lyrics were empty.
    pub fn lyrics(&self) -> Option<&str> {
        self.meta
            .recipe
            .as_ref()
            .and_then(|r| r.request.pointer("/request/lyrics"))
            .and_then(|v| v.as_str())
            .filter(|l| !l.trim().is_empty())
    }

    pub fn title(&self) -> &str {
        self.meta.title.as_deref().unwrap_or(&self.stem)
    }

    pub fn created_at(&self) -> String {
        self.meta
            .recipe
            .as_ref()
            .map(|r| r.created_at.clone())
            .unwrap_or_default()
    }

    /// The thing that gets moved: the folder, or the loose file.
    pub fn unit_path(&self) -> &Path {
        self.dir.as_deref().unwrap_or(&self.mp3)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct LibraryDelta {
    /// When set, `upserted` is the complete list and everything else is gone.
    pub full: bool,
    pub upserted: Vec<Song>,
    pub removed: Vec<SongId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LibraryCommand {
    Rescan,
    Rate(SongId, Rating),
    Unreview(SongId),
    SetTitle(SongId, String),
    /// Renames folder and files to `<name>-<seed>`.
    Rename(SongId, String),
    SetTags(SongId, Vec<String>),
    SetNotes(SongId, String),
    Delete(SongId),
    Export {
        ids: Vec<SongId>,
        dest: PathBuf,
        format: ExportFormat,
        strip_metadata: bool,
    },
    Undo,
}

#[derive(Clone, Debug, PartialEq)]
enum UndoOp {
    Move {
        id: SongId,
        from: PathBuf,
        to: PathBuf,
        prev_rating: Option<Rating>,
        prev_location: Location,
    },
    Rename {
        id: SongId,
        parent: PathBuf,
        old_stem: String,
        new_stem: String,
        old_title: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename = "Index")]
struct IndexFile {
    entries: Vec<IndexEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename = "Entry")]
struct IndexEntry {
    mp3: PathBuf,
    mtime: (u64, u32),
    size: u64,
    id: String,
    meta: String,
    wav_mtime: Option<(u64, u32)>,
    wav_ok: Option<bool>,
}

fn mtime_key(t: SystemTime) -> (u64, u32) {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    (d.as_secs(), d.subsec_nanos())
}

pub struct Library {
    root: PathBuf,
    songs: BTreeMap<SongId, Song>,
    undo: Vec<UndoOp>,
    reserved: BTreeSet<String>,
    ffmpeg: Option<Ffmpeg>,
}

impl Library {
    /// Opens (and creates) the layout under `root` (§7.1).
    pub fn open(root: impl Into<PathBuf>) -> Result<Library> {
        let root = root.into();
        for loc in Location::ALL {
            let d = root.join(loc.rel_dir());
            std::fs::create_dir_all(&d).at(&d)?;
        }
        for d in [INPUTS, EXPORTS, CACHE] {
            std::fs::create_dir_all(root.join(d)).at(root.join(d))?;
        }
        Ok(Library {
            root,
            songs: BTreeMap::new(),
            undo: Vec::new(),
            reserved: BTreeSet::new(),
            ffmpeg: None,
        })
    }

    pub fn with_ffmpeg(mut self, f: Ffmpeg) -> Self {
        self.ffmpeg = Some(f);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn inputs_dir(&self) -> PathBuf {
        self.root.join(INPUTS)
    }

    pub fn exports_dir(&self) -> PathBuf {
        self.root.join(EXPORTS)
    }

    pub fn songs(&self) -> impl Iterator<Item = &Song> {
        self.songs.values()
    }

    pub fn get(&self, id: &SongId) -> Option<&Song> {
        self.songs.get(id)
    }

    fn song(&self, id: &SongId) -> Result<&Song> {
        self.songs
            .get(id)
            .ok_or_else(|| Error::Library(format!("unknown song {id}")))
    }

    pub fn snapshot(&self) -> LibraryDelta {
        LibraryDelta {
            full: true,
            upserted: self.songs.values().cloned().collect(),
            removed: vec![],
        }
    }

    /// Removes leftover `*.part` staging folders (§6.1).
    pub fn cleanup_partials(&self) -> Result<usize> {
        let dir = self.root.join(UNREVIEWED);
        let mut n = 0;
        for e in std::fs::read_dir(&dir).at(&dir)? {
            let e = e.at(&dir)?;
            if e.file_name().to_string_lossy().ends_with(PART_SUFFIX) && e.path().is_dir() {
                std::fs::remove_dir_all(e.path()).at(e.path())?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// All stems in use in any location (plus reservations).
    fn stem_taken(&self, stem: &str) -> bool {
        self.reserved.contains(stem)
            || Location::ALL.iter().any(|l| {
                let d = self.root.join(l.rel_dir());
                d.join(stem).exists() || d.join(format!("{stem}{PART_SUFFIX}")).exists()
            })
    }

    /// Seeds used by a run name anywhere in the library (for the collision check, §5.1.1).
    pub fn seeds_for(&self, name: &str) -> BTreeSet<u32> {
        seeds_for(self.songs.values(), name)
    }

    // -----------------------------------------------------------------------------------
    // Scanning

    fn index_path(&self) -> PathBuf {
        self.root.join(CACHE).join("index.ron")
    }

    fn load_index(&self) -> HashMap<PathBuf, IndexEntry> {
        let Ok(text) = std::fs::read_to_string(self.index_path()) else {
            return HashMap::new();
        };
        match from_ron::<IndexFile>(&text) {
            Ok(f) => f.entries.into_iter().map(|e| (e.mp3.clone(), e)).collect(),
            Err(e) => {
                tracing::warn!("ignoring library index: {e}");
                HashMap::new()
            }
        }
    }

    fn save_index(&self) {
        let entries = self
            .songs
            .values()
            .filter_map(|s| {
                Some(IndexEntry {
                    mp3: s.mp3.strip_prefix(&self.root).ok()?.to_path_buf(),
                    mtime: mtime_key(s.modified),
                    size: s.size,
                    id: s.id.0.clone(),
                    meta: serde_json::to_string(&s.meta).ok()?,
                    wav_mtime: s
                        .wav
                        .as_ref()
                        .and_then(|w| std::fs::metadata(w).ok()?.modified().ok())
                        .map(mtime_key),
                    wav_ok: s.wav_ok,
                })
            })
            .collect();
        match to_ron(&IndexFile { entries }) {
            Ok(text) => {
                if let Err(e) = fsutil::write_atomic(&self.index_path(), text.as_bytes()) {
                    tracing::warn!("saving library index: {e}");
                }
            }
            Err(e) => tracing::warn!("serializing library index: {e}"),
        }
    }

    /// Full rescan. Uses `.cache/index.ron` for entries whose mtime and size are unchanged.
    pub fn scan(&mut self) -> Result<LibraryDelta> {
        let index = self.load_index();
        let by_path: HashMap<PathBuf, SongId> = self
            .songs
            .values()
            .map(|s| (s.mp3.clone(), s.id.clone()))
            .collect();
        let mut found = BTreeMap::new();
        for loc in Location::ALL {
            let dir = self.root.join(loc.rel_dir());
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let path = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || name.ends_with(PART_SUFFIX) {
                    continue;
                }
                let (dir_opt, mp3) = if path.is_dir() {
                    match find_mp3(&path, &name) {
                        Some(m) => (Some(path.clone()), m),
                        None => continue,
                    }
                } else if is_mp3(&path) {
                    (None, path.clone())
                } else {
                    continue;
                };
                match self.load_song(&index, &by_path, loc, dir_opt, mp3) {
                    Ok(song) => {
                        found.insert(song.id.clone(), song);
                    }
                    Err(e) => tracing::warn!("skipping {}: {e}", path.display()),
                }
            }
        }
        let removed = self
            .songs
            .keys()
            .filter(|k| !found.contains_key(*k))
            .cloned()
            .collect();
        self.songs = found;
        self.save_index();
        Ok(LibraryDelta {
            full: true,
            upserted: self.songs.values().cloned().collect(),
            removed,
        })
    }

    fn load_song(
        &self,
        index: &HashMap<PathBuf, IndexEntry>,
        by_path: &HashMap<PathBuf, SongId>,
        location: Location,
        dir: Option<PathBuf>,
        mp3: PathBuf,
    ) -> Result<Song> {
        let md = std::fs::metadata(&mp3).at(&mp3)?;
        let modified = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let size = md.len();
        let stem = match &dir {
            Some(d) => d.file_name().unwrap().to_string_lossy().into_owned(),
            None => fsutil::file_stem(&mp3),
        };
        let wav = dir.as_ref().and_then(|d| find_wav(d, &stem));
        let wav_mtime = wav
            .as_ref()
            .and_then(|w| std::fs::metadata(w).ok()?.modified().ok())
            .map(mtime_key);
        let rel = mp3.strip_prefix(&self.root).unwrap_or(&mp3).to_path_buf();
        let cached = index
            .get(&rel)
            .filter(|e| e.mtime == mtime_key(modified) && e.size == size);
        let meta = match cached.and_then(|e| serde_json::from_str::<SongMeta>(&e.meta).ok()) {
            Some(m) => m,
            None => media::read_meta(&mp3)?,
        };
        let wav_ok = match (&wav, meta.recipe.as_ref()) {
            (Some(w), Some(r)) => match cached.filter(|e| e.wav_mtime == wav_mtime) {
                Some(e) => e.wav_ok,
                None => Some(fsutil::sha256_file(w)? == r.output.wav_sha256),
            },
            _ => None,
        };
        let id = by_path
            .get(&mp3)
            .cloned()
            .or_else(|| meta.recipe.as_ref().map(|r| SongId(r.song_id.clone())))
            .or_else(|| cached.map(|e| SongId(e.id.clone())))
            .unwrap_or_else(|| SongId(ulid::Ulid::generate().to_string()));
        Ok(Song {
            id,
            stem,
            dir,
            mp3,
            wav,
            location,
            meta,
            wav_ok,
            modified,
            size,
        })
    }

    /// Reloads one song from disk after a change.
    fn refresh(
        &mut self,
        id: &SongId,
        location: Location,
        dir: Option<PathBuf>,
        mp3: PathBuf,
    ) -> Result<Song> {
        let by_path = HashMap::from([(mp3.clone(), id.clone())]);
        let song = self.load_song(&HashMap::new(), &by_path, location, dir, mp3)?;
        self.songs.insert(id.clone(), song.clone());
        Ok(song)
    }

    fn one(song: Song) -> LibraryDelta {
        LibraryDelta {
            full: false,
            upserted: vec![song],
            removed: vec![],
        }
    }

    // -----------------------------------------------------------------------------------
    // Staging new songs (§6.1)

    /// Reserves a collision-free stem and creates `unreviewed/<stem>.part/`.
    pub fn reserve(&mut self, stem: &str) -> Result<(String, PathBuf)> {
        let unrev = self.root.join(UNREVIEWED);
        let final_stem = fsutil::unique_stem(&unrev, stem, |s| self.stem_taken(s));
        self.reserve_exact(final_stem)
    }

    /// For *Regenerate*: `<stem>-r2`, `-r3`, … next to the original (§6.2).
    pub fn reserve_regen(&mut self, stem: &str) -> Result<(String, PathBuf)> {
        let s = (2u32..)
            .map(|i| format!("{stem}-r{i}"))
            .find(|s| !self.stem_taken(s))
            .expect("unbounded");
        self.reserve_exact(s)
    }

    fn reserve_exact(&mut self, stem: String) -> Result<(String, PathBuf)> {
        let part = self
            .root
            .join(UNREVIEWED)
            .join(format!("{stem}{PART_SUFFIX}"));
        std::fs::create_dir_all(&part).at(&part)?;
        self.reserved.insert(stem.clone());
        Ok((stem, part))
    }

    /// Atomically renames the staging folder into place and adds the song.
    pub fn commit(&mut self, stem: &str, part: &Path) -> Result<LibraryDelta> {
        let dest = self.root.join(UNREVIEWED).join(stem);
        self.reserved.remove(stem);
        std::fs::rename(part, &dest).at(part)?;
        let mp3 = find_mp3(&dest, stem)
            .ok_or_else(|| Error::Library(format!("{} has no mp3", dest.display())))?;
        let song = self.load_song(
            &HashMap::new(),
            &HashMap::new(),
            Location::Unreviewed,
            Some(dest),
            mp3,
        )?;
        self.songs.insert(song.id.clone(), song.clone());
        self.save_index();
        Ok(Self::one(song))
    }

    pub fn abandon(&mut self, stem: &str, part: &Path) {
        self.reserved.remove(stem);
        let _ = std::fs::remove_dir_all(part);
    }

    // -----------------------------------------------------------------------------------
    // Operations (§7.2)

    pub fn apply(&mut self, cmd: &LibraryCommand) -> Result<LibraryDelta> {
        match cmd {
            LibraryCommand::Rescan => self.scan(),
            LibraryCommand::Rate(id, r) => self.rate(id, *r),
            LibraryCommand::Unreview(id) => self.unreview(id),
            LibraryCommand::SetTitle(id, t) => self.set_title(id, t),
            LibraryCommand::Rename(id, n) => self.rename(id, n),
            LibraryCommand::SetTags(id, t) => self.set_tags(id, t.clone()),
            LibraryCommand::SetNotes(id, n) => self.set_notes(id, n),
            LibraryCommand::Delete(id) => self.delete(id),
            LibraryCommand::Export {
                ids,
                dest,
                format,
                strip_metadata,
            } => {
                self.export(ids, dest, *format, *strip_metadata)?;
                Ok(LibraryDelta::default())
            }
            LibraryCommand::Undo => self.undo(),
        }
    }

    fn edit_meta(&mut self, id: &SongId, f: impl FnOnce(&mut SongMeta)) -> Result<LibraryDelta> {
        let song = self.song(id)?.clone();
        let mut meta = song.meta.clone();
        f(&mut meta);
        media::write_meta(&song.mp3, &meta)?;
        let s = self.refresh(id, song.location, song.dir.clone(), song.mp3.clone())?;
        self.save_index();
        Ok(Self::one(s))
    }

    pub fn set_title(&mut self, id: &SongId, title: &str) -> Result<LibraryDelta> {
        let t = title.trim().to_string();
        self.edit_meta(id, |m| m.title = (!t.is_empty()).then_some(t))
    }

    pub fn set_tags(&mut self, id: &SongId, tags: Vec<String>) -> Result<LibraryDelta> {
        let mut seen = BTreeSet::new();
        let tags: Vec<String> = tags
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && seen.insert(t.clone()))
            .collect();
        self.edit_meta(id, |m| m.tags = tags)
    }

    pub fn set_notes(&mut self, id: &SongId, notes: &str) -> Result<LibraryDelta> {
        let n = notes.to_string();
        self.edit_meta(id, |m| m.comment = (!n.trim().is_empty()).then_some(n))
    }

    /// All tags in the library, for autocomplete.
    pub fn all_tags(&self) -> BTreeSet<String> {
        self.songs
            .values()
            .flat_map(|s| s.meta.tags.iter().cloned())
            .collect()
    }

    /// Wraps a loose MP3 in a folder named after its stem.
    fn wrap_loose(&mut self, id: &SongId) -> Result<()> {
        let song = self.song(id)?.clone();
        if song.dir.is_some() {
            return Ok(());
        }
        let parent = song.mp3.parent().unwrap().to_path_buf();
        let stem = fsutil::unique_stem(&parent, &song.stem, |s| {
            s != song.stem && self.stem_taken(s)
        });
        let dir = parent.join(&stem);
        std::fs::create_dir(&dir).at(&dir)?;
        let mp3 = dir.join(format!("{stem}.mp3"));
        std::fs::rename(&song.mp3, &mp3).at(&song.mp3)?;
        self.refresh(id, song.location, Some(dir), mp3)?;
        Ok(())
    }

    fn move_to(
        &mut self,
        id: &SongId,
        to_loc: Location,
        rating: Option<Rating>,
        record: bool,
    ) -> Result<LibraryDelta> {
        self.wrap_loose(id)?;
        let song = self.song(id)?.clone();
        let from = song.dir.clone().expect("wrapped");
        let prev_rating = song.meta.rating;
        let mut meta = song.meta.clone();
        meta.rating = rating;
        if meta != song.meta {
            media::write_meta(&song.mp3, &meta)?;
        }
        let to = if song.location == to_loc {
            from.clone()
        } else {
            let dest_dir = self.root.join(to_loc.rel_dir());
            let stem = fsutil::unique_stem(&dest_dir, &song.stem, |_| false);
            if stem != song.stem {
                // an identically named folder already sits there: rename files to match
                self.rename_files(&from, &song.stem, &stem)?;
            }
            let to = dest_dir.join(&stem);
            if let Err(e) = fsutil::move_dir(&from, &to) {
                if meta != song.meta {
                    let _ = media::write_meta(&song.mp3, &song.meta);
                }
                return Err(e);
            }
            to
        };
        if record {
            self.undo.push(UndoOp::Move {
                id: id.clone(),
                from: from.clone(),
                to: to.clone(),
                prev_rating,
                prev_location: song.location,
            });
        }
        let stem = to.file_name().unwrap().to_string_lossy().into_owned();
        let mp3 = find_mp3(&to, &stem).ok_or_else(|| Error::Library("mp3 vanished".into()))?;
        let s = self.refresh(id, to_loc, Some(to), mp3)?;
        self.save_index();
        Ok(Self::one(s))
    }

    /// Writes the rating atom and moves the folder to `reviewed/<rating>/`.
    pub fn rate(&mut self, id: &SongId, rating: Rating) -> Result<LibraryDelta> {
        self.move_to(id, Location::Reviewed(rating), Some(rating), true)
    }

    /// Moves back to `unreviewed/` and clears the rating atom.
    pub fn unreview(&mut self, id: &SongId) -> Result<LibraryDelta> {
        self.move_to(id, Location::Unreviewed, None, true)
    }

    fn rename_files(&self, dir: &Path, old: &str, new: &str) -> Result<Vec<(PathBuf, PathBuf)>> {
        let mut done = Vec::new();
        let rd = std::fs::read_dir(dir).at(dir)?;
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if !p.is_file() || fsutil::file_stem(&p) != old {
                continue;
            }
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            let q = dir.join(format!("{new}.{ext}"));
            if let Err(err) = std::fs::rename(&p, &q) {
                for (a, b) in done.iter().rev() {
                    let _ = std::fs::rename(b, a);
                }
                return Err(Error::io(&p, err));
            }
            done.push((p, q));
        }
        Ok(done)
    }

    fn rename_stem(
        &mut self,
        id: &SongId,
        new_stem: &str,
        title: Option<Option<String>>,
    ) -> Result<(PathBuf, String, Option<String>)> {
        self.wrap_loose(id)?;
        let song = self.song(id)?.clone();
        let dir = song.dir.clone().unwrap();
        let parent = dir.parent().unwrap().to_path_buf();
        let old_title = song.meta.title.clone();
        if let Some(t) = title {
            let mut meta = song.meta.clone();
            meta.title = t;
            media::write_meta(&song.mp3, &meta)?;
        }
        // files first, then the folder (§7.2)
        let done = self.rename_files(&dir, &song.stem, new_stem)?;
        let new_dir = parent.join(new_stem);
        if let Err(e) = std::fs::rename(&dir, &new_dir) {
            for (a, b) in done.iter().rev() {
                let _ = std::fs::rename(b, a);
            }
            return Err(Error::io(&dir, e));
        }
        let mp3 =
            find_mp3(&new_dir, new_stem).ok_or_else(|| Error::Library("mp3 vanished".into()))?;
        self.refresh(id, song.location, Some(new_dir), mp3)?;
        Ok((parent, song.stem, old_title))
    }

    /// Renames folder and both files to `<new name>-<seed>`; the `-<seed>` suffix (and any
    /// take suffix like `-r2`) is kept. Collisions get `-2`, `-3`, … (§7.2).
    pub fn rename(&mut self, id: &SongId, new_name: &str) -> Result<LibraryDelta> {
        let name = RunName::parse(new_name)?;
        let song = self.song(id)?.clone();
        let seed = song
            .seed()
            .ok_or_else(|| Error::Library(format!("{}: no seed to keep", song.stem)))?;
        let old_prefix = format!("{}-{seed}", song.run_name().unwrap_or_default());
        let suffix = song.stem.strip_prefix(&old_prefix).unwrap_or("");
        let wanted = format!("{}{suffix}", name.stem(seed));
        if wanted == song.stem {
            return Ok(LibraryDelta::default());
        }
        let parent = song.unit_path().parent().unwrap().to_path_buf();
        let new_stem =
            fsutil::unique_stem(&parent, &wanted, |s| s != song.stem && self.stem_taken(s));
        // keep the title in sync when it was the default (= old stem)
        let title = (song.meta.title.as_deref().is_none_or(|t| t == song.stem))
            .then(|| Some(new_stem.clone()));
        let (parent, old_stem, old_title) = self.rename_stem(id, &new_stem, title)?;
        self.undo.push(UndoOp::Rename {
            id: id.clone(),
            parent,
            old_stem,
            new_stem,
            old_title,
        });
        self.save_index();
        Ok(Self::one(self.song(id)?.clone()))
    }

    /// Moves the song folder to the system trash, never a hard delete.
    pub fn delete(&mut self, id: &SongId) -> Result<LibraryDelta> {
        let song = self.song(id)?.clone();
        trash::delete(song.unit_path()).map_err(|e| Error::Library(format!("trash: {e}")))?;
        self.songs.remove(id);
        self.undo.retain(|op| match op {
            UndoOp::Move { id: i, .. } | UndoOp::Rename { id: i, .. } => i != id,
        });
        self.save_index();
        Ok(LibraryDelta {
            full: false,
            upserted: vec![],
            removed: vec![id.clone()],
        })
    }

    /// Undoes the last rate/move/rename.
    pub fn undo(&mut self) -> Result<LibraryDelta> {
        let Some(op) = self.undo.pop() else {
            return Ok(LibraryDelta::default());
        };
        match op {
            UndoOp::Move {
                id,
                from,
                to,
                prev_rating,
                prev_location,
            } => {
                let song = self.song(&id)?.clone();
                if song.dir.as_deref() != Some(to.as_path()) {
                    return Err(Error::Library("song moved since; cannot undo".into()));
                }
                let mut meta = song.meta.clone();
                meta.rating = prev_rating;
                media::write_meta(&song.mp3, &meta)?;
                if from != to {
                    let from_stem = from.file_name().unwrap().to_string_lossy().into_owned();
                    if from_stem != song.stem {
                        self.rename_files(&to, &song.stem, &from_stem)?;
                    }
                    fsutil::move_dir(&to, &from)?;
                }
                let stem = from.file_name().unwrap().to_string_lossy().into_owned();
                let mp3 =
                    find_mp3(&from, &stem).ok_or_else(|| Error::Library("mp3 vanished".into()))?;
                let s = self.refresh(&id, prev_location, Some(from), mp3)?;
                self.save_index();
                Ok(Self::one(s))
            }
            UndoOp::Rename {
                id,
                parent,
                old_stem,
                new_stem,
                old_title,
            } => {
                let song = self.song(&id)?.clone();
                if song.dir.as_deref() != Some(parent.join(&new_stem).as_path()) {
                    return Err(Error::Library("song moved since; cannot undo".into()));
                }
                self.rename_stem(&id, &old_stem, Some(old_title))?;
                self.save_index();
                Ok(Self::one(self.song(&id)?.clone()))
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Copies songs to `dest` (§7.2). See [`export_songs`].
    pub fn export(
        &self,
        ids: &[SongId],
        dest: &Path,
        format: ExportFormat,
        strip: bool,
    ) -> Result<Vec<PathBuf>> {
        let songs = self.export_plan(ids)?;
        export_songs(&songs, self.ffmpeg.as_ref(), dest, format, strip)
    }

    /// The songs to export, cloned so encoding can run without holding the library.
    pub fn export_plan(&self, ids: &[SongId]) -> Result<Vec<Song>> {
        ids.iter().map(|id| self.song(id).cloned()).collect()
    }

    pub fn ffmpeg(&self) -> Option<&Ffmpeg> {
        self.ffmpeg.as_ref()
    }
}

/// Copies songs to `dest` (§7.2). MP3 is the library file itself; WAV is copied from the
/// master; FLAC is encoded from the master. Without a master, the MP3 is decoded instead.
pub fn export_songs(
    songs: &[Song],
    ffmpeg: Option<&Ffmpeg>,
    dest: &Path,
    format: ExportFormat,
    strip: bool,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dest).at(dest)?;
    let mut out = Vec::new();
    for song in songs {
        let ext = format.extension();
        let first = dest.join(format!("{}.{ext}", song.stem));
        let target = if !first.exists() {
            first
        } else {
            (2u32..)
                .map(|n| dest.join(format!("{}-{n}.{ext}", song.stem)))
                .find(|p| !p.exists())
                .expect("unbounded")
        };
        let ff = || ffmpeg.ok_or_else(|| Error::Ffmpeg("ffmpeg not configured".into()));
        match format {
            ExportFormat::Mp3 => {
                fsutil::copy_file(&song.mp3, &target)?;
                if strip {
                    media::strip_meta(&target)?;
                }
            }
            ExportFormat::Wav => match &song.wav {
                Some(w) => fsutil::copy_file(w, &target)?,
                None => ff()?.export(&song.mp3, &target, format, true)?,
            },
            ExportFormat::Flac => {
                let src = song.wav.as_ref().unwrap_or(&song.mp3);
                ff()?.export(src, &target, format, strip)?;
            }
        }
        out.push(target);
    }
    Ok(out)
}

pub fn seeds_for<'a>(songs: impl IntoIterator<Item = &'a Song>, name: &str) -> BTreeSet<u32> {
    songs
        .into_iter()
        .filter(|s| s.run_name().as_deref() == Some(name))
        .filter_map(|s| s.seed())
        .collect()
}

fn is_mp3(p: &Path) -> bool {
    fsutil::extension_lower(p).as_deref() == Some("mp3")
}

/// `<stem>.mp3` if present, else any MP3 in the folder.
fn find_mp3(dir: &Path, stem: &str) -> Option<PathBuf> {
    let p = dir.join(format!("{stem}.mp3"));
    if p.is_file() {
        return Some(p);
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_mp3(p))
        .collect();
    v.sort();
    v.into_iter().next()
}

fn find_wav(dir: &Path, stem: &str) -> Option<PathBuf> {
    let p = dir.join(format!("{stem}.wav"));
    if p.is_file() {
        return Some(p);
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && fsutil::extension_lower(p).as_deref() == Some("wav"))
        .collect();
    v.sort();
    v.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::tests::sample_recipe;
    use pretty_assertions::assert_eq;

    const TINY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/tiny.mp3");
    const WAV: &[u8] = include_bytes!("../../../tests/fixtures/one_second.wav");

    /// Creates `<loc>/<stem>/<stem>.mp3 + .wav` with a recipe for `name`/`seed`.
    fn add_song(lib_root: &Path, loc: Location, name: &str, seed: u32) -> PathBuf {
        let stem = format!("{name}-{seed}");
        let dir = lib_root.join(loc.rel_dir()).join(&stem);
        std::fs::create_dir_all(&dir).unwrap();
        let mp3 = dir.join(format!("{stem}.mp3"));
        std::fs::copy(TINY, &mp3).unwrap();
        std::fs::write(dir.join(format!("{stem}.wav")), WAV).unwrap();
        let mut r = sample_recipe();
        r.song_id = ulid::Ulid::generate().to_string();
        r.run.name = name.into();
        r.run.seed = seed;
        r.output.wav_sha256 = fsutil::sha256_hex(WAV);
        let meta = SongMeta {
            title: Some(stem.clone()),
            recipe: Some(r),
            ..Default::default()
        };
        media::write_meta(&mp3, &meta).unwrap();
        dir
    }

    fn lib() -> (tempfile::TempDir, Library) {
        let d = tempfile::tempdir().unwrap();
        let l = Library::open(d.path()).unwrap();
        (d, l)
    }

    fn id_of(l: &Library, stem: &str) -> SongId {
        l.songs()
            .find(|s| s.stem == stem)
            .unwrap_or_else(|| panic!("no {stem}"))
            .id
            .clone()
    }

    fn assert_unit(song: &Song) {
        let dir = song.dir.as_ref().unwrap();
        assert_eq!(dir.file_name().unwrap().to_string_lossy(), song.stem);
        assert_eq!(song.mp3, dir.join(format!("{}.mp3", song.stem)));
        if let Some(w) = &song.wav {
            assert_eq!(w, &dir.join(format!("{}.wav", song.stem)));
        }
    }

    #[test]
    fn layout_is_created() {
        let (d, _l) = lib();
        for p in [
            "unreviewed",
            "reviewed/good",
            "reviewed/neutral",
            "reviewed/bad",
            "inputs",
            "exports",
            ".cache",
        ] {
            assert!(d.path().join(p).is_dir(), "{p}");
        }
    }

    #[test]
    fn scan_finds_songs_and_flags() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        let nowav = add_song(d.path(), Location::Reviewed(Rating::Good), "a", 2);
        std::fs::remove_file(nowav.join("a-2.wav")).unwrap();
        let bad = add_song(d.path(), Location::Unreviewed, "a", 3);
        std::fs::write(bad.join("a-3.wav"), b"RIFF-corrupted").unwrap();
        std::fs::copy(TINY, d.path().join("reviewed/bad/loose-9.mp3")).unwrap();
        std::fs::create_dir_all(d.path().join("unreviewed/x-1.part")).unwrap();
        std::fs::create_dir_all(d.path().join("unreviewed/empty")).unwrap();

        let delta = l.scan().unwrap();
        assert!(delta.full);
        assert_eq!(l.songs().count(), 4);
        let a1 = l.get(&id_of(&l, "a-1")).unwrap();
        assert_eq!(a1.wav_ok, Some(true));
        assert_eq!(a1.seed(), Some(1));
        let a2 = l.get(&id_of(&l, "a-2")).unwrap();
        assert!(a2.wav.is_none(), "no master");
        assert_eq!(a2.rating(), Some(Rating::Good));
        assert_eq!(
            l.get(&id_of(&l, "a-3")).unwrap().wav_ok,
            Some(false),
            "hash mismatch flagged"
        );
        let loose = l.get(&id_of(&l, "loose-9")).unwrap();
        assert!(loose.dir.is_none());
        assert_eq!(loose.seed(), Some(9));
        assert_eq!(l.seeds_for("a"), BTreeSet::from([1, 2, 3]));

        assert_eq!(l.cleanup_partials().unwrap(), 1);
        assert!(!d.path().join("unreviewed/x-1.part").exists());
    }

    #[test]
    fn index_rebuilds_after_external_changes() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        l.scan().unwrap();
        assert!(d.path().join(".cache/index.ron").exists());
        let id = id_of(&l, "a-1");
        // external edit: change the title with a different tool → mtime/size change
        let mp3 = d.path().join("unreviewed/a-1/a-1.mp3");
        let mut m = media::read_meta(&mp3).unwrap();
        m.title = Some("edited elsewhere, a much longer title".into());
        media::write_meta(&mp3, &m).unwrap();
        // external move
        std::fs::rename(
            d.path().join("unreviewed/a-1"),
            d.path().join("reviewed/bad/a-1"),
        )
        .unwrap();
        let mut l2 = Library::open(d.path()).unwrap();
        l2.scan().unwrap();
        let s = l2.songs().next().unwrap();
        assert_eq!(s.title(), "edited elsewhere, a much longer title");
        assert_eq!(s.location, Location::Reviewed(Rating::Bad));
        assert_eq!(s.id, id, "recipe song_id is stable");
        // external delete
        std::fs::remove_dir_all(d.path().join("reviewed/bad/a-1")).unwrap();
        let delta = l2.scan().unwrap();
        assert_eq!(delta.removed, vec![id]);
    }

    #[test]
    fn rate_rerate_unreview_undo() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        l.scan().unwrap();
        let id = id_of(&l, "a-1");

        l.rate(&id, Rating::Good).unwrap();
        let s = l.get(&id).unwrap().clone();
        assert_eq!(s.location, Location::Reviewed(Rating::Good));
        assert_eq!(
            s.dir.as_deref(),
            Some(d.path().join("reviewed/good/a-1").as_path())
        );
        assert_eq!(media::read_meta(&s.mp3).unwrap().rating, Some(Rating::Good));
        assert!(s.wav.is_some());
        assert_unit(&s);
        assert!(!d.path().join("unreviewed/a-1").exists());

        l.rate(&id, Rating::Bad).unwrap();
        assert_eq!(
            l.get(&id).unwrap().location,
            Location::Reviewed(Rating::Bad)
        );
        l.unreview(&id).unwrap();
        let s = l.get(&id).unwrap().clone();
        assert_eq!(s.location, Location::Unreviewed);
        assert_eq!(media::read_meta(&s.mp3).unwrap().rating, None);

        l.undo().unwrap(); // back to bad
        assert_eq!(
            l.get(&id).unwrap().location,
            Location::Reviewed(Rating::Bad)
        );
        assert_eq!(
            media::read_meta(&l.get(&id).unwrap().mp3).unwrap().rating,
            Some(Rating::Bad)
        );
        l.undo().unwrap(); // back to good
        l.undo().unwrap(); // back to unreviewed
        let s = l.get(&id).unwrap();
        assert_eq!(s.location, Location::Unreviewed);
        assert_eq!(media::read_meta(&s.mp3).unwrap().rating, None);
        assert!(!l.can_undo());
    }

    #[test]
    fn rating_into_occupied_folder_gets_suffix() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        add_song(d.path(), Location::Reviewed(Rating::Good), "a", 1);
        l.scan().unwrap();
        let id = l
            .songs()
            .find(|s| s.location == Location::Unreviewed)
            .unwrap()
            .id
            .clone();
        l.rate(&id, Rating::Good).unwrap();
        let s = l.get(&id).unwrap();
        assert_eq!(s.stem, "a-1-2");
        assert_unit(s);
        l.undo().unwrap();
        let s = l.get(&id).unwrap();
        assert_eq!(s.stem, "a-1");
        assert_unit(s);
    }

    #[test]
    fn rename_keeps_seed_handles_collisions_and_undo() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        add_song(d.path(), Location::Reviewed(Rating::Good), "b", 1);
        l.scan().unwrap();
        let id = id_of(&l, "a-1");
        assert!(l.rename(&id, "bad/name").is_err());

        l.rename(&id, "b").unwrap();
        let s = l.get(&id).unwrap().clone();
        assert_eq!(s.stem, "b-1-2", "b-1 exists in reviewed/good");
        assert_unit(&s);
        assert_eq!(s.title(), "b-1-2", "default title follows the rename");

        l.rename(&id, "c d").unwrap();
        let s = l.get(&id).unwrap().clone();
        assert_eq!(s.stem, "c d-1");
        assert_unit(&s);

        l.undo().unwrap();
        l.undo().unwrap();
        let s = l.get(&id).unwrap().clone();
        assert_eq!(s.stem, "a-1");
        assert_eq!(s.title(), "a-1");
        assert_unit(&s);
    }

    #[test]
    fn rename_keeps_custom_title() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        l.scan().unwrap();
        let id = id_of(&l, "a-1");
        l.set_title(&id, "My Favourite").unwrap();
        l.rename(&id, "z").unwrap();
        assert_eq!(l.get(&id).unwrap().title(), "My Favourite");
    }

    #[test]
    fn loose_mp3_gets_wrapped_on_move() {
        let (d, mut l) = lib();
        std::fs::copy(TINY, d.path().join("reviewed/bad/loose-9.mp3")).unwrap();
        l.scan().unwrap();
        let id = id_of(&l, "loose-9");
        l.rate(&id, Rating::Good).unwrap();
        let s = l.get(&id).unwrap();
        assert_eq!(
            s.dir.as_deref(),
            Some(d.path().join("reviewed/good/loose-9").as_path())
        );
        assert_unit(s);
        assert!(!d.path().join("reviewed/bad/loose-9.mp3").exists());
    }

    #[test]
    fn tags_notes_title() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        l.scan().unwrap();
        let id = id_of(&l, "a-1");
        l.set_tags(&id, vec!["x".into(), " y ".into(), "x".into(), "".into()])
            .unwrap();
        l.set_notes(&id, "note").unwrap();
        l.set_title(&id, "T").unwrap();
        let m = media::read_meta(&l.get(&id).unwrap().mp3).unwrap();
        assert_eq!(m.tags, vec!["x", "y"]);
        assert_eq!(m.comment.as_deref(), Some("note"));
        assert_eq!(m.title.as_deref(), Some("T"));
        assert_eq!(
            l.all_tags(),
            BTreeSet::from(["x".to_string(), "y".to_string()])
        );
    }

    #[test]
    fn staging_commit_and_collisions() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Reviewed(Rating::Good), "a", 1);
        l.scan().unwrap();
        let (stem, part) = l.reserve("a-1").unwrap();
        assert_eq!(stem, "a-1-2", "exists in reviewed/good");
        let (stem2, _p2) = l.reserve("a-1").unwrap();
        assert_eq!(stem2, "a-1-3", "reservations count");
        std::fs::copy(TINY, part.join(format!("{stem}.mp3"))).unwrap();
        let delta = l.commit(&stem, &part).unwrap();
        assert_eq!(delta.upserted[0].stem, "a-1-2");
        assert!(d.path().join("unreviewed/a-1-2/a-1-2.mp3").exists());
        assert!(!part.exists());
        let (r, _) = l.reserve_regen("a-1").unwrap();
        assert_eq!(r, "a-1-r2");
    }

    #[test]
    fn export_wav_is_master_copy() {
        let (d, mut l) = lib();
        add_song(d.path(), Location::Unreviewed, "a", 1);
        l.scan().unwrap();
        let id = id_of(&l, "a-1");
        let dest = d.path().join("out");
        let out = l
            .export(std::slice::from_ref(&id), &dest, ExportFormat::Wav, false)
            .unwrap();
        assert_eq!(std::fs::read(&out[0]).unwrap(), WAV);
        let out2 = l
            .export(std::slice::from_ref(&id), &dest, ExportFormat::Wav, false)
            .unwrap();
        assert_eq!(out2[0], dest.join("a-1-2.wav"));
        let mp3 = l.export(&[id], &dest, ExportFormat::Mp3, true).unwrap();
        assert!(
            media::read_meta(&mp3[0]).unwrap().recipe.is_none(),
            "stripped"
        );
    }
}
