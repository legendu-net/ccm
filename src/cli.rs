//! Command-line argument definition and stage-1 ("argument shape") validation.
//!
//! Only checks that don't depend on repo state live here (prd.md "Check order",
//! stage 1: `--include`+`--exclude` together, `--gen-config` combined with anything
//! other than `--config`). Checks that need to know whether the repo is being handled
//! as git or jj (`--git` outside a git repo, `--include`/`--exclude` under git) are
//! stage 3 and live in `pipeline.rs`, once repo detection (stage 2) has run.

use crate::error::UsageError;
use clap::Parser;
use std::path::PathBuf;

/// `ccm` — generate a commit message from the current diff and commit it.
#[derive(Debug, Parser)]
#[command(name = "ccm", version, about, long_about = None)]
pub struct Cli {
    /// Restrict the diff to these files/directories (jj only). Repeatable and/or
    /// space-separated. Mutually exclusive with --exclude.
    #[arg(long, num_args = 1.., value_name = "FILE|DIR")]
    pub include: Vec<PathBuf>,

    /// Exclude these files/directories from the diff (jj only). Repeatable and/or
    /// space-separated. Mutually exclusive with --include.
    #[arg(long, num_args = 1.., value_name = "FILE|DIR")]
    pub exclude: Vec<PathBuf>,

    /// Force git handling in a colocated (git + jj) repository.
    #[arg(long)]
    pub git: bool,

    /// Print the generated commit message to stdout instead of editing/committing.
    #[arg(long)]
    pub dry_run: bool,

    /// Use this directory instead of the default config directory.
    #[arg(long, value_name = "DIR")]
    pub config: Option<PathBuf>,

    /// Create the config directory and example prompts.yaml/api.yaml, then exit.
    #[arg(long)]
    pub gen_config: bool,
}

impl Cli {
    /// Parses `ccm`'s own argv (`std::env::args_os`). clap handles `--help`/`--version`
    /// and malformed-argument errors itself (both `process::exit`), so this only
    /// returns for a successfully-parsed [`Cli`].
    #[must_use]
    pub fn parse_args() -> Self {
        Cli::parse()
    }
}

/// Stage 1 of the check-order pipeline: argument-shape usage errors that don't need to
/// know anything about the repository.
pub fn validate_shape(cli: &Cli) -> Result<(), UsageError> {
    if !cli.include.is_empty() && !cli.exclude.is_empty() {
        return Err(UsageError::IncludeExcludeConflict);
    }
    if cli.gen_config
        && (cli.git || cli.dry_run || !cli.include.is_empty() || !cli.exclude.is_empty())
    {
        return Err(UsageError::GenConfigWithOtherFlags);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Cli {
        Cli {
            include: vec![],
            exclude: vec![],
            git: false,
            dry_run: false,
            config: None,
            gen_config: false,
        }
    }

    #[test]
    fn accepts_a_plain_invocation() {
        assert!(validate_shape(&base()).is_ok());
    }

    #[test]
    fn rejects_include_and_exclude_together() {
        let mut cli = base();
        cli.include = vec![PathBuf::from("a")];
        cli.exclude = vec![PathBuf::from("b")];
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::IncludeExcludeConflict)
        ));
    }

    #[test]
    fn gen_config_alone_is_fine() {
        let mut cli = base();
        cli.gen_config = true;
        assert!(validate_shape(&cli).is_ok());
    }

    #[test]
    fn gen_config_with_config_dir_is_fine() {
        let mut cli = base();
        cli.gen_config = true;
        cli.config = Some(PathBuf::from("/tmp/x"));
        assert!(validate_shape(&cli).is_ok());
    }

    #[test]
    fn gen_config_with_dry_run_is_rejected() {
        let mut cli = base();
        cli.gen_config = true;
        cli.dry_run = true;
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::GenConfigWithOtherFlags)
        ));
    }

    #[test]
    fn gen_config_with_git_is_rejected() {
        let mut cli = base();
        cli.gen_config = true;
        cli.git = true;
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::GenConfigWithOtherFlags)
        ));
    }

    #[test]
    fn gen_config_with_include_is_rejected() {
        let mut cli = base();
        cli.gen_config = true;
        cli.include = vec![PathBuf::from("a")];
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::GenConfigWithOtherFlags)
        ));
    }

    #[test]
    fn cli_parses_a_representative_invocation() {
        let cli = Cli::parse_from([
            "ccm",
            "--include",
            "a",
            "b",
            "--config",
            "/tmp/cfg",
            "--dry-run",
        ]);
        assert_eq!(cli.include, vec![PathBuf::from("a"), PathBuf::from("b")]);
        assert!(cli.exclude.is_empty());
        assert!(cli.dry_run);
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/cfg")));
        assert!(!cli.git);
        assert!(!cli.gen_config);
    }
}
