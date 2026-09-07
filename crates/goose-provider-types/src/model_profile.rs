//! Explicit user-declared capabilities for an exact custom provider/model pair.
use crate::model::ModelConfig;
use crate::thinking::ThinkingEffort;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROFILE_PARAM: &str = "esi_model_profile";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingProtocol {
    #[default]
    None,
    EnableThinking,
    ChatTemplate,
    ReasoningEffort,
    Unsloth,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub context_limit: Option<usize>,
    pub max_tokens: Option<i32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i32>,
    pub min_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub repetition_penalty: Option<f64>,
    /// Exact advertised wire values keyed by harness effort; absent for manual legacy profiles.
    pub thinking_levels: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    pub preserve_thinking_wire: bool,
    #[serde(default)]
    pub extended_sampling: bool,
    #[serde(default)]
    pub thinking_protocol: ThinkingProtocol,
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default)]
    pub preserve_thinking: bool,
}

impl ModelProfile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.context_limit.is_none_or(|n| n > 0),
            "Context must be positive"
        );
        ensure!(
            self.max_tokens.is_none_or(|n| n > 0),
            "Output must be positive"
        );
        if let (Some(context), Some(output)) = (self.context_limit, self.max_tokens) {
            ensure!(
                (output as usize) < context,
                "Output must be smaller than context"
            );
        }
        for (name, value, min, max) in [
            ("temperature", self.temperature, 0.0, 2.0),
            ("top_p", self.top_p, 0.0, 1.0),
            ("min_p", self.min_p, 0.0, 1.0),
            ("presence_penalty", self.presence_penalty, -2.0, 2.0),
            ("frequency_penalty", self.frequency_penalty, -2.0, 2.0),
            ("repetition_penalty", self.repetition_penalty, 0.0, 10.0),
        ] {
            ensure!(
                value.is_none_or(|v| v.is_finite() && (min..=max).contains(&v)),
                "Invalid {name}"
            );
        }
        ensure!(
            self.top_k.is_none_or(|n| n >= -1),
            "top_k must be -1 or nonnegative"
        );
        if self.thinking_protocol == ThinkingProtocol::Unsloth {
            ensure!(
                self.top_k.is_none_or(|n| n <= 100),
                "Unsloth top_k must not exceed 100"
            );
        }
        ensure!(
            self.extended_sampling || (self.top_k.is_none() && self.min_p.is_none()),
            "Confirm server support for top_k/min_p before setting them"
        );
        if let Some(levels) = &self.thinking_levels {
            ensure!(
                self.thinking_protocol == ThinkingProtocol::ReasoningEffort,
                "Advertised levels require reasoning_effort"
            );
            for (key, value) in levels {
                ensure!(
                    matches!(
                        (key.as_str(), value.as_str()),
                        ("off", "none" | "off")
                            | ("low", "low")
                            | ("medium", "medium")
                            | ("high", "high")
                            | ("max", "max" | "xhigh")
                    ),
                    "Invalid advertised thinking mapping"
                );
            }
        }
        if let Some(effort) = self.thinking_effort {
            self.validate_effort(effort)?;
        }
        Ok(())
    }

    pub fn validate_effort(&self, effort: ThinkingEffort) -> Result<()> {
        if let Some(levels) = &self.thinking_levels {
            ensure!(
                levels.contains_key(&effort.to_string()),
                "Thinking choice is not advertised by the provider"
            );
            return Ok(());
        }
        let allowed = match self.thinking_protocol {
            ThinkingProtocol::None => false,
            ThinkingProtocol::EnableThinking | ThinkingProtocol::ChatTemplate => {
                matches!(effort, ThinkingEffort::Off | ThinkingEffort::Medium)
            }
            ThinkingProtocol::ReasoningEffort | ThinkingProtocol::Unsloth => matches!(
                effort,
                ThinkingEffort::Off
                    | ThinkingEffort::Low
                    | ThinkingEffort::Medium
                    | ThinkingEffort::High
            ),
        };
        ensure!(
            allowed,
            "Thinking choice is not supported by this model profile"
        );
        Ok(())
    }

    pub fn apply_defaults(&self, mut model: ModelConfig) -> Result<ModelConfig> {
        self.validate()?;
        model = model
            .with_default_context_limit(self.context_limit)
            .with_default_max_tokens(self.max_tokens);
        if model.temperature.is_none() {
            model.temperature = self.temperature.map(|value| value as f32);
        }
        model.reasoning = Some(self.thinking_protocol != ThinkingProtocol::None);
        let params = model.request_params.get_or_insert_default();
        params.insert(PROFILE_PARAM.into(), serde_json::to_value(self)?);
        params
            .entry("preserve_thinking_context".into())
            .or_insert(json!(self.preserve_thinking));
        for (name, value) in [
            ("top_p", self.top_p),
            ("min_p", self.min_p),
            ("presence_penalty", self.presence_penalty),
            ("frequency_penalty", self.frequency_penalty),
            ("repetition_penalty", self.repetition_penalty),
        ] {
            if let Some(value) = value {
                params.entry(name.into()).or_insert(json!(value));
            }
        }
        if let Some(top_k) = self.top_k {
            params.entry("top_k".into()).or_insert(json!(top_k));
        }
        if let Some(effort) = self.thinking_effort {
            params
                .entry("thinking_effort".into())
                .or_insert(json!(effort));
        }
        Ok(model)
    }
}

/// Applied last so legacy name heuristics cannot override the declared wire protocol.
pub fn apply_profile_thinking(payload: &mut Value, model: &ModelConfig) -> Result<()> {
    let Some(profile) = model.request_param::<ModelProfile>(PROFILE_PARAM) else {
        return Ok(());
    };
    profile.validate()?;
    let object = payload.as_object_mut().expect("request is an object");
    object.remove("reasoning_effort");
    object.remove("enable_thinking");
    if profile.preserve_thinking_wire {
        let kwargs = object.entry("chat_template_kwargs").or_insert(json!({}));
        kwargs["preserve_thinking"] = json!(profile.preserve_thinking);
    }
    if profile.thinking_protocol == ThinkingProtocol::Unsloth {
        object.insert("enable_tools".into(), json!(false));
        object.insert("preserve_thinking".into(), json!(profile.preserve_thinking));
    }
    if profile.thinking_protocol == ThinkingProtocol::None {
        return Ok(());
    }
    let Some(effort) = model.thinking_effort() else {
        return Ok(());
    };
    profile.validate_effort(effort)?;
    let enabled = effort != ThinkingEffort::Off;
    match profile.thinking_protocol {
        ThinkingProtocol::None => {}
        ThinkingProtocol::EnableThinking => {
            object.insert("enable_thinking".into(), json!(enabled));
        }
        ThinkingProtocol::ChatTemplate => {
            let kwargs = object.entry("chat_template_kwargs").or_insert(json!({}));
            kwargs["enable_thinking"] = json!(enabled);
        }
        ThinkingProtocol::ReasoningEffort => {
            object.insert(
                "reasoning_effort".into(),
                if let Some(wire) = profile
                    .thinking_levels
                    .as_ref()
                    .and_then(|levels| levels.get(&effort.to_string()))
                {
                    json!(wire)
                } else if enabled {
                    json!(effort)
                } else {
                    json!("none")
                },
            );
        }
        ThinkingProtocol::Unsloth => {
            object.insert("enable_thinking".into(), json!(enabled));
            if enabled {
                object.insert("reasoning_effort".into(), json!(effort));
            }
            // Local harness tools remain owned by Goose, never executed by the server.
            object.insert("enable_tools".into(), json!(false));
        }
    }
    Ok(())
}
