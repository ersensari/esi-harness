use super::*;
use crate::model_profiles::{self, ModelProfile};
use goose_providers::model_profile::{ThinkingProtocol, PROFILE_PARAM};
use goose_providers::thinking::ThinkingEffort;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProfileRequest {
    provider: String,
    model: String,
    profile: Option<ModelProfile>,
    session_id: Option<String>,
    effort: Option<String>,
}

impl GooseAcpAgent {
    pub(super) async fn on_model_profile(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, agent_client_protocol::Error> {
        let request: ProfileRequest = serde_json::from_value(params)
            .invalid_params_err_ctx("Invalid model profile request")?;
        let config = self.config()?;
        model_profiles::validate_target(&request.provider, &request.model)
            .invalid_params_err_ctx("Unsupported model profile target")?;
        match method {
            "_goose/esi/model-profile/save" => {
                model_profiles::save(config, &request.provider, &request.model, request.profile)
                    .invalid_params_err_ctx("Invalid model profile")?;
                Ok(serde_json::json!({}))
            }
            "_goose/esi/model-profile/apply" | "_goose/esi/model-profile/thinking" => {
                let session_id = request.session_id.as_deref().ok_or_else(|| {
                    agent_client_protocol::Error::invalid_params()
                        .data("A chat session is required")
                })?;
                // Never recreate a provider while it is executing a turn.
                let active_runs = self.active_prompt_runs.lock().await;
                if active_runs.contains_key(session_id) {
                    return Err(agent_client_protocol::Error::invalid_params()
                        .data("Wait for the current response to finish"));
                }
                let agent = self.get_session_agent(session_id).await?;
                let provider = agent
                    .provider()
                    .await
                    .internal_err_ctx("Provider unavailable")?;
                let current = agent
                    .model_config_for_session(session_id)
                    .await
                    .internal_err_ctx("Session model unavailable")?;
                if provider.get_name() != request.provider || current.model_name != request.model {
                    return Err(agent_client_protocol::Error::invalid_params()
                        .data("The chat model has changed; reopen its settings"));
                }
                let model = if method.ends_with("/thinking") {
                    let profile = current
                        .request_param::<ModelProfile>(PROFILE_PARAM)
                        .ok_or_else(|| {
                            agent_client_protocol::Error::invalid_params()
                                .data("Apply a model profile to this chat first")
                        })?;
                    let effort = request
                        .effort
                        .as_deref()
                        .unwrap_or("")
                        .parse::<ThinkingEffort>()
                        .map_err(|_| {
                            agent_client_protocol::Error::invalid_params()
                                .data("Invalid thinking choice")
                        })?;
                    profile
                        .validate_effort(effort)
                        .invalid_params_err_ctx("Unsupported thinking choice")?;
                    current.with_thinking_effort(effort)
                } else {
                    crate::model_config::model_config_from_user_config(
                        &request.provider,
                        &request.model,
                    )
                    .invalid_params_err_ctx("Invalid model configuration")?
                };
                let effort = model.thinking_effort();
                agent
                    .recreate_provider_for_session(session_id, &request.provider, model)
                    .await
                    .internal_err_ctx("Could not apply model settings")?;
                drop(active_runs);
                Ok(serde_json::json!({"thinkingEffort": effort}))
            }
            "_goose/esi/model-profile/read" => {
                let profile = model_profiles::read(config, &request.provider, &request.model)
                    .internal_err_ctx("Could not read model profile")?;
                let model = crate::model_config::model_config_from_user_config(
                    &request.provider,
                    &request.model,
                )
                .invalid_params_err_ctx("Invalid model configuration")?;
                let provider = crate::providers::create(&request.provider, vec![]).await;
                let (advertised, server_context) = match provider {
                    Ok(provider) => {
                        let advertised = provider.advertised_model_profile(&request.model).await;
                        let context = match advertised.as_ref().and_then(|p| p.context_limit) {
                            Some(limit) => Some(limit),
                            None => provider.advertised_context_limit(&request.model).await,
                        };
                        (advertised, context)
                    }
                    Err(_) => (None, None),
                };
                let effective_profile = profile.as_ref().or(advertised.as_ref());
                let model = if server_context.is_none() && effective_profile.is_none() {
                    match crate::providers::get_from_registry(&request.provider).await {
                        Ok(entry) => entry.normalize_model_config(model.clone()).unwrap_or(model),
                        Err(_) => model,
                    }
                } else {
                    model
                };
                let mut context = model
                    .context_limit
                    .or(server_context)
                    .unwrap_or_else(|| model.context_limit());
                let mut source = if profile.as_ref().is_some_and(|p| p.context_limit.is_some()) {
                    "manual"
                } else if config.get_goose_context_limit().ok().flatten().is_some() {
                    "global"
                } else if server_context.is_some() {
                    "server"
                } else if model.context_limit.is_some() {
                    "catalog"
                } else {
                    "fallback"
                };
                let mut session_profile = None;
                let mut session_effort = None;
                if let Some(id) = &request.session_id {
                    let agent = self.get_session_agent(id).await?;
                    let current = agent
                        .model_config_for_session(id)
                        .await
                        .internal_err_ctx("Session model unavailable")?;
                    let provider = agent
                        .provider()
                        .await
                        .internal_err_ctx("Provider unavailable")?;
                    if provider.get_name() == request.provider
                        && current.model_name == request.model
                    {
                        session_profile = current.request_param::<ModelProfile>(PROFILE_PARAM);
                        session_effort = current.thinking_effort();
                        let session_context = current
                            .context_limit
                            .or(server_context)
                            .unwrap_or_else(|| current.context_limit());
                        if current.context_limit != model.context_limit
                            || session_profile != profile
                        {
                            source = if current.context_limit.is_some() {
                                "session"
                            } else if server_context.is_some() {
                                "server"
                            } else {
                                "fallback"
                            };
                        }
                        context = session_context;
                    }
                }
                Ok(serde_json::json!({
                    "profile": profile,
                    "providerProfile": advertised,
                    "effectiveProfile": effective_profile,
                    "contextLimit": context, "contextSource": source,
                    "serverContextLimit": server_context,
                    "contextWarning": server_context.is_some_and(|limit| context > limit),
                    "sessionProfile": session_profile,
                    "thinkingEffort": session_effort,
                    "thinkingProtocol": session_profile.as_ref().map(|p| p.thinking_protocol).unwrap_or(ThinkingProtocol::None),
                }))
            }
            _ => Err(agent_client_protocol::Error::method_not_found()),
        }
    }
}
