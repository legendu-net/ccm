//! Serde-facing shapes for `prompts.yaml`/`api.yaml`, matching prd.md's "Configuration"
//! section field-for-field. Deliberately dumb (every type-specific field is `Option`,
//! regardless of which entry `type` actually requires it) — `config/validate.rs` is
//! where the real "is this shape actually valid" logic lives, kept separate so it can
//! produce precise, entry-scoped [`crate::error::ConfigError`] variants instead of a
//! generic serde parse error for every mistake.
//!
//! `#[serde(deny_unknown_fields)]` throughout: an unrecognized key (e.g. a typo'd
//! `temprature`) is a loud config error (exit 5) rather than a silently-ignored one.

use serde::Deserialize;
use std::collections::HashMap;

fn default_true() -> bool {
    true
}

/// One `prompts.yaml` value.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPrompt {
    pub system: Option<String>,
    pub template: String,
}

/// `prompts.yaml` as a whole: a mapping keyed by prompt name.
pub type RawPrompts = HashMap<String, RawPrompt>;

/// The `api_key:` block of an `openai_api` entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawApiKey {
    pub env: Option<String>,
    pub value: Option<String>,
}

/// One `api.yaml` list entry. All type-specific fields are optional here; which ones
/// are actually required depends on `type` and is enforced in `validate.rs`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub prompt: String,
    pub timeout: Option<u64>,

    // `type: openai_api` fields.
    pub base_url: Option<String>,
    pub api_key: Option<RawApiKey>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub headers: Option<HashMap<String, String>>,

    // `type: agent_cli` fields.
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,

    // Required by both types.
    pub model: Option<String>,
}

/// `api.yaml` as a whole: an ordered list of entries.
pub type RawEntries = Vec<RawEntry>;
