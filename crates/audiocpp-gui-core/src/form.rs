//! The Generate panel's form and its validation (design §5.1.1, §5.2, §8).

use std::collections::BTreeSet;

use audiocpp_core::abc::{self, AbcSummary};
use audiocpp_core::config::ModelSpec;
use audiocpp_core::params::GenerationParams;
use audiocpp_core::run::{AbcSource, Collision, RunName, RunSpec, check_collision};

/// The ABC source selector (§5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbcChoice {
    LoadFile,
    Transcribe,
    Paste,
    /// Let YuE2 compose; no `abc` field is sent.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenerateForm {
    /// Required; kept after a run starts.
    pub name: String,
    pub seed: String,
    pub count: String,
    pub until_stopped: bool,
    pub params: GenerationParams,
    pub abc_text: String,
    pub abc_choice: AbcChoice,
    pub abc_source: AbcSource,
    pub abc_summary: AbcSummary,
    /// `allow_no_abc` of the loaded preset (or set by "Don't ask again").
    pub allow_no_abc: bool,
    pub advanced_open: bool,
    pub abc_section_open: bool,
    /// The preset file the form came from, if any.
    pub preset_path: Option<std::path::PathBuf>,
}

impl Default for GenerateForm {
    fn default() -> Self {
        GenerateForm {
            name: String::new(),
            seed: "0".into(),
            count: "10".into(),
            until_stopped: false,
            params: GenerationParams::default(),
            abc_text: String::new(),
            // a new form opens with the ABC section expanded; load/transcribe are primary
            abc_choice: AbcChoice::LoadFile,
            abc_source: AbcSource::Manual,
            abc_summary: AbcSummary::default(),
            allow_no_abc: false,
            advanced_open: false,
            abc_section_open: true,
            preset_path: None,
        }
    }
}

/// Why *Start run* is disabled, in the order the user should fix things.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    Name(String),
    Seed(String),
    Count(String),
    Params(String),
    Collision(Collision),
    NoServerModel,
}

impl Blocker {
    pub fn message(&self) -> String {
        match self {
            Blocker::Name(m) => format!("Name: {m}"),
            Blocker::Seed(m) => format!("Starting seed: {m}"),
            Blocker::Count(m) => format!("Count: {m}"),
            Blocker::Params(m) => m.clone(),
            Blocker::Collision(c) => {
                let shown: Vec<String> = c.seeds.iter().take(5).map(u32::to_string).collect();
                let more = if c.seeds.len() > 5 {
                    format!(" (+{} more)", c.seeds.len() - 5)
                } else {
                    String::new()
                };
                format!(
                    "Songs with seed {}{more} already exist for this name",
                    shown.join(", ")
                )
            }
            Blocker::NoServerModel => "no model configured".into(),
        }
    }
}

pub fn parse_seed(s: &str) -> Result<u32, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("required".into());
    }
    t.parse::<u32>()
        .map_err(|_| format!("must be a whole number from 0 to {}", u32::MAX))
}

impl GenerateForm {
    pub fn name_result(&self) -> Result<RunName, String> {
        RunName::parse(&self.name).map_err(|e| match e {
            audiocpp_core::Error::InvalidName(m) => m,
            other => other.to_string(),
        })
    }

    pub fn seed_result(&self) -> Result<u32, String> {
        parse_seed(&self.seed)
    }

    pub fn count_result(&self) -> Result<Option<u32>, String> {
        if self.until_stopped {
            return Ok(None);
        }
        match self.count.trim().parse::<u32>() {
            Ok(0) => Err("must be at least 1".into()),
            Ok(n) => Ok(Some(n)),
            Err(_) => Err("must be a whole number (or choose “until stopped”)".into()),
        }
    }

    /// The ABC that would be sent: `None` when the choice is *None* or the editor is empty.
    pub fn effective_abc(&self) -> Option<String> {
        if self.abc_choice == AbcChoice::None || self.abc_text.trim().is_empty() {
            None
        } else {
            Some(self.abc_text.clone())
        }
    }

    pub fn effective_source(&self) -> AbcSource {
        match self.effective_abc() {
            None => AbcSource::None,
            Some(_) => self.abc_source.clone(),
        }
    }

    pub fn collision(&self, existing: &BTreeSet<u32>) -> Option<Collision> {
        let seed = self.seed_result().ok()?;
        let count = self.count_result().ok()?;
        check_collision(existing, seed, count)
    }

    /// First reason *Start run* is disabled, if any.
    pub fn blocker(&self, existing_seeds: &BTreeSet<u32>) -> Option<Blocker> {
        if let Err(m) = self.name_result() {
            return Some(Blocker::Name(m));
        }
        if let Err(m) = self.seed_result() {
            return Some(Blocker::Seed(m));
        }
        if let Err(m) = self.count_result() {
            return Some(Blocker::Count(m));
        }
        let mut p = self.params.clone();
        p.abc = self.effective_abc();
        if let Some(m) = p.validate().into_iter().next() {
            return Some(Blocker::Params(m));
        }
        self.collision(existing_seeds).map(Blocker::Collision)
    }

    /// True when starting needs the "No ABC melody" confirmation (§5.2): no ABC, not
    /// chosen on purpose, and the preset doesn't allow it.
    pub fn needs_no_abc_confirmation(&self) -> bool {
        self.effective_abc().is_none() && self.abc_choice != AbcChoice::None && !self.allow_no_abc
    }

    pub fn build_spec(&self, model: ModelSpec) -> Result<RunSpec, String> {
        let name = self.name_result()?;
        let start_seed = self.seed_result()?;
        let count = self.count_result()?;
        let mut params = self.params.clone();
        params.abc = self.effective_abc();
        Ok(RunSpec {
            name,
            params,
            start_seed,
            count,
            model,
            abc_source: self.effective_source(),
        })
    }

    pub fn set_abc_text(&mut self, text: String) {
        self.abc_summary = if text.trim().is_empty() {
            AbcSummary::default()
        } else {
            abc::summarize(&text)
        };
        self.abc_text = text;
    }

    /// Fills everything but the name from params (preset, recipe or project).
    pub fn load_params(&mut self, params: &GenerationParams) {
        let mut p = params.clone();
        let abc = p.abc.take();
        self.params = p;
        match abc {
            Some(a) => {
                self.set_abc_text(a);
                if self.abc_choice == AbcChoice::None {
                    self.abc_choice = AbcChoice::Paste;
                }
            }
            None => self.set_abc_text(String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> GenerateForm {
        let mut f = GenerateForm {
            name: "song".into(),
            seed: "5".into(),
            ..Default::default()
        };
        f.params.lyrics = "l".into();
        f.params.style = "s".into();
        f.set_abc_text("X:1\nK:C\n".into());
        f
    }

    #[test]
    fn blockers_in_order() {
        let empty = BTreeSet::new();
        let mut f = filled();
        assert_eq!(f.blocker(&empty), None);
        f.name = " ".into();
        assert_eq!(
            f.blocker(&empty),
            Some(Blocker::Name("name is required".into()))
        );
        f.name = "a:b".into();
        assert!(matches!(f.blocker(&empty), Some(Blocker::Name(_))));
        f.name = "ok".into();
        f.seed = "".into();
        assert_eq!(f.blocker(&empty), Some(Blocker::Seed("required".into())));
        f.seed = "-1".into();
        assert!(matches!(f.blocker(&empty), Some(Blocker::Seed(_))));
        f.seed = "4294967295".into();
        f.count = "0".into();
        assert!(matches!(f.blocker(&empty), Some(Blocker::Count(_))));
        f.until_stopped = true;
        assert_eq!(f.blocker(&empty), None);
        f.params.lyrics.clear();
        assert_eq!(f.blocker(&empty), None, "empty lyrics are allowed");
        f.params.style.clear();
        assert!(matches!(f.blocker(&empty), Some(Blocker::Params(_))));
    }

    #[test]
    fn collision_blocks() {
        let mut f = filled();
        f.count = "3".into();
        let existing = BTreeSet::from([6]);
        match f.blocker(&existing) {
            Some(Blocker::Collision(c)) => assert_eq!(c.continue_from, Some(7)),
            other => panic!("{other:?}"),
        }
        f.seed = "7".into();
        assert_eq!(f.blocker(&existing), None);
    }

    #[test]
    fn abc_choice_and_confirmation() {
        let mut f = filled();
        assert!(!f.needs_no_abc_confirmation());
        f.set_abc_text("  ".into());
        assert!(f.needs_no_abc_confirmation());
        f.allow_no_abc = true;
        assert!(!f.needs_no_abc_confirmation());
        f.allow_no_abc = false;
        f.abc_choice = AbcChoice::None;
        assert!(
            !f.needs_no_abc_confirmation(),
            "None on purpose skips the dialog"
        );
        f.set_abc_text("X:1\n".into());
        assert_eq!(
            f.effective_abc(),
            None,
            "None sends no abc even with text in the editor"
        );
        assert_eq!(f.effective_source(), AbcSource::None);
    }
}
