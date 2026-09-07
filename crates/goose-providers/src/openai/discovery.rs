use super::OpenAiProvider;
use crate::model_profile::{ModelProfile, ThinkingProtocol};
use crate::thinking::ThinkingEffort;
use serde_json::{json, Value};

impl OpenAiProvider {
    pub(super) async fn metadata_json(&self, path: &str) -> Option<Value> {
        let mut response = self.api_client.request(path).metadata_get().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        const LIMIT: usize = 4 * 1024 * 1024;
        if response.content_length().is_some_and(|n| n > LIMIT as u64) {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len().checked_add(chunk.len())? > LIMIT {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).ok()
    }

    pub(super) async fn discover_profile(&self, model: &str) -> Option<ModelProfile> {
        let path = Self::map_base_path(
            &self.base_path,
            "models",
            super::OPEN_AI_DEFAULT_MODELS_PATH,
        );
        // Resolve against the actual configured prefix, including /gateway/v1.
        let mut host = url::Url::parse(self.api_client.host()).ok()?;
        if !host.path().ends_with('/') {
            host.set_path(&format!("{}/", host.path()));
        }
        let models_url = host.join(&path).ok()?;
        let prefix = models_url.path().strip_suffix("models")?;
        let info_path = format!("{}model/info", prefix.strip_suffix("v1/").unwrap_or(prefix));
        let list = self.metadata_json(&path).await?;
        let entries = list.get("data")?.as_array()?;
        let mut matches = entries
            .iter()
            .filter(|row| row["id"].as_str() == Some(model));
        let entry = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let info = self.metadata_json(&info_path).await;
        parse_profile(entry, info.as_ref(), model)
    }
}

fn positive(value: &Value) -> Option<usize> {
    usize::try_from(value.as_u64()?).ok().filter(|n| *n > 0)
}

fn parse_profile(entry: &Value, catalog: Option<&Value>, model: &str) -> Option<ModelProfile> {
    if let Some(rows) = catalog.and_then(|v| v["data"].as_array()) {
        let mut matches = rows
            .iter()
            .filter(|row| row["model_name"].as_str() == Some(model));
        if let Some(row) = matches.next() {
            // A load-balanced alias with inconsistent settings is not a single profile.
            if matches.any(|other| {
                other["model_info"] != row["model_info"]
                    || other["litellm_params"] != row["litellm_params"]
            }) {
                return None;
            }
            return litellm_profile(row);
        }
    }
    let context = positive(&entry["meta"]["n_ctx"]).or_else(|| {
        (entry["owned_by"] == "unsloth-studio")
            .then(|| positive(&entry["context_length"]))
            .flatten()
    });
    if let Some(context) = context {
        return Some(ModelProfile {
            context_limit: Some(context),
            ..Default::default()
        });
    }
    // Restricted gateway keys often cannot read model/info. Their visible
    // model cards still publish input/output budgets; no elevated key is needed.
    litellm_profile(&json!({"model_info": entry, "litellm_params": {}}))
}

pub(super) fn visible_context(entry: &Value) -> Option<usize> {
    parse_profile(entry, None, entry["id"].as_str()?).and_then(|profile| profile.context_limit)
}

fn litellm_profile(row: &Value) -> Option<ModelProfile> {
    let info = &row["model_info"];
    let params = &row["litellm_params"];
    let extra = &params["extra_body"];
    for key in [
        "max_output_tokens",
        "max_input_tokens",
        "context_window",
        "max_context_tokens",
    ] {
        if !info[key].is_null() {
            positive(&info[key])?;
        }
    }
    if !params["max_tokens"].is_null() {
        positive(&params["max_tokens"])?;
    }
    let output = positive(&info["max_output_tokens"]).or_else(|| positive(&params["max_tokens"]));
    let context = positive(&info["context_window"])
        .or_else(|| positive(&info["max_context_tokens"]))
        .or_else(|| positive(&info["max_input_tokens"])?.checked_add(output?));
    let max_tokens = match (positive(&params["max_tokens"]), output) {
        (Some(default), Some(limit)) => Some(default.min(limit)),
        (a, b) => a.or(b),
    };
    let mut value = json!({
        "context_limit": context,
        "max_tokens": max_tokens,
        "extended_sampling": false,
        "thinking_protocol": "none",
        "preserve_thinking": false,
    });
    // Whitelist only generation settings; never retain api_key/api_base or raw rows.
    for key in [
        "temperature",
        "top_p",
        "top_k",
        "min_p",
        "presence_penalty",
        "frequency_penalty",
        "repetition_penalty",
    ] {
        if let Some(setting) = params.get(key).or_else(|| extra.get(key)) {
            value[key] = setting.clone();
        }
    }
    value["extended_sampling"] = json!(!value["top_k"].is_null() || !value["min_p"].is_null());
    if let Some(preserve) = extra.pointer("/chat_template_kwargs/preserve_thinking") {
        value["preserve_thinking"] = preserve.clone();
        value["preserve_thinking_wire"] = json!(true);
    }
    let mut profile: ModelProfile = serde_json::from_value(value).ok()?;
    if info["supports_reasoning"] == true {
        if let Some(efforts) = info["supported_reasoning_efforts"].as_array() {
            let mut levels = std::collections::BTreeMap::new();
            for (key, candidates) in [
                ("off", &["none", "off"][..]),
                ("low", &["low"][..]),
                ("medium", &["medium"][..]),
                ("high", &["high"][..]),
                ("max", &["max", "xhigh"][..]),
            ] {
                if let Some(wire) = candidates
                    .iter()
                    .find(|wire| efforts.iter().any(|v| v.as_str() == Some(**wire)))
                {
                    levels.insert(key.to_owned(), (*wire).to_owned());
                }
            }
            if !levels.is_empty() {
                profile.thinking_protocol = ThinkingProtocol::ReasoningEffort;
                profile.thinking_effort = params["reasoning_effort"]
                    .as_str()
                    .and_then(|v| v.parse::<ThinkingEffort>().ok())
                    .filter(|effort| levels.contains_key(&effort.to_string()));
                profile.thinking_levels = Some(levels);
            }
        }
    }
    profile.validate().ok()?;
    if profile == ModelProfile::default() {
        return None;
    }
    Some(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api_client::{ApiClient, AuthMethod},
        base::Provider,
    };
    use wiremock::{
        matchers::{header, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn discovery_http_prefix_auth_refresh_and_missing_model() {
        let server = MockServer::start().await;
        let provider = super::super::OpenAiProviderBuilder::new(
            ApiClient::new_with_tls(
                server.uri(),
                AuthMethod::BearerToken("fixture-secret".into()),
                None,
            )
            .unwrap(),
        )
        .base_path("gateway/v1/chat/completions")
        .name("custom_discovery_fixture")
        .build();
        Mock::given(path("/gateway/v1/models"))
            .and(header("Authorization", "Bearer fixture-secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"alias"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/gateway/model/info"))
            .and(header("Authorization", "Bearer fixture-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[row()]})))
            .mount(&server)
            .await;
        assert_eq!(
            provider
                .advertised_model_profile("alias")
                .await
                .unwrap()
                .context_limit,
            Some(262144)
        );
        assert!(provider.advertised_model_profile("missing").await.is_none());
        assert_eq!(
            provider
                .get_context_limit(&crate::model::ModelConfig::new("alias"))
                .await
                .unwrap(),
            262144,
            "Context on Auto must use the same rich metadata as the UI"
        );
        server.reset().await;
        assert!(
            provider.advertised_model_profile("alias").await.is_none(),
            "No stale cache after provider failure"
        );
    }

    #[tokio::test]
    async fn discovery_http_rejects_redirects_and_oversized_json() {
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(
            ApiClient::new_with_tls(server.uri(), AuthMethod::NoAuth, None).unwrap(),
        );
        Mock::given(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("Location", "/credentials-target"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/credentials-target"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        assert!(provider.advertised_model_profile("alias").await.is_none());
        server.verify().await;
        server.reset().await;
        Mock::given(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(" ".repeat(4 * 1024 * 1024 + 1)),
            )
            .mount(&server)
            .await;
        assert!(provider.advertised_model_profile("alias").await.is_none());
    }

    #[tokio::test]
    async fn discovery_http_deadline_is_bounded() {
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(
            ApiClient::new_with_tls(server.uri(), AuthMethod::NoAuth, None).unwrap(),
        );
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(8)))
            .mount(&server)
            .await;
        let start = std::time::Instant::now();
        assert!(provider.advertised_model_profile("alias").await.is_none());
        assert!(start.elapsed() < std::time::Duration::from_secs(7));
    }

    #[tokio::test]
    async fn discovery_restricted_key_uses_visible_model_budgets_when_info_is_forbidden() {
        let server = MockServer::start().await;
        let provider = super::super::OpenAiProviderBuilder::new(
            ApiClient::new_with_tls(
                server.uri(),
                AuthMethod::BearerToken("restricted-fixture".into()),
                None,
            )
            .unwrap(),
        )
        .name("custom_restricted_fixture")
        .build();
        Mock::given(path("/v1/models"))
            .and(header("Authorization", "Bearer restricted-fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{
                "id":"alias", "max_input_tokens":229376, "max_output_tokens":32768
            }]})))
            .mount(&server)
            .await;
        Mock::given(path("/model/info"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let profile = provider.advertised_model_profile("alias").await.unwrap();
        assert_eq!(profile.context_limit, Some(262144));
        assert_eq!(profile.max_tokens, Some(32768));
        assert_eq!(profile.thinking_protocol, ThinkingProtocol::None);
        assert_eq!(
            provider.advertised_context_limit("alias").await,
            Some(262144)
        );
        assert!(provider
            .advertised_context_limit("different-model")
            .await
            .is_none());
        assert_eq!(
            provider
                .get_context_limit(&crate::model::ModelConfig::new("alias"))
                .await
                .unwrap(),
            262144
        );
        assert!(provider
            .advertised_model_profile("different-model")
            .await
            .is_none());
    }

    #[test]
    fn discovery_visible_budgets_validate_overflow_missing_and_exact_context() {
        let entry = json!({"id":"alias", "max_input_tokens":229376,"max_output_tokens":32768});
        assert_eq!(
            parse_profile(&entry, None, "alias").unwrap().context_limit,
            Some(262144)
        );
        let mut explicit = entry.clone();
        explicit["context_window"] = json!(131072);
        assert_eq!(
            parse_profile(&explicit, None, "alias")
                .unwrap()
                .context_limit,
            Some(131072)
        );
        let overflow = json!({"id":"alias", "max_input_tokens":u64::MAX,"max_output_tokens":32768});
        assert!(parse_profile(&overflow, None, "alias").is_none_or(|p| p.context_limit.is_none()));
        assert!(parse_profile(
            &json!({"id":"alias","max_context_length":1048576}),
            None,
            "alias"
        )
        .is_none());
    }
    fn row() -> Value {
        json!({"model_name":"alias", "model_info":{"max_input_tokens":229376,"max_output_tokens":32768,
            "supports_reasoning":true,"supported_reasoning_efforts":["none","low","medium","high","xhigh"]},
            "litellm_params":{"max_tokens":32768,"temperature":0.65,"reasoning_effort":"xhigh",
                "api_key":"must-not-copy","extra_body":{"top_k":20,"min_p":0.0,"repetition_penalty":1.0,
                "chat_template_kwargs":{"preserve_thinking":true}}}})
    }
    #[test]
    fn discovery_litellm_settings_are_typed_and_secrets_excluded() {
        let profile = litellm_profile(&row()).unwrap();
        assert_eq!(profile.context_limit, Some(262144));
        assert_eq!(profile.max_tokens, Some(32768));
        assert_eq!(profile.top_k, Some(20));
        assert_eq!(profile.thinking_effort, Some(ThinkingEffort::Max));
        assert_eq!(profile.thinking_levels.as_ref().unwrap()["max"], "xhigh");
        assert!(profile.preserve_thinking && profile.preserve_thinking_wire);
        assert!(!serde_json::to_string(&profile)
            .unwrap()
            .contains("must-not-copy"));
    }
    #[test]
    fn discovery_rejects_invalid_and_ambiguous_metadata() {
        let mut invalid = row();
        invalid["litellm_params"]["temperature"] = json!(9);
        assert!(litellm_profile(&invalid).is_none());
        let entry = json!({"id":"alias"});
        assert!(parse_profile(&entry, Some(&json!({"data":[row(),invalid]})), "alias").is_none());
        assert!(parse_profile(&entry, Some(&json!({"data":[row()]})), "other").is_none());
    }
    #[test]
    fn discovery_never_guesses_reasoning_or_theoretical_capacity() {
        let mut data = row();
        data["model_info"]["supports_reasoning"] = json!(false);
        assert_eq!(
            litellm_profile(&data).unwrap().thinking_protocol,
            ThinkingProtocol::None
        );
        let entry = json!({"id":"alias","owned_by":"unsloth-studio","context_length":32768,"max_context_length":1048576});
        assert_eq!(
            parse_profile(&entry, None, "alias").unwrap().context_limit,
            Some(32768)
        );
        assert!(parse_profile(&json!({"max_context_length":1048576}), None, "alias").is_none());
    }
}
