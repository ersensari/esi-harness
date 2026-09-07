use goose_provider_types::{
    formats::openai::{create_request_with_options, OpenAiFormatOptions},
    images::ImageFormat,
    model::ModelConfig,
    model_profile::{ModelProfile, ThinkingProtocol},
    thinking::ThinkingEffort,
};
use serde_json::json;

fn payload(protocol: ThinkingProtocol, effort: ThinkingEffort) -> serde_json::Value {
    let profile = ModelProfile {
        thinking_protocol: protocol,
        thinking_effort: Some(effort),
        temperature: Some(0.7),
        top_p: Some(0.8),
        top_k: Some(20),
        min_p: Some(0.0),
        extended_sampling: true,
        max_tokens: Some(512),
        ..Default::default()
    };
    let mut model = ModelConfig::new("manual-model");
    model.model_name = "manual-model-high".into();
    let model = profile.apply_defaults(model).unwrap();
    create_request_with_options(
        &model,
        "test",
        &[],
        &[],
        &ImageFormat::OpenAi,
        false,
        OpenAiFormatOptions::default(),
    )
    .unwrap()
}

#[test]
fn advertised_model_profile_preserves_exact_wire_levels_and_sampling() {
    let profile: ModelProfile = serde_json::from_value(json!({
        "context_limit":262144,"max_tokens":32768,"thinking_protocol":"reasoning_effort",
        "thinking_effort":"max","thinking_levels":{"off":"none","max":"xhigh"},
        "preserve_thinking":true,"preserve_thinking_wire":true,"repetition_penalty":1.1
    }))
    .unwrap();
    let model = profile.apply_defaults(ModelConfig::new("alias")).unwrap();
    let mut payload = json!({});
    goose_provider_types::model_profile::apply_profile_thinking(&mut payload, &model).unwrap();
    assert_eq!(payload["reasoning_effort"], "xhigh");
    assert_eq!(payload["chat_template_kwargs"]["preserve_thinking"], true);
    assert!(profile.validate_effort(ThinkingEffort::Low).is_err());
    let off = model.with_thinking_effort(ThinkingEffort::Off);
    goose_provider_types::model_profile::apply_profile_thinking(&mut payload, &off).unwrap();
    assert_eq!(payload["reasoning_effort"], "none");
    assert_eq!(off.request_param::<f64>("repetition_penalty"), Some(1.1));
}

#[test]
fn unsloth_off_sends_real_boolean_and_no_effort_or_internal_fields() {
    let p = payload(ThinkingProtocol::Unsloth, ThinkingEffort::Off);
    assert_eq!(p["model"], "manual-model-high");
    assert_eq!(p["enable_thinking"], false);
    assert_eq!(p["enable_tools"], false);
    assert!(p.get("reasoning_effort").is_none());
    assert!(p.get("esi_model_profile").is_none());
    assert!(p.get("preserve_thinking_context").is_none());
    assert_eq!(p["top_k"], 20);
    assert_eq!(p["max_tokens"], 512);
}

#[test]
fn unsloth_levels_and_chat_template_have_distinct_wire_contracts() {
    let p = payload(ThinkingProtocol::Unsloth, ThinkingEffort::High);
    assert_eq!(p["enable_thinking"], true);
    assert_eq!(p["reasoning_effort"], "high");
    let p = payload(ThinkingProtocol::ChatTemplate, ThinkingEffort::Medium);
    assert_eq!(p["chat_template_kwargs"]["enable_thinking"], true);
    assert!(p.get("enable_thinking").is_none());
    assert!(p.get("reasoning_effort").is_none());
    let p = payload(ThinkingProtocol::ReasoningEffort, ThinkingEffort::Off);
    assert_eq!(p["reasoning_effort"], "none");
}

#[test]
fn unsupported_settings_and_bad_limits_are_rejected() {
    for value in [
        json!({"context_limit":0}),
        json!({"max_tokens":0}),
        json!({"context_limit":512,"max_tokens":512}),
        json!({"temperature":-1}),
        json!({"top_p":1.1}),
        json!({"top_k":20}),
        json!({"min_p":0.1}),
        json!({"thinking_protocol":"none","thinking_effort":"low"}),
        json!({"thinking_protocol":"chat_template","thinking_effort":"high"}),
        json!({"extended_sampling":true,"top_k":-2}),
    ] {
        let profile: ModelProfile = serde_json::from_value(value.clone()).unwrap();
        assert!(profile.validate().is_err(), "{value}");
    }
    assert!(serde_json::from_value::<ModelProfile>(json!({"arbitrary_api_key":"no"})).is_err());
}

#[test]
fn explicit_session_settings_outrank_profile_defaults() {
    let profile = ModelProfile {
        temperature: Some(0.7),
        context_limit: Some(8192),
        max_tokens: Some(512),
        thinking_protocol: ThinkingProtocol::Unsloth,
        thinking_effort: Some(ThinkingEffort::Low),
        preserve_thinking: true,
        ..Default::default()
    };
    let model = ModelConfig::new("manual")
        .with_temperature(Some(0.2))
        .with_context_limit(Some(4096))
        .with_thinking_effort(ThinkingEffort::Off);
    let model = profile.apply_defaults(model).unwrap();
    assert_eq!(model.context_limit, Some(4096));
    assert_eq!(model.temperature, Some(0.2));
    assert_eq!(model.thinking_effort(), Some(ThinkingEffort::Off));
    assert_eq!(
        model.request_param::<bool>("preserve_thinking_context"),
        Some(true)
    );
}

#[test]
fn profile_decimal_values_survive_persistence_without_f32_expansion() {
    let profile: ModelProfile = serde_json::from_value(json!({
        "temperature": 0.7, "top_p": 0.8, "min_p": 0.05,
        "extended_sampling": true
    }))
    .unwrap();
    let saved = serde_json::to_value(&profile).unwrap();
    assert_eq!(saved["temperature"], json!(0.7));
    assert_eq!(saved["top_p"], json!(0.8));
    assert_eq!(saved["min_p"], json!(0.05));
}

#[test]
fn absent_profile_keeps_existing_request_behavior() {
    let model = ModelConfig::new("manual").with_thinking_effort(ThinkingEffort::High);
    let p = create_request_with_options(
        &model,
        "test",
        &[],
        &[],
        &ImageFormat::OpenAi,
        false,
        OpenAiFormatOptions::default(),
    )
    .unwrap();
    assert!(p.get("enable_thinking").is_none());
    assert!(p.get("reasoning_effort").is_none());
}
