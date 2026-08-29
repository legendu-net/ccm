//! Exit-code architecture.
//!
//! `ccm` promises the Lua wrapper a stable, documented exit code per distinct failure
//! mode (see "Error Handling" in prd.md). To keep that promise, every stage of the
//! pipeline returns its own narrow error enum (so e.g. a config-loading function
//! cannot accidentally return a diff-generation exit code), and [`CcmError`] is the
//! single place where those per-stage enums are gathered and mapped to an actual
//! [`ExitCode`]. No other module constructs or matches on raw exit-code integers.

/// A validated `ccm` process exit code (0-17, per prd.md "Error Handling").
///
/// A newtype rather than a bare `u8` so a stray literal can't be passed around as an
/// exit code by accident; the only way to produce one is via the `pub const`s below or
/// [`CcmError::exit_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(u8);

impl ExitCode {
    pub const SUCCESS: ExitCode = ExitCode(0);
    pub const UNEXPECTED: ExitCode = ExitCode(1);
    pub const USAGE: ExitCode = ExitCode(2);
    pub const GEN_CONFIG_FAILED: ExitCode = ExitCode(3);
    pub const NOT_A_REPO: ExitCode = ExitCode(4);
    pub const CONFIG_ERROR: ExitCode = ExitCode(5);
    pub const SELECTION_FAILED: ExitCode = ExitCode(6);
    pub const DIFF_FAILED: ExitCode = ExitCode(7);
    pub const NOTHING_TO_DIFF: ExitCode = ExitCode(8);
    pub const API_KEY_FAILED: ExitCode = ExitCode(9);
    pub const CALL_FAILED: ExitCode = ExitCode(10);
    pub const MALFORMED_RESPONSE: ExitCode = ExitCode(11);
    pub const EDITOR_UNAVAILABLE: ExitCode = ExitCode(12);
    pub const EDITOR_ABORTED: ExitCode = ExitCode(13);
    pub const PICKER_CANCELLED: ExitCode = ExitCode(14);
    pub const EMPTY_MESSAGE: ExitCode = ExitCode(15);
    pub const COMMIT_FAILED: ExitCode = ExitCode(16);
    pub const TEMP_FILE_FAILED: ExitCode = ExitCode(17);

    /// The raw process exit status this code corresponds to.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

/// Stage 1 / stage 3: usage errors — invalid or conflicting CLI arguments (exit 2).
#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    #[error("--include and --exclude are mutually exclusive")]
    IncludeExcludeConflict,
    #[error("--gen-config cannot be combined with any flag other than --config")]
    GenConfigWithOtherFlags,
    #[error("--git was passed, but the current directory is not a git repository")]
    GitFlagOutsideGitRepo,
    #[error("--include/--exclude are only supported for jj repositories")]
    IncludeExcludeUnderGit,
    #[error("{entry}: does not match any file in the jj working copy")]
    UnmatchedPath { entry: String },
    #[error("--list-tools cannot be combined with any flag other than --config")]
    ListToolsWithOtherFlags,
}

/// Stage 1 (short-circuit): `--gen-config` failed to create the config directory or
/// write a config file (exit 3).
#[derive(Debug, thiserror::Error)]
#[error("failed to generate config: {0}")]
pub struct GenConfigError(pub String);

/// Stage 2: neither a git nor a jj repository was found (exit 4).
#[derive(Debug, thiserror::Error)]
#[error("not a git or jj repository")]
pub struct NotARepo;

/// Stage 4: `prompts.yaml`/`api.yaml` loading, parsing, or cross-validation failed
/// (exit 5).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("api.yaml is an empty list")]
    Empty,
    #[error("duplicate entry name in api.yaml: {name}")]
    DuplicateName { name: String },
    #[error("api.yaml entry {entry} references unknown prompt {prompt}")]
    UnknownPrompt { entry: String, prompt: String },
    #[error("api.yaml entry {entry}: api_key must set exactly one of env or value")]
    InvalidApiKey { entry: String },
    #[error("api.yaml entry {entry}: unknown type {kind:?} (expected openai_api or agent_cli)")]
    UnknownEntryType { entry: String, kind: String },
    #[error("api.yaml entry {entry}: missing required field {field}")]
    MissingField { entry: String, field: &'static str },
}

/// Stage 5: tool/API selection failed (exit 6) — either every `api.yaml` entry is
/// disabled, or `--tool <NAME>` matched no entry.
#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("no enabled tool/API in api.yaml")]
    NoEnabledTool,
    #[error("no api.yaml entry named {name}")]
    UnknownTool { name: String },
}

/// Stage 6: diff enumeration/generation (exit 7), and nothing to diff (exit 8).
#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    #[error("failed to enumerate working-copy files: {0}")]
    Enumeration(String),
    #[error("failed to generate diff: {0}")]
    Generation(String),
    #[error("nothing to diff")]
    Empty,
}

/// Stage 7: generation — API key resolution (exit 9), the tool/API call itself
/// (exit 10), and response parsing (exit 11).
#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("failed to resolve API key: {0}")]
    ApiKeyResolution(String),
    #[error("{0}")]
    CallFailed(String),
    #[error("malformed response: {0}")]
    MalformedResponse(String),
}

/// Stage 8: editor & commit-message handling — `$EDITOR` resolution (exit 12), temp
/// file creation/write (exit 17), an aborted editor (exit 13), and a blank message
/// (exit 15, reached either directly under `--dry-run` or after editor cleanup).
#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("no usable editor: {0}")]
    Unavailable(String),
    #[error("failed to create or populate the editor temp file: {0}")]
    TempFile(String),
    #[error("editor exited with a non-zero status")]
    Aborted,
    #[error("empty commit message")]
    BlankMessage,
}

/// Stage 8: the user cancelled the jj commit-command picker (EOF on stdin, exit 14).
#[derive(Debug, thiserror::Error)]
#[error("aborted: no jj command selected")]
pub struct PickerCancelled;

/// Stage 8: the final `git commit` / `jj commit`|`describe`|`split` invocation failed
/// (exit 16).
#[derive(Debug, thiserror::Error)]
#[error("commit failed: {0}")]
pub struct CommitError(pub String);

/// The single top-level error type `main` matches on. Every pipeline stage funnels
/// into exactly one variant here via `?`/`From`, and [`CcmError::exit_code`] is the
/// only place that maps a variant to an actual process exit code.
#[derive(Debug, thiserror::Error)]
pub enum CcmError {
    #[error(transparent)]
    Usage(#[from] UsageError),
    #[error(transparent)]
    GenConfig(#[from] GenConfigError),
    #[error(transparent)]
    NotARepo(#[from] NotARepo),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Selection(#[from] SelectionError),
    #[error(transparent)]
    Diff(#[from] DiffError),
    #[error(transparent)]
    Generation(#[from] GenerationError),
    #[error(transparent)]
    Editor(#[from] EditorError),
    #[error(transparent)]
    Picker(#[from] PickerCancelled),
    #[error(transparent)]
    Commit(#[from] CommitError),
    /// Exit 1: the generic/unexpected-error catch-all. Deliberately has no `#[from]`
    /// impl for any error type (in particular not `std::io::Error`) — the only way to
    /// produce this variant is to construct it explicitly at one of the few places
    /// prd.md actually names it (e.g. a saved editor temp file that went missing
    /// before `ccm` could read it back).
    #[error("{0}")]
    Unexpected(String),
}

impl CcmError {
    /// Maps this error to the process exit code documented in prd.md's "Error
    /// Handling" table. Exhaustive by construction: adding a new [`CcmError`] variant
    /// without extending this match is a compile error.
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            CcmError::Unexpected(_) => ExitCode::UNEXPECTED,
            CcmError::Usage(_) => ExitCode::USAGE,
            CcmError::GenConfig(_) => ExitCode::GEN_CONFIG_FAILED,
            CcmError::NotARepo(_) => ExitCode::NOT_A_REPO,
            CcmError::Config(_) => ExitCode::CONFIG_ERROR,
            CcmError::Selection(_) => ExitCode::SELECTION_FAILED,
            CcmError::Diff(DiffError::Enumeration(_) | DiffError::Generation(_)) => {
                ExitCode::DIFF_FAILED
            }
            CcmError::Diff(DiffError::Empty) => ExitCode::NOTHING_TO_DIFF,
            CcmError::Generation(GenerationError::ApiKeyResolution(_)) => ExitCode::API_KEY_FAILED,
            CcmError::Generation(GenerationError::CallFailed(_)) => ExitCode::CALL_FAILED,
            CcmError::Generation(GenerationError::MalformedResponse(_)) => {
                ExitCode::MALFORMED_RESPONSE
            }
            CcmError::Editor(EditorError::Unavailable(_)) => ExitCode::EDITOR_UNAVAILABLE,
            CcmError::Editor(EditorError::Aborted) => ExitCode::EDITOR_ABORTED,
            CcmError::Editor(EditorError::BlankMessage) => ExitCode::EMPTY_MESSAGE,
            CcmError::Editor(EditorError::TempFile(_)) => ExitCode::TEMP_FILE_FAILED,
            CcmError::Picker(_) => ExitCode::PICKER_CANCELLED,
            CcmError::Commit(_) => ExitCode::COMMIT_FAILED,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the full exit-code table from prd.md's "Error Handling" section so a
    /// future refactor that renumbers or drops a code fails a test instead of quietly
    /// breaking the Lua wrapper's branching.
    #[test]
    fn exit_code_table_matches_spec() {
        let cases: Vec<(CcmError, u8)> = vec![
            (UsageError::IncludeExcludeConflict.into(), 2),
            (UsageError::GenConfigWithOtherFlags.into(), 2),
            (UsageError::GitFlagOutsideGitRepo.into(), 2),
            (UsageError::IncludeExcludeUnderGit.into(), 2),
            (UsageError::UnmatchedPath { entry: "x".into() }.into(), 2),
            (UsageError::ListToolsWithOtherFlags.into(), 2),
            (GenConfigError("boom".into()).into(), 3),
            (NotARepo.into(), 4),
            (ConfigError::Empty.into(), 5),
            (ConfigError::DuplicateName { name: "x".into() }.into(), 5),
            (
                ConfigError::UnknownPrompt {
                    entry: "x".into(),
                    prompt: "y".into(),
                }
                .into(),
                5,
            ),
            (ConfigError::InvalidApiKey { entry: "x".into() }.into(), 5),
            (
                ConfigError::UnknownEntryType {
                    entry: "x".into(),
                    kind: "y".into(),
                }
                .into(),
                5,
            ),
            (
                ConfigError::MissingField {
                    entry: "x".into(),
                    field: "y",
                }
                .into(),
                5,
            ),
            (SelectionError::NoEnabledTool.into(), 6),
            (SelectionError::UnknownTool { name: "x".into() }.into(), 6),
            (DiffError::Enumeration("x".into()).into(), 7),
            (DiffError::Generation("x".into()).into(), 7),
            (DiffError::Empty.into(), 8),
            (GenerationError::ApiKeyResolution("x".into()).into(), 9),
            (GenerationError::CallFailed("x".into()).into(), 10),
            (GenerationError::MalformedResponse("x".into()).into(), 11),
            (EditorError::Unavailable("x".into()).into(), 12),
            (EditorError::Aborted.into(), 13),
            (PickerCancelled.into(), 14),
            (EditorError::BlankMessage.into(), 15),
            (CommitError("x".into()).into(), 16),
            (EditorError::TempFile("x".into()).into(), 17),
            (CcmError::Unexpected("x".into()), 1),
        ];
        for (err, expected) in cases {
            assert_eq!(
                err.exit_code().as_u8(),
                expected,
                "wrong exit code for {err:?}"
            );
        }
    }
}
