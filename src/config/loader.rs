//! Reading and parsing `prompts.yaml`/`api.yaml` from a config directory, then handing
//! them to `validate.rs`. The only I/O in the `config` module's load path.

use super::model::Config;
use super::raw::{RawEntries, RawPrompts};
use super::validate;
use crate::error::ConfigError;
use serde::de::DeserializeOwned;
use std::path::Path;

/// Loads, parses, and cross-validates `prompts.yaml`/`api.yaml` from `dir`.
///
/// # Errors
/// A [`ConfigError`] (exit 5) if either file is missing/unreadable, fails to parse, or
/// fails cross-validation (see `validate::validate`).
pub fn load(dir: &Path) -> Result<Config, ConfigError> {
    let prompts_path = dir.join("prompts.yaml");
    let api_path = dir.join("api.yaml");
    let raw_prompts: RawPrompts = read_and_parse(&prompts_path)?;
    let raw_entries: RawEntries = read_and_parse(&api_path)?;
    validate::validate(raw_prompts, raw_entries)
}

fn read_and_parse<T: DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    serde_yaml_ng::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_shipped_example_assets() {
        // The exact files --gen-config writes must always be valid config — catches
        // drift between assets/*.yaml and the validator.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("prompts.yaml"),
            include_str!("../../assets/prompts.yaml"),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("api.yaml"),
            include_str!("../../assets/api.yaml"),
        )
        .unwrap();

        let config = load(tmp.path()).unwrap();
        assert_eq!(config.entries.len(), 2);
        assert!(config.prompts.contains_key("default"));
    }

    #[test]
    fn missing_api_yaml_is_a_read_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("prompts.yaml"), "default:\n  template: x\n").unwrap();
        assert!(matches!(load(tmp.path()), Err(ConfigError::Read { .. })));
    }

    #[test]
    fn malformed_yaml_is_a_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("prompts.yaml"), "default:\n  template: x\n").unwrap();
        std::fs::write(tmp.path().join("api.yaml"), "not: [valid, api, yaml\n").unwrap();
        assert!(matches!(load(tmp.path()), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn unknown_yaml_key_is_a_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("prompts.yaml"), "default:\n  template: x\n").unwrap();
        std::fs::write(
            tmp.path().join("api.yaml"),
            "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  api_key:\n    env: K\n  temprature: 0.2\n",
        )
        .unwrap();
        assert!(matches!(load(tmp.path()), Err(ConfigError::Parse { .. })));
    }
}
