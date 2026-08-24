//! `type: openai_api` backend, via `litellm-rs`.
//!
//! `litellm-rs`'s free-function `completion()` API rejects per-call `api_key`/
//! `api_base`/`headers`/`timeout` overrides on `CompletionOptions` outright — those
//! must instead be baked into a `ProviderConfig`, wrapped in a `Router`, and installed
//! as the process-wide default runtime *before* calling `completion()`. See
//! `.claude-task/ccm-impl/findings.md` for how this was verified (a live smoke test
//! against a mock OpenAI-compatible server). Two details matter for correctness:
//!
//! - `endpoint_access` must be `PrivateNetwork`: the default `PublicOnly` policy
//!   SSRF-blocks loopback/RFC1918 targets, which would break the primary use case
//!   (prd.md Goal: a local OmniRoute server). `ccm`'s `api.yaml` is user-authored local
//!   config, not untrusted input, so this carries no SSRF risk for `ccm` itself.
//! - `install_default_runtime` is a process-wide `OnceLock` — installable exactly once
//!   per process. `ccm` only ever calls this once per run (Requirement 4's no-fallback
//!   design selects exactly one entry), so this always succeeds in production. Tests
//!   exercising this path drive the real binary as a fresh subprocess per test for the
//!   same reason (see `tests/backend_openai.rs`), rather than linking litellm-rs into
//!   the unit-test process.
//!
//! **Accepted limitation** (escalated to and confirmed by the user — see
//! `.claude-task/ccm-impl/findings.md`): prd.md requires a non-2xx response's body
//! always be surfaced alongside the status for exit 10. litellm-rs only does this when
//! the upstream body is a standard OpenAI-shaped `{"error": {"message": ...}}` JSON
//! envelope; for any other shape (plain text, a differently-shaped JSON error, an HTML
//! error page) it discards the real body internally and reports only `"Server error:
//! <status>"` — there is no way to recover the body afterward, since litellm-rs owns
//! the whole HTTP request/response cycle. OmniRoute and most "OpenAI-compatible"
//! gateways already emit the OpenAI error shape, so this covers the PRD's actual
//! primary use case; pinned by `tests/backend_openai.rs`.

use super::MessageGenerator;
use crate::config::model::{ApiKeySource, OpenAiApiEntry};
use crate::env::Environment;
use crate::error::GenerationError;
use crate::prompt::{self, ResolvedPrompt};
use litellm_rs::config::models::provider::ProviderConfig;
use litellm_rs::core::completion::{
    CompletionOptions, MessageContent, completion, system_message, user_message,
};
use litellm_rs::core::net::ProviderEndpointAccess;
use litellm_rs::core::router::{
    RouterConfig, RuntimeBinding, UnifiedRouter, install_default_runtime,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Drives one `openai_api` entry.
pub struct OpenAiGenerator<'a> {
    entry: &'a OpenAiApiEntry,
    env: &'a dyn Environment,
}

impl<'a> OpenAiGenerator<'a> {
    #[must_use]
    pub fn new(entry: &'a OpenAiApiEntry, env: &'a dyn Environment) -> Self {
        Self { entry, env }
    }
}

impl MessageGenerator for OpenAiGenerator<'_> {
    fn generate(&self, prompt: &ResolvedPrompt<'_>, diff: &str) -> Result<String, GenerationError> {
        let api_key = resolve_api_key(&self.entry.api_key, self.env)?;
        let user_message_text = prompt::openai_user_message(prompt.template, diff);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| {
                GenerationError::CallFailed(format!("failed to start async runtime: {err}"))
            })?;
        runtime.block_on(call(
            self.entry,
            &api_key,
            prompt.system,
            &user_message_text,
        ))
    }
}

/// Resolves `openai_api`'s `api_key:` block (prd.md, `api_key` under `type: openai_api`
/// fields). An `env` variable that is unset *or set to the empty string* is treated the
/// same way — mirroring the spec's own "set to a non-empty value" convention for
/// `$XDG_CONFIG_HOME` — and a literal `value: ""` is rejected the same way, rather than
/// silently reaching the HTTP call with a blank key.
fn resolve_api_key(
    source: &ApiKeySource,
    env: &dyn Environment,
) -> Result<String, GenerationError> {
    match source {
        ApiKeySource::Value(value) if value.is_empty() => Err(GenerationError::ApiKeyResolution(
            "api_key.value is empty".to_string(),
        )),
        ApiKeySource::Value(value) => Ok(value.clone()),
        ApiKeySource::Env(name) => {
            let value = env.var(name).unwrap_or_default();
            if value.is_empty() {
                Err(GenerationError::ApiKeyResolution(format!(
                    "environment variable {name} is not set (or is empty)"
                )))
            } else {
                Ok(value)
            }
        }
    }
}

async fn call(
    entry: &OpenAiApiEntry,
    api_key: &str,
    system: Option<&str>,
    user_message_text: &str,
) -> Result<String, GenerationError> {
    let mut settings = HashMap::new();
    if !entry.headers.is_empty() {
        settings.insert("headers".to_string(), serde_json::json!(entry.headers));
    }

    let provider = ProviderConfig {
        name: "ccm-selected".to_string(),
        provider_type: "openai_compatible".to_string(),
        api_key: api_key.to_string(),
        base_url: Some(entry.base_url.clone()),
        timeout: entry.timeout_secs,
        models: vec![entry.model.clone()],
        endpoint_access: ProviderEndpointAccess::PrivateNetwork,
        settings,
        ..Default::default()
    };

    let router = UnifiedRouter::from_gateway_config(&[provider], Some(RouterConfig::default()))
        .await
        .map_err(|err| {
            GenerationError::CallFailed(format!("failed to configure provider: {err}"))
        })?;

    install_default_runtime(RuntimeBinding::new(Arc::new(router))).map_err(|err| {
        GenerationError::CallFailed(format!("failed to install provider runtime: {err}"))
    })?;

    let mut messages = Vec::new();
    if let Some(system_text) = system {
        messages.push(system_message(system_text));
    }
    messages.push(user_message(user_message_text));

    let options = CompletionOptions {
        max_tokens: entry.max_tokens,
        temperature: entry.temperature,
        ..Default::default()
    };

    // Belt-and-suspenders alongside the provider's own `timeout` (already applied as
    // the reqwest client timeout inside litellm-rs): guards against any router-level
    // retry/cooldown behavior silently extending the effective wall clock beyond what
    // the configured timeout promises.
    let response = tokio::time::timeout(
        Duration::from_secs(entry.timeout_secs),
        completion(&entry.model, messages, Some(options)),
    )
    .await
    .map_err(|_elapsed| {
        GenerationError::CallFailed(format!(
            "request exceeded the configured timeout of {}s",
            entry.timeout_secs
        ))
    })?
    .map_err(|err| GenerationError::CallFailed(err.to_string()))?;

    let choice =
        response.choices.into_iter().next().ok_or_else(|| {
            GenerationError::MalformedResponse("response had no choices".to_string())
        })?;

    match choice.message.content {
        Some(MessageContent::Text(text)) => Ok(text),
        Some(_) => Err(GenerationError::MalformedResponse(
            "response content was not plain text".to_string(),
        )),
        None => Err(GenerationError::MalformedResponse(
            "response had no message content".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::FakeEnvironment;

    #[test]
    fn resolves_a_literal_api_key_value() {
        let env = FakeEnvironment::new();
        let key = resolve_api_key(&ApiKeySource::Value("sk-literal".to_string()), &env).unwrap();
        assert_eq!(key, "sk-literal");
    }

    #[test]
    fn resolves_an_env_api_key() {
        let env = FakeEnvironment::new().with_var("OMNIROUTE_API_KEY", "sk-from-env");
        let key =
            resolve_api_key(&ApiKeySource::Env("OMNIROUTE_API_KEY".to_string()), &env).unwrap();
        assert_eq!(key, "sk-from-env");
    }

    #[test]
    fn empty_literal_api_key_value_is_rejected_like_the_empty_env_case() {
        let env = FakeEnvironment::new();
        let err = resolve_api_key(&ApiKeySource::Value(String::new()), &env).unwrap_err();
        assert!(matches!(err, GenerationError::ApiKeyResolution(_)));
    }

    #[test]
    fn missing_env_api_key_is_api_key_resolution_error() {
        let env = FakeEnvironment::new();
        let err = resolve_api_key(&ApiKeySource::Env("MISSING_VAR".to_string()), &env).unwrap_err();
        assert!(matches!(err, GenerationError::ApiKeyResolution(_)));
    }

    #[test]
    fn empty_string_env_api_key_is_treated_as_unset() {
        let env = FakeEnvironment::new().with_var("EMPTY_VAR", "");
        let err = resolve_api_key(&ApiKeySource::Env("EMPTY_VAR".to_string()), &env).unwrap_err();
        assert!(matches!(err, GenerationError::ApiKeyResolution(_)));
    }
}
