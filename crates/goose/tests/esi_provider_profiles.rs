use goose::config::Config;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u8,
    profiles: Vec<Profile>,
}

#[derive(Debug, Deserialize)]
struct Profile {
    id: String,
    default_provider: String,
    providers: Vec<Provider>,
    allow_other_goose_providers: bool,
    allow_private_litellm: bool,
    allow_private_forgeloop: bool,
}

#[derive(Debug, Deserialize)]
struct Provider {
    id: String,
    role: String,
    authentication: String,
    credential_owner: String,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn load_manifest() -> Manifest {
    let content =
        fs::read_to_string(repository_root().join("provider-profiles/manifest.json")).unwrap();
    serde_json::from_str(&content).unwrap()
}

fn profile<'a>(manifest: &'a Manifest, id: &str) -> &'a Profile {
    manifest
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .unwrap()
}

fn clean_root_config(profile_name: &str) -> (tempfile::TempDir, Config) {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("config");
    fs::create_dir_all(&config_dir).unwrap();
    let profile_path = repository_root()
        .join("provider-profiles")
        .join(format!("{profile_name}.yaml"));
    let user_config = config_dir.join("config.yaml");
    let secrets = config_dir.join("secrets.yaml");
    let config = Config::new_with_config_paths(vec![profile_path, user_config], secrets).unwrap();
    (root, config)
}

#[test]
fn clean_path_root_uses_team_profile_as_the_runtime_default() {
    let root = tempfile::tempdir().unwrap();
    let root_value = root.path().to_str().unwrap();
    let _guard = env_lock::lock_env([
        ("GOOSE_PATH_ROOT", Some(root_value)),
        ("GOOSE_PROVIDER", None),
        ("GOOSE_MODEL", None),
        ("ESI_PROVIDER_PROFILE", None),
        ("GOOSE_ADDITIONAL_CONFIG_FILES", None),
    ]);

    let config = Config::default();
    assert_eq!(config.get_goose_provider().unwrap(), "chatgpt_codex");
    assert_eq!(config.get_goose_model().unwrap(), "gpt-5.5");
    assert_eq!(
        config.get_param::<String>("ESI_PROVIDER_PROFILE").unwrap(),
        "team"
    );
    assert_eq!(
        config.path(),
        root.path().join("config/config.yaml").display().to_string()
    );
    assert!(!root.path().join("config/config.yaml").exists());
    assert!(!root.path().join("config/secrets.yaml").exists());
}

#[test]
fn team_profile_is_clean_codex_primary_with_claude_alternative() {
    let manifest = load_manifest();
    assert_eq!(manifest.schema_version, 1);
    let team = profile(&manifest, "team");
    assert_eq!(team.default_provider, "chatgpt_codex");
    assert_eq!(
        team.providers
            .iter()
            .map(|provider| (provider.id.as_str(), provider.role.as_str()))
            .collect::<Vec<_>>(),
        vec![("chatgpt_codex", "primary"), ("claude-acp", "alternative")]
    );
    assert!(!team.allow_other_goose_providers);
    assert!(!team.allow_private_litellm);
    assert!(!team.allow_private_forgeloop);
    assert!(team.providers.iter().all(|provider| {
        matches!(
            provider.authentication.as_str(),
            "browser_oauth" | "official_client"
        ) && matches!(
            provider.credential_owner.as_str(),
            "provider_client" | "official_claude_client"
        )
    }));

    let (root, config) = clean_root_config("team");
    assert_eq!(config.get_goose_provider().unwrap(), "chatgpt_codex");
    assert_eq!(config.get_goose_model().unwrap(), "gpt-5.5");
    assert_eq!(
        config.get_param::<String>("ESI_PROVIDER_PROFILE").unwrap(),
        "team"
    );
    assert!(!root.path().join("config/config.yaml").exists());
    assert!(!root.path().join("config/secrets.yaml").exists());
}

#[test]
fn operator_profile_exposes_optional_goose_providers_without_shipping_secrets() {
    let manifest = load_manifest();
    let operator = profile(&manifest, "operator");
    let providers = operator
        .providers
        .iter()
        .map(|provider| provider.id.as_str())
        .collect::<HashSet<_>>();
    for expected in [
        "chatgpt_codex",
        "claude-acp",
        "codex-acp",
        "openai",
        "anthropic",
        "litellm",
        "ollama",
        "lmstudio",
    ] {
        assert!(providers.contains(expected), "missing provider {expected}");
    }
    assert!(operator.allow_other_goose_providers);
    assert!(operator.allow_private_litellm);
    assert!(operator.allow_private_forgeloop);

    let (root, config) = clean_root_config("operator");
    assert_eq!(config.get_goose_provider().unwrap(), "chatgpt_codex");
    assert_eq!(
        config.get_param::<String>("ESI_PROVIDER_PROFILE").unwrap(),
        "operator"
    );
    assert!(!root.path().join("config/config.yaml").exists());
    assert!(!root.path().join("config/secrets.yaml").exists());
}

#[test]
fn profiles_contain_no_private_endpoint_or_credential_material() {
    let profile_dir = repository_root().join("provider-profiles");
    let content = ["manifest.json", "team.yaml", "operator.yaml"]
        .into_iter()
        .map(|name| fs::read_to_string(profile_dir.join(name)).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden in [
        "litellm_host:",
        "litellm_api_key:",
        "forgeloop_server:",
        "forgeloop_server_bearer_token:",
        "/.codex/",
        "/.claude/",
        "access_token:",
        "refresh_token:",
        "api_token:",
    ] {
        assert!(!content.contains(forbidden), "found forbidden {forbidden}");
    }
}
