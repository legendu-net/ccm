//! Message generation backends: `openai_api` (`litellm-rs`) and `agent_cli` (a
//! subprocess). Both implement [`MessageGenerator`], the seam that lets pipeline-level
//! logic (blank-message handling, progress logging) be tested against a stub with no
//! network or subprocess involved at all.

pub mod agent_cli;
pub mod openai;

use crate::error::GenerationError;
use crate::prompt::ResolvedPrompt;

/// Generates a raw commit message from a diff. Implementations return whatever the
/// tool/API produced verbatim, including a blank result — the blank-message check
/// (exit 15) is a caller concern (it differs between `--dry-run` and the default
/// editor flow), not this trait's.
pub trait MessageGenerator {
    /// # Errors
    /// A [`GenerationError`] — API key resolution (exit 9), the call itself (exit 10),
    /// or response parsing (exit 11, `openai_api` only).
    fn generate(&self, prompt: &ResolvedPrompt<'_>, diff: &str) -> Result<String, GenerationError>;
}
