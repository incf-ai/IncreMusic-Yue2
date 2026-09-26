//! `ensure_loaded`: compare against `/v1/models`, then load, reload or do nothing (§2.2).
//! `check_paths`: ask a server whether the configured model files exist (§1.1).

use crate::api::{AudioCppClient, ModelInfo, PathStatus};
use crate::config::ModelSpec;
use crate::error::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnsureAction {
    AlreadyLoaded,
    Loaded,
    Reloaded,
}

/// What `ensure_loaded` would do given the server's model list.
pub fn plan(models: &[ModelInfo], spec: &ModelSpec) -> EnsureAction {
    match models.iter().find(|m| m.id == spec.id) {
        Some(m) if m.loaded => {
            let same_opts = m
                .session_options
                .as_ref()
                .is_none_or(|o| o == &spec.session_options);
            let same_path = m.path.as_deref().is_none_or(|p| p == spec.path);
            if same_opts && same_path {
                EnsureAction::AlreadyLoaded
            } else {
                EnsureAction::Reloaded
            }
        }
        _ => EnsureAction::Loaded,
    }
}

pub async fn ensure_loaded(client: &AudioCppClient, spec: &ModelSpec) -> Result<EnsureAction> {
    let models = client.list_models().await?;
    let action = plan(&models, spec);
    match action {
        EnsureAction::AlreadyLoaded => {}
        EnsureAction::Loaded => {
            client.load_model(spec).await?;
        }
        EnsureAction::Reloaded => {
            client.unload_model(&spec.id).await?;
            client.load_model(spec).await?;
        }
    }
    tracing::debug!(model = %spec.id, ?action, server = client.base_url(), "ensure_loaded");
    Ok(action)
}

/// A server-local path the config expects, and what it must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedPath {
    pub model: String,
    pub path: String,
    /// `None`: either a file or a directory will do.
    pub directory: Option<bool>,
}

/// The model `path` itself, plus every relative `*.gguf` session option (YuE2's
/// `yue2.model_gguf`, `yue2.vae_gguf`), which the server resolves inside `path`.
pub fn expected_paths(spec: &ModelSpec) -> Vec<ExpectedPath> {
    let files: Vec<&String> = spec
        .session_options
        .values()
        .filter(|v| v.ends_with(".gguf") && !is_absolute(v))
        .collect();
    let mut out = vec![ExpectedPath {
        model: spec.id.clone(),
        path: spec.path.clone(),
        directory: if files.is_empty() { None } else { Some(true) },
    }];
    // the server may run on another OS; follow the separator the config already uses
    let sep = if spec.path.contains('\\') && !spec.path.contains('/') {
        '\\'
    } else {
        '/'
    };
    let base = spec.path.trim_end_matches(['/', '\\']);
    for f in files {
        out.push(ExpectedPath {
            model: spec.id.clone(),
            path: format!("{base}{sep}{f}"),
            directory: Some(false),
        });
    }
    out
}

fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with('\\') || p.as_bytes().get(1) == Some(&b':')
}

/// Why `status` doesn't match what `want` expects, if it doesn't.
pub fn path_problem(want: &ExpectedPath, status: &PathStatus) -> Option<String> {
    let what = |dir: bool| if dir { "a directory" } else { "a file" };
    if !status.exists {
        return Some(format!("{}: {} does not exist", want.model, want.path));
    }
    match want.directory {
        Some(dir) if dir != status.directory => Some(format!(
            "{}: {} is not {}",
            want.model,
            want.path,
            what(dir)
        )),
        _ => None,
    }
}

/// Checks every model's paths on one server. `Err` means the server couldn't answer
/// (for example, it runs without `--ui-management`); `Ok` lists the problems found.
pub async fn check_paths(client: &AudioCppClient, specs: &[&ModelSpec]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for want in specs.iter().flat_map(|s| expected_paths(s)) {
        let status = client.path_status(&want.path).await?;
        problems.extend(path_problem(&want, &status));
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ModelSpec {
        ModelSpec {
            id: "yue2".into(),
            family: "yue2".into(),
            task: "gen".into(),
            mode: "offline".into(),
            path: "/p".into(),
            load_options: Default::default(),
            session_options: [("a".to_string(), "1".to_string())].into(),
        }
    }

    fn info(loaded: bool, opt: &str) -> ModelInfo {
        ModelInfo {
            id: "yue2".into(),
            family: None,
            task: None,
            mode: None,
            loaded,
            path: Some("/p".into()),
            session_options: Some([("a".to_string(), opt.to_string())].into()),
        }
    }

    #[test]
    fn plans() {
        assert_eq!(plan(&[], &spec()), EnsureAction::Loaded);
        assert_eq!(plan(&[info(false, "1")], &spec()), EnsureAction::Loaded);
        assert_eq!(
            plan(&[info(true, "1")], &spec()),
            EnsureAction::AlreadyLoaded
        );
        assert_eq!(plan(&[info(true, "2")], &spec()), EnsureAction::Reloaded);
    }

    fn status(exists: bool, directory: bool) -> PathStatus {
        PathStatus {
            exists,
            directory,
            file: exists && !directory,
        }
    }

    #[test]
    fn expected_paths_include_relative_gguf_options() {
        let mut s = spec();
        s.path = "/models/Yue2-3B-GGUF/".into();
        s.session_options = [
            ("yue2.model_gguf", "yue2-3b-bf16.gguf"),
            ("yue2.vae_gguf", "/elsewhere/vae.gguf"),
            ("yue2.ar_lora_scale", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let p = expected_paths(&s);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].path, "/models/Yue2-3B-GGUF/");
        assert_eq!(p[0].directory, Some(true));
        assert_eq!(p[1].path, "/models/Yue2-3B-GGUF/yue2-3b-bf16.gguf");
        assert_eq!(p[1].directory, Some(false));

        s.path = r"D:\models\Yue2".into();
        assert_eq!(
            expected_paths(&s)[1].path,
            r"D:\models\Yue2\yue2-3b-bf16.gguf"
        );

        // a single-file model (SheetSage2) may be either
        s.session_options.clear();
        assert_eq!(expected_paths(&s)[0].directory, None);
    }

    #[test]
    fn path_problems() {
        let want = |directory| ExpectedPath {
            model: "yue2".into(),
            path: "/m/y".into(),
            directory,
        };
        assert_eq!(path_problem(&want(None), &status(true, false)), None);
        assert_eq!(path_problem(&want(Some(true)), &status(true, true)), None);
        assert_eq!(
            path_problem(&want(None), &status(false, false)).unwrap(),
            "yue2: /m/y does not exist"
        );
        assert_eq!(
            path_problem(&want(Some(true)), &status(true, false)).unwrap(),
            "yue2: /m/y is not a directory"
        );
        assert_eq!(
            path_problem(&want(Some(false)), &status(true, true)).unwrap(),
            "yue2: /m/y is not a file"
        );
    }
}
