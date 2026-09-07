use goose::config::{
    declarative_providers::{create_custom_provider, CreateCustomProviderParams},
    Config,
};
use goose::model_config::{
    model_config_from_user_config, model_config_from_user_config_with_session_settings,
};
use goose::model_profiles::{read, save, ModelProfile};
use goose_providers::{model_profile::ThinkingProtocol, thinking::ThinkingEffort};

#[test]
fn profiles_persist_by_exact_pair_without_global_or_cross_model_leaks() {
    let root = tempfile::tempdir().unwrap();
    let _guard = env_lock::lock_env([
        ("GOOSE_PATH_ROOT", root.path().to_str()),
        ("GOOSE_DISABLE_KEYRING", Some("1")),
        ("GOOSE_ADDITIONAL_CONFIG_FILES", Some("")),
        ("GOOSE_TEMPERATURE", None),
        ("GOOSE_CONTEXT_LIMIT", None),
    ]);
    let config = Config::global();
    let provider = create_custom_provider(CreateCustomProviderParams {
        engine: "openai".into(),
        display_name: "Profile fixture".into(),
        api_url: "http://127.0.0.1:1".into(),
        api_key: None,
        models: vec!["manual-high".into(), "second".into()],
        supports_streaming: Some(true),
        headers: None,
        requires_auth: false,
        catalog_provider_id: None,
        base_path: None,
        preserves_thinking: None,
    })
    .unwrap();
    let profile = ModelProfile {
        temperature: Some(0.4),
        context_limit: Some(16384),
        max_tokens: Some(1024),
        thinking_protocol: ThinkingProtocol::Unsloth,
        thinking_effort: Some(ThinkingEffort::Low),
        ..Default::default()
    };
    save(config, &provider.name, "manual-high", Some(profile.clone())).unwrap();
    assert_eq!(
        read(config, &provider.name, "manual-high").unwrap(),
        Some(profile)
    );
    assert!(read(config, &provider.name, "manual-high ")
        .unwrap()
        .is_none());
    assert!(read(config, "another-provider", "manual-high")
        .unwrap()
        .is_none());
    assert!(config.get_param::<f32>("GOOSE_TEMPERATURE").is_err());
    let model = model_config_from_user_config(&provider.name, "manual-high").unwrap();
    assert_eq!(model.model_name, "manual-high");
    assert_eq!(model.context_limit, Some(16384));
    assert_eq!(model.temperature, Some(0.4));
    let current = model.with_thinking_effort(ThinkingEffort::Off);
    let same = model_config_from_user_config_with_session_settings(
        &provider.name,
        "manual-high",
        Some(&current),
        None,
        None,
    )
    .unwrap();
    assert_eq!(same.thinking_effort(), Some(ThinkingEffort::Off));
    let other = model_config_from_user_config_with_session_settings(
        &provider.name,
        "second",
        Some(&current),
        None,
        None,
    )
    .unwrap();
    assert_eq!(other.thinking_effort(), None);
    assert_eq!(other.temperature, None);
    assert_eq!(other.context_limit, None);
    save(config, &provider.name, "manual-high", None).unwrap();
    assert!(read(config, &provider.name, "manual-high")
        .unwrap()
        .is_none());
    assert_eq!(
        model_config_from_user_config(&provider.name, "manual-high")
            .unwrap()
            .temperature,
        None
    );
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use goose_providers::{
            api_client::{ApiClient, AuthMethod},
            openai::OpenAiProviderBuilder,
        };
        use wiremock::{matchers::path, Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"data":[{"id":"manual-high"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/model/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":[{"model_name":"manual-high",
                "model_info":{"context_window":262144,"max_output_tokens":32768},
                "litellm_params":{"temperature":0.65,"top_p":0.95}}]}),
            ))
            .mount(&server)
            .await;
        let runtime_provider = OpenAiProviderBuilder::new(
            ApiClient::new_with_tls(server.uri(), AuthMethod::NoAuth, None).unwrap(),
        )
        .name(provider.name.clone())
        .build();
        let fresh = model_config_from_user_config(&provider.name, "manual-high").unwrap();
        let automatic = goose::model_profiles::apply_advertised(&runtime_provider, fresh)
            .await
            .unwrap();
        assert_eq!(automatic.context_limit, Some(262144));
        assert_eq!(automatic.max_tokens, Some(32768));
        assert_eq!(automatic.temperature, Some(0.65));
        assert!(
            read(config, &provider.name, "manual-high")
                .unwrap()
                .is_none(),
            "Discovery must not save a manual profile"
        );
        let explicit = ModelProfile {
            context_limit: Some(16384),
            ..Default::default()
        }
        .apply_defaults(model_config_from_user_config(&provider.name, "manual-high").unwrap())
        .unwrap();
        assert_eq!(
            goose::model_profiles::apply_advertised(&runtime_provider, explicit)
                .await
                .unwrap()
                .context_limit,
            Some(16384)
        );
        server.reset().await;
        assert_eq!(
            goose::model_profiles::apply_advertised(&runtime_provider, automatic)
                .await
                .unwrap()
                .context_limit,
            Some(262144),
            "Existing snapshot survives outage"
        );
    });
}
