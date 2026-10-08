//! ABC notation summary and header checks for the ABC editor (§5.2).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AbcSummary {
    pub meter: Option<String>,
    pub key: Option<String>,
    pub tempo: Option<String>,
    pub unit: Option<String>,
    pub voices: Vec<String>,
    /// Bars in the longest voice.
    pub bars: usize,
    /// `% chorus`, `% verse`, … in order of appearance.
    pub sections: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn summarize(abc: &str) -> AbcSummary {
    let mut s = AbcSummary::default();
    let mut has_x = false;
    let mut bars: std::collections::BTreeMap<String, usize> = Default::default();
    let mut current = String::from("");
    for raw in abc.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('%') {
            let name = rest.trim();
            if !name.is_empty() && !name.starts_with('%') && name.split_whitespace().count() <= 3 {
                s.sections.push(name.to_string());
            }
            continue;
        }
        let header = |p: &str| line.strip_prefix(p).map(|v| v.trim().to_string());
        if header("X:").is_some() {
            has_x = true;
        } else if let Some(v) = header("M:") {
            s.meter.get_or_insert(v);
        } else if let Some(v) = header("K:") {
            s.key.get_or_insert(v);
        } else if let Some(v) = header("Q:") {
            s.tempo.get_or_insert(v);
        } else if let Some(v) = header("L:") {
            s.unit.get_or_insert(v);
        } else if let Some(v) = header("V:") {
            let id = v.split_whitespace().next().unwrap_or_default().to_string();
            if !id.is_empty() && !s.voices.contains(&id) {
                s.voices.push(id.clone());
            }
            current = id;
        } else if line.len() >= 2
            && line.as_bytes()[1] == b':'
            && line.as_bytes()[0].is_ascii_alphabetic()
        {
            // other header / inline field
        } else {
            *bars.entry(current.clone()).or_default() += count_bars(line);
        }
    }
    s.bars = bars.values().copied().max().unwrap_or(0);
    if !has_x {
        s.warnings.push("missing X: header".into());
    }
    if s.key.is_none() {
        s.warnings.push("missing K: header".into());
    }
    s
}

fn count_bars(line: &str) -> usize {
    let mut in_quote = false;
    let mut n = 0;
    for c in line.chars() {
        match c {
            '"' => in_quote = !in_quote,
            '|' if !in_quote => n += 1,
            _ => {}
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_sheetsage_output() {
        let resp: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/sheetsage2_response.json"
        ))
        .unwrap();
        let s = summarize(resp["text"].as_str().unwrap());
        assert_eq!(s.meter.as_deref(), Some("4/4"));
        assert_eq!(s.key.as_deref(), Some("Fm"));
        assert_eq!(s.tempo.as_deref(), Some("1/4=94"));
        assert_eq!(s.voices, vec!["Vocal", "Ins"]);
        assert_eq!(s.sections.first().map(String::as_str), Some("intro"));
        assert!(s.sections.contains(&"chorus".to_string()));
        assert!(s.bars > 50, "{}", s.bars);
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn warns_on_missing_headers() {
        let s = summarize("abc|def|\n");
        assert_eq!(s.warnings, vec!["missing X: header", "missing K: header"]);
        assert_eq!(s.bars, 2);
    }
}
