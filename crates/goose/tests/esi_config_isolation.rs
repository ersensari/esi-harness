use goose::config::{paths::Paths, Config};
use std::collections::HashMap;

#[test]
fn discovery_writes_to_the_first_global_config_even_after_a_path_override() {
    // Reproduce the leak into a disposable stand-in for the process config,
    // never the operator's real config. Integration binaries have their own
    // singleton, so this test cannot initialize another test binary's Config.
    let process_root = tempfile::tempdir().unwrap();
    let local_root = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let _guard = env_lock::lock_env([
        ("GOOSE_PATH_ROOT", process_root.path().to_str()),
        ("GOOSE_DISABLE_KEYRING", Some("1")),
        ("GOOSE_ADDITIONAL_CONFIG_FILES", Some("")),
        ("PLUGINS", None),
    ]);
    let global = Config::global();
    let global_path = Paths::in_config_dir("config.yaml");
    std::env::set_var("GOOSE_PATH_ROOT", local_root.path());

    let plugin = project.path().join(".agents/plugins/isolation-fixture");
    let skill = plugin.join("skills/isolation-fixture");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"isolation-fixture"}"#,
    )
    .unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: isolation-fixture\ndescription: fixture\n---\nFixture body",
    )
    .unwrap();

    let found = goose::skills::discover_skills(Some(project.path()));
    assert!(found.iter().any(|entry| entry.name == "isolation-fixture"));
    let plugins: HashMap<String, serde_json::Value> = global.get_param("plugins").unwrap();
    assert!(plugins.contains_key(plugin.to_str().unwrap()));
    assert!(global_path.starts_with(process_root.path()));
    assert!(global_path.is_file());
    assert!(!Paths::in_config_dir("config.yaml").exists());
}
