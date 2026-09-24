//! Generation parameters, presets and the YuE2 request body (design §1.4, §5.1).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::config::{from_ron, to_ron};
use crate::error::{Error, IoContext, Result};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "Sampling")]
pub struct Sampling {
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: u32,
    pub repetition_penalty: f64,
    pub penalty_window: u32,
    pub min_tokens: u32,
    pub max_tokens: u32,
}

impl Sampling {
    pub fn abc_default() -> Self {
        Sampling {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 30,
            repetition_penalty: 1.005,
            penalty_window: 100,
            min_tokens: 32,
            max_tokens: 4096,
        }
    }

    pub fn semantic_default() -> Self {
        Sampling {
            temperature: 1.0,
            top_p: 0.95,
            top_k: 100,
            repetition_penalty: 1.2,
            penalty_window: 50,
            min_tokens: 200,
            max_tokens: 9000,
        }
    }

    fn write_options(&self, prefix: &str, o: &mut Map<String, Value>) {
        o.insert(format!("{prefix}_temperature"), js_number(self.temperature));
        o.insert(format!("{prefix}_top_p"), js_number(self.top_p));
        o.insert(format!("{prefix}_top_k"), json!(self.top_k));
        o.insert(
            format!("{prefix}_repetition_penalty"),
            js_number(self.repetition_penalty),
        );
        o.insert(
            format!("{prefix}_penalty_window"),
            json!(self.penalty_window),
        );
        o.insert(format!("{prefix}_min_tokens"), json!(self.min_tokens));
        o.insert(format!("{prefix}_max_tokens"), json!(self.max_tokens));
    }

    fn read_options(prefix: &str, o: &Map<String, Value>, default: Sampling) -> Sampling {
        let f = |k: &str, d: f64| {
            o.get(&format!("{prefix}_{k}"))
                .and_then(Value::as_f64)
                .unwrap_or(d)
        };
        let u = |k: &str, d: u32| {
            o.get(&format!("{prefix}_{k}"))
                .and_then(Value::as_u64)
                .map(|v| v as u32)
                .unwrap_or(d)
        };
        Sampling {
            temperature: f("temperature", default.temperature),
            top_p: f("top_p", default.top_p),
            top_k: u("top_k", default.top_k),
            repetition_penalty: f("repetition_penalty", default.repetition_penalty),
            penalty_window: u("penalty_window", default.penalty_window),
            min_tokens: u("min_tokens", default.min_tokens),
            max_tokens: u("max_tokens", default.max_tokens),
        }
    }

    pub fn validate(&self, what: &str) -> Vec<String> {
        let mut p = Vec::new();
        if !(self.temperature > 0.0 && self.temperature.is_finite()) {
            p.push(format!("{what} temperature must be > 0"));
        }
        if !(self.top_p > 0.0 && self.top_p <= 1.0) {
            p.push(format!("{what} top_p must be in (0, 1]"));
        }
        if self.repetition_penalty <= 0.0 {
            p.push(format!("{what} repetition penalty must be > 0"));
        }
        if self.min_tokens > self.max_tokens {
            p.push(format!("{what} min tokens > max tokens"));
        }
        p
    }
}

/// Numbers are serialized the way the web UI (JavaScript) does: integral values without a
/// fractional part, so `1.0` becomes `1`. This keeps request bodies equal to the captures.
pub fn js_number(v: f64) -> Value {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename = "GenerationParams")]
pub struct GenerationParams {
    pub lyrics: String,
    pub style: String,
    /// `None` → YuE2 generates its own ABC (§5.2).
    #[serde(default)]
    pub abc: Option<String>,
    #[serde(default = "default_cot")]
    pub cot: String,
    pub guidance_scale: f64,
    pub num_inference_steps: u32,
    pub abc_sampling: Sampling,
    pub semantic_sampling: Sampling,
    /// Forward-compatible passthrough into `request.options`.
    #[serde(default)]
    pub extra_options: BTreeMap<String, Value>,
}

fn default_cot() -> String {
    "full".into()
}

impl Default for GenerationParams {
    fn default() -> Self {
        GenerationParams {
            lyrics: String::new(),
            style: String::new(),
            abc: None,
            cot: default_cot(),
            guidance_scale: 1.01,
            num_inference_steps: 8,
            abc_sampling: Sampling::abc_default(),
            semantic_sampling: Sampling::semantic_default(),
            extra_options: BTreeMap::new(),
        }
    }
}

impl GenerationParams {
    /// The `request` object of `POST /v1/tasks/run` (§1.4). The `abc` field is left out
    /// entirely when there is no ABC.
    pub fn to_request(&self, seed: u32) -> Value {
        let mut o = Map::new();
        o.insert("style".into(), json!(self.style));
        if let Some(abc) = &self.abc {
            o.insert("abc".into(), json!(abc));
        }
        o.insert("cot".into(), json!(self.cot));
        o.insert("guidance_scale".into(), js_number(self.guidance_scale));
        o.insert(
            "num_inference_steps".into(),
            json!(self.num_inference_steps),
        );
        self.abc_sampling.write_options("abc", &mut o);
        self.semantic_sampling.write_options("semantic", &mut o);
        for (k, v) in &self.extra_options {
            o.insert(k.clone(), v.clone());
        }
        json!({ "lyrics": self.lyrics, "seed": seed, "options": o })
    }

    /// The full body of `POST /v1/tasks/run`.
    pub fn to_task_body(&self, model_id: &str, seed: u32) -> Value {
        json!({ "model": model_id, "request": self.to_request(seed) })
    }

    /// Inverse of [`to_request`](Self::to_request), used by *Regenerate* (§6.2).
    /// Returns the params and the seed.
    pub fn from_request(request: &Value) -> Result<(GenerationParams, u32)> {
        let bad = |m: &str| Error::BadResponse(format!("recipe request: {m}"));
        let o = request
            .get("options")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("no options"))?;
        let seed = request
            .get("seed")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("no seed"))?;
        let d = GenerationParams::default();
        let known = [
            "style",
            "abc",
            "cot",
            "guidance_scale",
            "num_inference_steps",
        ];
        let mut extra = BTreeMap::new();
        for (k, v) in o {
            let sampling = ["abc_", "semantic_"].iter().any(|p| {
                k.strip_prefix(p).is_some_and(|rest| {
                    matches!(
                        rest,
                        "temperature"
                            | "top_p"
                            | "top_k"
                            | "repetition_penalty"
                            | "penalty_window"
                            | "min_tokens"
                            | "max_tokens"
                    )
                })
            });
            if !sampling && !known.contains(&k.as_str()) {
                extra.insert(k.clone(), v.clone());
            }
        }
        let params = GenerationParams {
            lyrics: request
                .get("lyrics")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            style: o
                .get("style")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            abc: o.get("abc").and_then(Value::as_str).map(str::to_string),
            cot: o
                .get("cot")
                .and_then(Value::as_str)
                .unwrap_or("full")
                .to_string(),
            guidance_scale: o
                .get("guidance_scale")
                .and_then(Value::as_f64)
                .unwrap_or(d.guidance_scale),
            num_inference_steps: o
                .get("num_inference_steps")
                .and_then(Value::as_u64)
                .map(|v| v as u32)
                .unwrap_or(d.num_inference_steps),
            abc_sampling: Sampling::read_options("abc", o, d.abc_sampling),
            semantic_sampling: Sampling::read_options("semantic", o, d.semantic_sampling),
            extra_options: extra,
        };
        let seed = u32::try_from(seed).map_err(|_| bad("seed out of u32 range"))?;
        Ok((params, seed))
    }

    pub fn validate(&self) -> Vec<String> {
        let mut p = Vec::new();
        if self.lyrics.trim().is_empty() {
            p.push("lyrics are empty".into());
        }
        if self.style.trim().is_empty() {
            p.push("style is empty".into());
        }
        if !(self.guidance_scale > 0.0 && self.guidance_scale.is_finite()) {
            p.push("guidance scale must be > 0".into());
        }
        if self.num_inference_steps == 0 {
            p.push("inference steps must be > 0".into());
        }
        p.extend(self.abc_sampling.validate("ABC"));
        p.extend(self.semantic_sampling.validate("semantic"));
        p
    }
}

/// A `GenerationParams` preset file. Presets never store a run name (§5.1.1).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename = "Preset")]
pub struct Preset {
    pub params: GenerationParams,
    /// Set by "Don't ask again for this preset" on the no-ABC confirmation (§5.2).
    #[serde(default)]
    pub allow_no_abc: bool,
    /// ABC loaded from a `.abc` file (relative to the preset) when `params.abc` is `None`.
    #[serde(default)]
    pub abc_file: Option<std::path::PathBuf>,
}

impl Preset {
    pub fn load(path: &Path) -> Result<Preset> {
        let text = std::fs::read_to_string(path).at(path)?;
        let mut p: Preset = from_ron(&text)?;
        if p.params.abc.is_none()
            && let Some(f) = &p.abc_file
        {
            let f = if f.is_relative() {
                path.parent().unwrap_or(Path::new(".")).join(f)
            } else {
                f.clone()
            };
            p.params.abc = Some(std::fs::read_to_string(&f).at(&f)?);
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::fsutil::write_atomic(path, to_ron(self)?.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn fixture_request() -> Value {
        serde_json::from_str(include_str!("../../../tests/fixtures/yue2_request.json")).unwrap()
    }

    fn params_from_fixture() -> GenerationParams {
        let req = fixture_request();
        let o = &req["request"]["options"];
        GenerationParams {
            lyrics: req["request"]["lyrics"].as_str().unwrap().into(),
            style: o["style"].as_str().unwrap().into(),
            abc: Some(o["abc"].as_str().unwrap().into()),
            ..GenerationParams::default()
        }
    }

    #[test]
    fn request_matches_har_fixture() {
        let body = params_from_fixture().to_task_body("yue2", 1233);
        assert_eq!(body, fixture_request());
    }

    #[test]
    fn no_abc_leaves_field_out() {
        let mut p = params_from_fixture();
        p.abc = None;
        let r = p.to_request(1);
        assert!(r["options"].get("abc").is_none());
    }

    #[test]
    fn js_numbers() {
        assert_eq!(js_number(1.0).to_string(), "1");
        assert_eq!(js_number(1.01).to_string(), "1.01");
        assert_eq!(js_number(0.7).to_string(), "0.7");
    }

    #[test]
    fn request_round_trips() {
        let mut p = params_from_fixture();
        p.extra_options.insert("future_knob".into(), json!(3));
        let (back, seed) = GenerationParams::from_request(&p.to_request(77)).unwrap();
        assert_eq!(seed, 77);
        assert_eq!(back, p);
    }

    #[test]
    fn preset_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.ron");
        let preset = Preset {
            params: params_from_fixture(),
            allow_no_abc: true,
            abc_file: None,
        };
        preset.save(&path).unwrap();
        assert_eq!(Preset::load(&path).unwrap(), preset);
    }

    #[test]
    fn preset_abc_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("m.abc"), "X:1\nK:C\n").unwrap();
        let path = dir.path().join("p.ron");
        std::fs::write(
            &path,
            r#"Preset(params: GenerationParams(lyrics: "l", style: "s", guidance_scale: 1.0,
                num_inference_steps: 8,
                abc_sampling: Sampling(temperature: 0.7, top_p: 0.9, top_k: 30, repetition_penalty: 1.0,
                    penalty_window: 100, min_tokens: 32, max_tokens: 4096),
                semantic_sampling: Sampling(temperature: 1.0, top_p: 0.95, top_k: 100, repetition_penalty: 1.2,
                    penalty_window: 50, min_tokens: 200, max_tokens: 9000)),
               abc_file: "m.abc")"#,
        )
        .unwrap();
        let p = Preset::load(&path).unwrap();
        assert_eq!(p.params.abc.as_deref(), Some("X:1\nK:C\n"));
        assert!(!p.allow_no_abc);
    }

    #[test]
    fn validation() {
        let mut p = GenerationParams::default();
        assert!(p.validate().iter().any(|m| m.contains("lyrics")));
        p.lyrics = "x".into();
        p.style = "y".into();
        assert!(p.validate().is_empty());
        p.abc_sampling.min_tokens = 10_000;
        assert_eq!(
            p.validate(),
            vec!["ABC min tokens > max tokens".to_string()]
        );
    }
}
