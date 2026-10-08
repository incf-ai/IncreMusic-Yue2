//! Runs: names, seeds, specs and edits (design §5.1).

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::ModelSpec;
use crate::error::{Error, Result};
use crate::params::GenerationParams;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RunId(pub ulid::Ulid);

impl RunId {
    pub fn new() -> Self {
        RunId(ulid::Ulid::generate())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A validated run name (§5.1.1). The same rules on every OS so files stay portable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RunName(String);

pub const MAX_NAME_LEN: usize = 100;
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

impl RunName {
    pub fn parse(input: &str) -> Result<RunName> {
        let s = input.trim();
        if s.is_empty() {
            return Err(Error::InvalidName("name is required".into()));
        }
        if s.chars().count() > MAX_NAME_LEN {
            return Err(Error::InvalidName(format!(
                "at most {MAX_NAME_LEN} characters"
            )));
        }
        if let Some(c) = s.chars().find(|c| FORBIDDEN.contains(c)) {
            return Err(Error::InvalidName(format!("must not contain `{c}`")));
        }
        if s.chars().any(char::is_control) {
            return Err(Error::InvalidName(
                "must not contain control characters".into(),
            ));
        }
        if s.ends_with('.') {
            return Err(Error::InvalidName("must not end in `.`".into()));
        }
        let base = s
            .split('.')
            .next()
            .unwrap_or(s)
            .trim_end()
            .to_ascii_uppercase();
        if RESERVED.contains(&base.as_str()) {
            return Err(Error::InvalidName(format!(
                "`{base}` is a reserved name on Windows"
            )));
        }
        Ok(RunName(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `<name>-<seed>`: the folder, both files and the MP3 title (§5.1.1).
    pub fn stem(&self, seed: u32) -> String {
        format!("{}-{seed}", self.0)
    }
}

impl TryFrom<String> for RunName {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        RunName::parse(&s)
    }
}

impl From<RunName> for String {
    fn from(n: RunName) -> String {
        n.0
    }
}

impl fmt::Display for RunName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where `params.abc` came from (§5.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AbcSource {
    /// Loaded `.abc` file (copied into the project).
    File { file_name: String, sha256: String },
    /// Any audio → WAV → SheetSage2.
    Transcribed(ReferenceAudio),
    /// Typed or pasted into the editor.
    #[default]
    Manual,
    /// `params.abc == None`; YuE2 generates its own.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceAudio {
    pub file_name: String,
    pub format: String,
    pub sha256: String,
    pub upload_sha256: String,
    pub abc_model: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSpec {
    pub name: RunName,
    pub params: GenerationParams,
    pub start_seed: u32,
    /// `None` → keep going until stopped.
    pub count: Option<u32>,
    pub model: ModelSpec,
    pub abc_source: AbcSource,
}

impl RunSpec {
    /// Planned seeds, capped so an "until stopped" run doesn't enumerate billions.
    pub fn planned_seeds(&self, cap: u32) -> impl Iterator<Item = u32> {
        planned_seeds(self.start_seed, self.count, cap)
    }
}

pub fn planned_seeds(start: u32, count: Option<u32>, cap: u32) -> impl Iterator<Item = u32> {
    let n = count.unwrap_or(cap).min(cap) as u64;
    (start as u64..(start as u64 + n).min(u32::MAX as u64 + 1)).map(|s| s as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Active,
    Paused,
    /// No new seeds; running jobs finish and are kept.
    Stopping,
    Done,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub id: RunId,
    pub spec: RunSpec,
    /// Bumped on every params edit.
    pub revision: u32,
    /// Seed cursor: the next job takes this, then +1.
    pub next_seed: u32,
    /// Jobs handed out so far (retries not counted).
    pub issued: u32,
    pub status: RunStatus,
    /// Set when the cursor would go past `u32::MAX`.
    pub seeds_exhausted: bool,
}

impl RunState {
    pub fn new(id: RunId, spec: RunSpec) -> Self {
        RunState {
            id,
            next_seed: spec.start_seed,
            spec,
            revision: 0,
            issued: 0,
            status: RunStatus::Active,
            seeds_exhausted: false,
        }
    }

    /// True if the cursor can hand out another new seed.
    pub fn has_new_seeds(&self) -> bool {
        !self.seeds_exhausted && self.spec.count.is_none_or(|c| self.issued < c)
    }

    /// Takes the next seed and advances the cursor. Stops at `u32::MAX` (§5.1.2).
    pub fn take_seed(&mut self) -> Option<(u32, u32)> {
        if self.status != RunStatus::Active || !self.has_new_seeds() {
            return None;
        }
        let seed = self.next_seed;
        let index = self.issued;
        self.issued += 1;
        match seed.checked_add(1) {
            Some(n) => self.next_seed = n,
            None => self.seeds_exhausted = true,
        }
        Some((seed, index))
    }

    pub fn apply(&mut self, edit: &RunEdit) {
        match edit {
            RunEdit::Params(p) => {
                if &self.spec.params != p {
                    self.spec.params = p.clone();
                    self.revision += 1;
                }
            }
            RunEdit::Count(c) => self.spec.count = *c,
            RunEdit::NextSeed(s) => {
                self.next_seed = *s;
                self.seeds_exhausted = false;
            }
            RunEdit::AbcSource(src) => self.spec.abc_source = src.clone(),
        }
    }
}

/// Edits allowed while a run is in progress (§5.1.2). The name is never editable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RunEdit {
    Params(GenerationParams),
    Count(Option<u32>),
    NextSeed(u32),
    AbcSource(AbcSource),
}

/// Result of the collision check (§5.1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collision {
    /// Seeds from the planned range that already exist in the library.
    pub seeds: Vec<u32>,
    /// `max existing seed + 1`, if that fits in `u32`.
    pub continue_from: Option<u32>,
}

/// Checks a planned seed range against the seeds already used by this name anywhere in
/// the library. `count: None` checks everything from `start` upward.
pub fn check_collision(
    existing: &BTreeSet<u32>,
    start: u32,
    count: Option<u32>,
) -> Option<Collision> {
    let end = match count {
        Some(c) => (start as u64 + c as u64).min(u32::MAX as u64 + 1),
        None => u32::MAX as u64 + 1,
    };
    let seeds: Vec<u32> = existing
        .range(start..)
        .take_while(|&&s| (s as u64) < end)
        .copied()
        .collect();
    if seeds.is_empty() {
        return None;
    }
    let max = *existing.iter().next_back().unwrap();
    Some(Collision {
        seeds,
        continue_from: max.checked_add(1),
    })
}

/// The next unused seed for a name: `max existing + 1`, or `None` if nothing exists.
pub fn next_free_seed(existing: &BTreeSet<u32>) -> Option<u32> {
    existing.iter().next_back().and_then(|m| m.checked_add(1))
}

/// Splits `<name>-<seed>` (optionally followed by a `-N` or `-rN` suffix) into name and
/// seed. Used for songs without a recipe and for collision checks.
pub fn parse_stem(stem: &str) -> Option<(String, u32)> {
    let mut parts: Vec<&str> = stem.rsplitn(3, '-').collect();
    parts.reverse();
    // try "<name>-<seed>" first, then "<name>-<seed>-<suffix>"
    if let Some((name, seed)) = stem.rsplit_once('-')
        && let Ok(s) = seed.parse::<u32>()
        && !name.is_empty()
        && !seed.starts_with('+')
    {
        // "<name>-<seed>-<n>" is ambiguous with a name ending in "-<digits>"; the
        // recipe resolves it for real songs, so prefer the longer name here.
        return Some((name.to_string(), s));
    }
    if parts.len() == 3 {
        let (name, seed, suffix) = (parts[0], parts[1], parts[2]);
        let suffix_ok = suffix
            .strip_prefix('r')
            .unwrap_or(suffix)
            .parse::<u32>()
            .is_ok();
        if suffix_ok && let Ok(s) = seed.parse::<u32>() {
            return Some((name.to_string(), s));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn name_rules() {
        assert_eq!(
            RunName::parse("  sunny hook ").unwrap().as_str(),
            "sunny hook"
        );
        for bad in [
            "", "   ", "a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\u{7}b",
            "a.", "a\tb",
        ] {
            assert!(RunName::parse(bad).is_err(), "{bad:?} should be rejected");
        }
        for reserved in ["CON", "nul", "Com1", "lpt9", "aux.txt", "PRN "] {
            assert!(RunName::parse(reserved).is_err(), "{reserved:?}");
        }
        assert!(RunName::parse("console").is_ok());
        assert!(RunName::parse("COM10").is_ok());
        assert!(RunName::parse(&"x".repeat(100)).is_ok());
        assert!(RunName::parse(&"x".repeat(101)).is_err());
        assert!(RunName::parse(&"é".repeat(100)).is_ok());
        // trailing space is trimmed, not rejected
        assert_eq!(RunName::parse("abc ").unwrap().as_str(), "abc");
    }

    #[test]
    fn stems() {
        let n = RunName::parse("sunny-hook").unwrap();
        assert_eq!(n.stem(1233), "sunny-hook-1233");
        assert_eq!(
            parse_stem("sunny-hook-1233"),
            Some(("sunny-hook".into(), 1233))
        );
        assert_eq!(parse_stem("a b-7"), Some(("a b".into(), 7)));
        assert_eq!(parse_stem("nohyphen"), None);
        assert_eq!(parse_stem("x-abc"), None);
        assert_eq!(parse_stem("sunny-1233-r2"), Some(("sunny".into(), 1233)));
    }

    fn state(start: u32, count: Option<u32>) -> RunState {
        let spec = RunSpec {
            name: RunName::parse("t").unwrap(),
            params: GenerationParams::default(),
            start_seed: start,
            count,
            model: crate::config::ModelSpec {
                id: "yue2".into(),
                family: "yue2".into(),
                task: "gen".into(),
                mode: "offline".into(),
                path: "/x".into(),
                load_options: Default::default(),
                session_options: Default::default(),
            },
            abc_source: AbcSource::Manual,
        };
        RunState::new(RunId::new(), spec)
    }

    #[test]
    fn seed_cursor() {
        let mut s = state(10, Some(3));
        assert_eq!(s.take_seed(), Some((10, 0)));
        assert_eq!(s.take_seed(), Some((11, 1)));
        assert_eq!(s.take_seed(), Some((12, 2)));
        assert_eq!(s.take_seed(), None);
        s.apply(&RunEdit::Count(Some(4)));
        assert_eq!(s.take_seed(), Some((13, 3)));
        s.apply(&RunEdit::Count(None));
        s.apply(&RunEdit::NextSeed(100));
        assert_eq!(s.take_seed(), Some((100, 4)));
    }

    #[test]
    fn stops_at_u32_max() {
        let mut s = state(u32::MAX - 1, None);
        assert_eq!(s.take_seed(), Some((u32::MAX - 1, 0)));
        assert_eq!(s.take_seed(), Some((u32::MAX, 1)));
        assert_eq!(s.take_seed(), None);
        assert!(!s.has_new_seeds());
    }

    #[test]
    fn param_edits_bump_revision() {
        let mut s = state(0, None);
        let mut p = s.spec.params.clone();
        s.apply(&RunEdit::Params(p.clone()));
        assert_eq!(s.revision, 0, "no-op edit keeps the revision");
        p.style = "new".into();
        s.apply(&RunEdit::Params(p));
        assert_eq!(s.revision, 1);
    }

    #[test]
    fn paused_run_hands_out_nothing() {
        let mut s = state(0, None);
        s.status = RunStatus::Paused;
        assert_eq!(s.take_seed(), None);
    }

    #[test]
    fn collisions() {
        let existing: BTreeSet<u32> = [5, 6, 20].into();
        assert_eq!(check_collision(&existing, 0, Some(5)), None);
        assert_eq!(
            check_collision(&existing, 0, Some(6)),
            Some(Collision {
                seeds: vec![5],
                continue_from: Some(21)
            })
        );
        assert_eq!(
            check_collision(&existing, 6, None),
            Some(Collision {
                seeds: vec![6, 20],
                continue_from: Some(21)
            })
        );
        assert_eq!(check_collision(&existing, 21, None), None);
        assert_eq!(next_free_seed(&existing), Some(21));
        let max: BTreeSet<u32> = [u32::MAX].into();
        assert_eq!(check_collision(&max, 0, None).unwrap().continue_from, None);
        assert_eq!(check_collision(&BTreeSet::new(), 0, None), None);
    }

    #[test]
    fn planned() {
        assert_eq!(
            planned_seeds(1, Some(3), 100).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            planned_seeds(u32::MAX, Some(3), 100).collect::<Vec<_>>(),
            vec![u32::MAX]
        );
        assert_eq!(planned_seeds(0, None, 2).count(), 2);
    }
}
