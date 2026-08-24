//! Raw `prompts.yaml`/`api.yaml` → validated [`Config`], and first-enabled selection.
//! Pure — no I/O, no filesystem, no network.
//!
//! Two checks are byte-for-byte, case-sensitive comparisons with no trimming or
//! normalization (prd.md, "Selection"): entry `name` uniqueness, and an entry's
//! `prompt` reference against the `prompts.yaml` keys. Plain Rust `String`/`HashMap`
//! equality is already byte-exact, so no special-casing is needed here — it's pinned by
//! a test below (`omniroute` vs `OmniRoute` are distinct).

use super::model::{
    AgentCliEntry, ApiKeySource, Config, DEFAULT_TIMEOUT_SECS, Entry, EntryKind, OpenAiApiEntry,
    Prompt, Prompts,
};
use super::raw::{RawApiKey, RawEntries, RawEntry, RawPrompts};
use crate::error::{ConfigError, SelectionError};
use std::collections::HashSet;

/// Cross-validates raw `prompts.yaml`/`api.yaml` content into a [`Config`].
///
/// # Errors
/// A [`ConfigError`] (exit 5) for: an empty `api.yaml`, a duplicate entry `name`, an
/// entry referencing an unknown prompt, an unrecognized `type`, a missing
/// type-required field, or an `api_key` that doesn't set exactly one of `env`/`value`.
pub fn validate(raw_prompts: RawPrompts, raw_entries: RawEntries) -> Result<Config, ConfigError> {
    if raw_entries.is_empty() {
        return Err(ConfigError::Empty);
    }

    let mut seen_names: HashSet<String> = HashSet::new();
    let mut entries = Vec::with_capacity(raw_entries.len());
    for raw in raw_entries {
        let name = raw.name.clone();
        if !seen_names.insert(name.clone()) {
            return Err(ConfigError::DuplicateName { name });
        }
        if !raw_prompts.contains_key(&raw.prompt) {
            return Err(ConfigError::UnknownPrompt {
                entry: name,
                prompt: raw.prompt,
            });
        }
        let enabled = raw.enabled;
        let prompt = raw.prompt.clone();
        let kind = build_entry_kind(&name, raw)?;
        entries.push(Entry {
            name,
            enabled,
            prompt,
            kind,
        });
    }

    let prompts: Prompts = raw_prompts
        .into_iter()
        .map(|(name, raw)| {
            (
                name,
                Prompt {
                    system: raw.system,
                    template: raw.template,
                },
            )
        })
        .collect();

    Ok(Config { prompts, entries })
}

fn build_entry_kind(name: &str, raw: RawEntry) -> Result<EntryKind, ConfigError> {
    let timeout_secs = raw.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
    match raw.kind.as_str() {
        "openai_api" => {
            let base_url = require_non_empty(name, "base_url", raw.base_url)?;
            let model = require_non_empty(name, "model", raw.model)?;
            let raw_api_key = require(name, "api_key", raw.api_key)?;
            let api_key = build_api_key(name, raw_api_key)?;
            Ok(EntryKind::OpenaiApi(OpenAiApiEntry {
                base_url,
                model,
                api_key,
                max_tokens: raw.max_tokens,
                temperature: raw.temperature,
                headers: raw.headers.unwrap_or_default(),
                timeout_secs,
            }))
        }
        "agent_cli" => {
            let command = require_non_empty(name, "command", raw.command)?;
            let model = require_non_empty(name, "model", raw.model)?;
            let args = require(name, "args", raw.args)?;
            Ok(EntryKind::AgentCli(AgentCliEntry {
                command,
                model,
                args,
                env: raw.env.unwrap_or_default(),
                timeout_secs,
            }))
        }
        other => Err(ConfigError::UnknownEntryType {
            entry: name.to_string(),
            kind: other.to_string(),
        }),
    }
}

fn require<T>(entry: &str, field: &'static str, value: Option<T>) -> Result<T, ConfigError> {
    value.ok_or_else(|| ConfigError::MissingField {
        entry: entry.to_string(),
        field,
    })
}

/// Like [`require`], but for string fields that must also be non-blank — `base_url:
/// ""`/`command: ""`/`model: ""` pass `Option`-presence validation trivially but are
/// just as unusable as leaving the field out, and would otherwise only surface as an
/// opaque failure deep inside a backend instead of a fast, specific config error.
fn require_non_empty(
    entry: &str,
    field: &'static str,
    value: Option<String>,
) -> Result<String, ConfigError> {
    let value = require(entry, field, value)?;
    if value.trim().is_empty() {
        return Err(ConfigError::MissingField {
            entry: entry.to_string(),
            field,
        });
    }
    Ok(value)
}

fn build_api_key(entry: &str, raw: RawApiKey) -> Result<ApiKeySource, ConfigError> {
    match (raw.env, raw.value) {
        (Some(env), None) => Ok(ApiKeySource::Env(env)),
        (None, Some(value)) => Ok(ApiKeySource::Value(value)),
        _ => Err(ConfigError::InvalidApiKey {
            entry: entry.to_string(),
        }),
    }
}

/// Stage 5 of the check-order pipeline: `api.yaml` is a priority-ordered list, and
/// `ccm` selects the first entry with `enabled: true` — nothing more (prd.md
/// "Selection"). Pure.
///
/// # Errors
/// [`SelectionError`] (exit 6) if every entry is disabled.
pub fn select_first_enabled(entries: &[Entry]) -> Result<&Entry, SelectionError> {
    entries
        .iter()
        .find(|entry| entry.enabled)
        .ok_or(SelectionError)
}

#[cfg(test)]
mod tests {
    use super::super::raw::{RawApiKey, RawEntry, RawPrompt};
    use super::*;
    use std::collections::HashMap;

    fn prompts_with(name: &str) -> RawPrompts {
        let mut map = HashMap::new();
        map.insert(
            name.to_string(),
            RawPrompt {
                system: None,
                template: "write a message".to_string(),
            },
        );
        map
    }

    fn openai_entry(name: &str, prompt: &str) -> RawEntry {
        RawEntry {
            name: name.to_string(),
            kind: "openai_api".to_string(),
            enabled: true,
            prompt: prompt.to_string(),
            timeout: None,
            base_url: Some("http://localhost:1234/v1".to_string()),
            api_key: Some(RawApiKey {
                env: Some("KEY".to_string()),
                value: None,
            }),
            max_tokens: Some(300),
            temperature: Some(0.2),
            headers: None,
            command: None,
            args: None,
            env: None,
            model: Some("gpt-5.4".to_string()),
        }
    }

    fn agent_cli_entry(name: &str, prompt: &str) -> RawEntry {
        RawEntry {
            name: name.to_string(),
            kind: "agent_cli".to_string(),
            enabled: true,
            prompt: prompt.to_string(),
            timeout: None,
            base_url: None,
            api_key: None,
            max_tokens: None,
            temperature: None,
            headers: None,
            command: Some("gemini".to_string()),
            args: Some(vec!["-p".to_string(), "{{prompt}}".to_string()]),
            env: None,
            model: Some("gemini-2.5-pro".to_string()),
        }
    }

    #[test]
    fn valid_config_round_trips_into_the_model() {
        let config = validate(prompts_with("default"), vec![openai_entry("a", "default")]).unwrap();
        assert_eq!(config.entries.len(), 1);
        assert!(config.prompts.contains_key("default"));
        match &config.entries[0].kind {
            EntryKind::OpenaiApi(e) => {
                assert_eq!(e.base_url, "http://localhost:1234/v1");
                assert_eq!(e.timeout_secs, DEFAULT_TIMEOUT_SECS);
                assert_eq!(e.api_key, ApiKeySource::Env("KEY".to_string()));
            }
            EntryKind::AgentCli(_) => panic!("expected openai_api"),
        }
    }

    #[test]
    fn agent_cli_entry_validates() {
        let config = validate(
            prompts_with("default"),
            vec![agent_cli_entry("a", "default")],
        )
        .unwrap();
        match &config.entries[0].kind {
            EntryKind::AgentCli(e) => {
                assert_eq!(e.command, "gemini");
                assert_eq!(e.args, vec!["-p", "{{prompt}}"]);
            }
            EntryKind::OpenaiApi(_) => panic!("expected agent_cli"),
        }
    }

    #[test]
    fn empty_entries_is_an_error() {
        assert!(matches!(
            validate(prompts_with("default"), vec![]),
            Err(ConfigError::Empty)
        ));
    }

    #[test]
    fn duplicate_names_are_byte_exact_case_sensitive() {
        // "omniroute" and "OmniRoute" must count as distinct, not a duplicate.
        let entries = vec![
            openai_entry("omniroute", "default"),
            openai_entry("OmniRoute", "default"),
        ];
        assert!(validate(prompts_with("default"), entries).is_ok());
    }

    #[test]
    fn a_true_duplicate_name_is_rejected() {
        let entries = vec![
            openai_entry("omniroute", "default"),
            openai_entry("omniroute", "default"),
        ];
        assert!(matches!(
            validate(prompts_with("default"), entries),
            Err(ConfigError::DuplicateName { name }) if name == "omniroute"
        ));
    }

    #[test]
    fn unknown_prompt_reference_is_byte_exact_too() {
        // prompts.yaml has "Default" (capital D); the entry references "default".
        let entries = vec![openai_entry("a", "default")];
        assert!(matches!(
            validate(prompts_with("Default"), entries),
            Err(ConfigError::UnknownPrompt { .. })
        ));
    }

    #[test]
    fn unknown_entry_type_is_rejected() {
        let mut entry = openai_entry("a", "default");
        entry.kind = "not_a_real_type".to_string();
        assert!(matches!(
            validate(prompts_with("default"), vec![entry]),
            Err(ConfigError::UnknownEntryType { .. })
        ));
    }

    #[test]
    fn missing_required_field_is_rejected() {
        let mut entry = openai_entry("a", "default");
        entry.base_url = None;
        assert!(matches!(
            validate(prompts_with("default"), vec![entry]),
            Err(ConfigError::MissingField {
                field: "base_url",
                ..
            })
        ));
    }

    #[test]
    fn empty_string_base_url_is_rejected_like_a_missing_one() {
        let mut entry = openai_entry("a", "default");
        entry.base_url = Some(String::new());
        assert!(matches!(
            validate(prompts_with("default"), vec![entry]),
            Err(ConfigError::MissingField {
                field: "base_url",
                ..
            })
        ));
    }

    #[test]
    fn whitespace_only_model_is_rejected() {
        let mut entry = openai_entry("a", "default");
        entry.model = Some("   ".to_string());
        assert!(matches!(
            validate(prompts_with("default"), vec![entry]),
            Err(ConfigError::MissingField { field: "model", .. })
        ));
    }

    #[test]
    fn empty_string_agent_cli_command_is_rejected() {
        let mut entry = agent_cli_entry("a", "default");
        entry.command = Some(String::new());
        assert!(matches!(
            validate(prompts_with("default"), vec![entry]),
            Err(ConfigError::MissingField {
                field: "command",
                ..
            })
        ));
    }

    #[test]
    fn api_key_with_neither_env_nor_value_is_rejected() {
        let mut entry = openai_entry("a", "default");
        entry.api_key = Some(RawApiKey {
            env: None,
            value: None,
        });
        assert!(matches!(
            validate(prompts_with("default"), vec![entry]),
            Err(ConfigError::InvalidApiKey { .. })
        ));
    }

    #[test]
    fn api_key_with_both_env_and_value_is_rejected() {
        let mut entry = openai_entry("a", "default");
        entry.api_key = Some(RawApiKey {
            env: Some("K".to_string()),
            value: Some("v".to_string()),
        });
        assert!(matches!(
            validate(prompts_with("default"), vec![entry]),
            Err(ConfigError::InvalidApiKey { .. })
        ));
    }

    #[test]
    fn entry_timeout_overrides_the_default() {
        let mut entry = openai_entry("a", "default");
        entry.timeout = Some(120);
        let config = validate(prompts_with("default"), vec![entry]).unwrap();
        match &config.entries[0].kind {
            EntryKind::OpenaiApi(e) => assert_eq!(e.timeout_secs, 120),
            EntryKind::AgentCli(_) => unreachable!(),
        }
    }

    // ---- select_first_enabled ----

    #[test]
    fn selects_the_first_enabled_entry() {
        let mut disabled = openai_entry("first", "default");
        disabled.enabled = false;
        let config = validate(
            prompts_with("default"),
            vec![disabled, openai_entry("second", "default")],
        )
        .unwrap();
        let selected = select_first_enabled(&config.entries).unwrap();
        assert_eq!(selected.name, "second");
    }

    #[test]
    fn no_enabled_entries_is_a_selection_error() {
        let mut entry = openai_entry("a", "default");
        entry.enabled = false;
        let config = validate(prompts_with("default"), vec![entry]).unwrap();
        assert!(select_first_enabled(&config.entries).is_err());
    }

    #[test]
    fn first_enabled_wins_over_a_later_enabled_entry() {
        let config = validate(
            prompts_with("default"),
            vec![
                openai_entry("first", "default"),
                openai_entry("second", "default"),
            ],
        )
        .unwrap();
        let selected = select_first_enabled(&config.entries).unwrap();
        assert_eq!(selected.name, "first");
    }
}
