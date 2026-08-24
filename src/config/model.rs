//! Validated configuration domain types — what `config::validate::validate` produces
//! and everything downstream (prompt rendering, backend dispatch) consumes. Unlike
//! `raw.rs`, invalid states are unrepresentable here: an [`Entry`]'s [`EntryKind`]
//! always carries exactly the fields its type needs, already required-checked.

use std::collections::HashMap;

/// `timeout` under Common fields in prd.md's Configuration section, when an entry
/// doesn't set its own.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// A resolved `prompts.yaml` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: Option<String>,
    pub template: String,
}

/// `prompts.yaml` as a whole.
pub type Prompts = HashMap<String, Prompt>;

/// Where an `openai_api` entry's API key comes from — exactly one of `env`/`value`, per
/// prd.md's `api_key` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeySource {
    Env(String),
    Value(String),
}

/// A validated `type: openai_api` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiApiEntry {
    pub base_url: String,
    pub model: String,
    pub api_key: ApiKeySource,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub headers: HashMap<String, String>,
    pub timeout_secs: u64,
}

/// A validated `type: agent_cli` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCliEntry {
    pub command: String,
    pub model: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub timeout_secs: u64,
}

/// The type-specific payload of one `api.yaml` entry.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    OpenaiApi(OpenAiApiEntry),
    AgentCli(AgentCliEntry),
}

/// One validated `api.yaml` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub enabled: bool,
    pub prompt: String,
    pub kind: EntryKind,
}

/// The fully loaded and cross-validated configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub prompts: Prompts,
    pub entries: Vec<Entry>,
}
