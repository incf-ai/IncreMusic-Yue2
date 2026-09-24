//! `ensure_loaded`: compare against `/v1/models`, then load, reload or do nothing (§2.2).

use crate::api::{AudioCppClient, ModelInfo};
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
}
