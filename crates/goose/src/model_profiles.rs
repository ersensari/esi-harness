use crate::config::{declarative_providers, Config, ConfigError};
use anyhow::{ensure, Result};
use goose_providers::model::ModelConfig;
pub use goose_providers::model_profile::ModelProfile;

fn key(provider: &str, model: &str) -> String {
    // A JSON tuple preserves exact names and cannot collide at a separator.
    format!("ESI_MODEL_PROFILE:{}", serde_json::json!([provider, model]))
}

pub fn read(config: &Config, provider: &str, model: &str) -> Result<Option<ModelProfile>> {
    match config.get_param::<Option<ModelProfile>>(&key(provider, model)) {
        Ok(profile) => {
            if let Some(profile) = &profile {
                profile.validate()?;
            }
            Ok(profile)
        }
        Err(ConfigError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn validate_target(provider: &str, model: &str) -> Result<()> {
    ensure!(
        !model.trim().is_empty() && model.len() <= 1024,
        "Invalid model name"
    );
    let loaded = declarative_providers::load_provider(provider)?;
    ensure!(
        loaded.is_editable && loaded.config.engine == declarative_providers::ProviderEngine::OpenAI,
        "Model profiles currently support editable OpenAI-compatible custom providers"
    );
    Ok(())
}

pub fn save(
    config: &Config,
    provider: &str,
    model: &str,
    profile: Option<ModelProfile>,
) -> Result<()> {
    validate_target(provider, model)?;
    if let Some(profile) = &profile {
        profile.validate()?;
    }
    config.set_param(&key(provider, model), profile)?;
    Ok(())
}

pub fn apply(config: &Config, provider: &str, model: ModelConfig) -> Result<ModelConfig> {
    if let Some(snapshot) =
        model.request_param::<ModelProfile>(goose_providers::model_profile::PROFILE_PARAM)
    {
        return snapshot.apply_defaults(model);
    }
    match read(config, provider, &model.model_name)? {
        Some(profile) => profile.apply_defaults(model),
        None => Ok(model),
    }
}

pub async fn apply_advertised(
    provider: &dyn goose_providers::base::Provider,
    model: ModelConfig,
) -> Result<ModelConfig> {
    if validate_target(provider.get_name(), &model.model_name).is_err()
        || model
            .request_param::<ModelProfile>(goose_providers::model_profile::PROFILE_PARAM)
            .is_some()
    {
        return Ok(model);
    }
    match provider.advertised_model_profile(&model.model_name).await {
        Some(profile) => profile.apply_defaults(model),
        None => Ok(model),
    }
}
