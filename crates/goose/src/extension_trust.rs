//! Host-owned, configuration-bound grants for unrestricted extension execution.
use crate::agents::ExtensionConfig;
use crate::config::Config;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const KEY: &str = "ESI_EXTENSION_TRUST";

fn fingerprint(extension: &ExtensionConfig) -> Result<String> {
    let mut identity = serde_json::to_value(extension)?;
    if let Some(fields) = identity.as_object_mut() {
        // Bundled catalog refreshes presentation independently of execution.
        for field in ["description", "display_name", "bundled"] {
            fields.remove(field);
        }
    }
    identity.sort_all_objects();
    Ok(Sha256::digest(serde_json::to_vec(&identity)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(crate) fn is_trusted(config: &Config, extension: &ExtensionConfig) -> bool {
    let Ok(grants) = config.get_stored_param::<BTreeMap<String, String>>(KEY) else {
        return false;
    };
    fingerprint(extension)
        .ok()
        .is_some_and(|hash| grants.get(&extension.key()) == Some(&hash))
}

pub(crate) fn configured(config: &Config, key: &str) -> Result<ExtensionConfig> {
    let raw: serde_yaml::Mapping = config.get_stored_param("extensions")?;
    let entries = crate::config::extensions::parse_extensions_map(&raw);
    entries
        .into_values()
        .find(|entry| entry.config.key() == key)
        .map(|entry| entry.config)
        .context("Save this extension in Settings before changing Trust")
}

pub(crate) fn set_trusted(config: &Config, key: &str, trusted: bool) -> Result<()> {
    let extension = configured(config, key)?;
    let hash = fingerprint(&extension)?;
    config.update_param(KEY, |mut grants: BTreeMap<String, String>| {
        if trusted {
            grants.insert(key.to_owned(), hash);
        } else {
            grants.remove(key);
        }
        grants
    })?;
    Ok(())
}

pub(crate) fn tool_is_trusted(name: &str) -> bool {
    let (key, tool) = match name.split_once("__") {
        Some(parts) => parts,
        None if matches!(name, "shell" | "write" | "edit" | "tree" | "read_image") => {
            ("developer", name)
        }
        None => return false,
    };
    // Structured approval always retains its exact human-receipt semantics.
    if key == "controller" || (key == "workspaceplan" && tool == "approve") {
        return false;
    }
    let config = Config::global();
    configured(config, key).is_ok_and(|extension| is_trusted(config, &extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_trust_is_exact_revocable_and_not_granted_to_new_entries() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let secrets = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "extensions:\n  fetch:\n    enabled: true\n    type: stdio\n    name: fetch\n    cmd: original\n    args: []\n    description: test\n").unwrap();
        let config = Config::new_with_file_secrets(file.path(), secrets.path()).unwrap();
        let extension = configured(&config, "fetch").unwrap();
        assert!(!is_trusted(&config, &extension));
        set_trusted(&config, "fetch", true).unwrap();
        assert!(is_trusted(&config, &extension));
        let mut relabeled = extension.clone();
        if let ExtensionConfig::Stdio {
            description,
            bundled,
            ..
        } = &mut relabeled
        {
            *description = "Updated catalog description".into();
            *bundled = Some(true);
        }
        assert!(is_trusted(&config, &relabeled));
        let mut changed = extension.clone();
        if let ExtensionConfig::Stdio { cmd, .. } = &mut changed {
            *cmd = "replacement".into();
        }
        assert!(!is_trusted(&config, &changed));
        assert!(set_trusted(&config, "unknown", true).is_err());
        set_trusted(&config, "fetch", false).unwrap();
        assert!(!is_trusted(&config, &extension));
    }
}
