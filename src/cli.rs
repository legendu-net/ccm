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
    #[arg(short = 'd', long)]
    pub dry_run: bool,

    /// Use this directory instead of the default config directory.
    #[arg(short = 'c', long, value_name = "DIR")]
    pub config: Option<PathBuf>,

    /// Create the config directory and example prompts.yaml/api.yaml, then exit.
    #[arg(short = 'g', long)]
    pub gen_config: bool,

    /// List the tools/APIs configured in api.yaml, then exit.
    #[arg(short = 'l', long)]
    pub list_tools: bool,

    /// Use this api.yaml entry by name for this run, regardless of its `enabled` flag.
    #[arg(short = 't', long, value_name = "NAME")]
    pub tool: Option<String>,
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
        && (cli.git
            || cli.dry_run
            || cli.list_tools
            || cli.tool.is_some()
            || !cli.include.is_empty()
            || !cli.exclude.is_empty())
    {
        return Err(UsageError::GenConfigWithOtherFlags);
    }
    if cli.list_tools
        && (cli.git
            || cli.dry_run
            || cli.tool.is_some()
            || !cli.include.is_empty()
            || !cli.exclude.is_empty())
    {
        return Err(UsageError::ListToolsWithOtherFlags);
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
            list_tools: false,
            tool: None,
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
    fn gen_config_with_list_tools_is_rejected() {
        let mut cli = base();
        cli.gen_config = true;
        cli.list_tools = true;
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::GenConfigWithOtherFlags)
        ));
    }

    #[test]
    fn gen_config_with_tool_is_rejected() {
        let mut cli = base();
        cli.gen_config = true;
        cli.tool = Some("x".into());
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::GenConfigWithOtherFlags)
        ));
    }

    #[test]
    fn list_tools_alone_is_fine() {
        let mut cli = base();
        cli.list_tools = true;
        assert!(validate_shape(&cli).is_ok());
    }

    #[test]
    fn list_tools_with_config_dir_is_fine() {
        let mut cli = base();
        cli.list_tools = true;
        cli.config = Some(PathBuf::from("/tmp/x"));
        assert!(validate_shape(&cli).is_ok());
    }

    #[test]
    fn list_tools_with_dry_run_is_rejected() {
        let mut cli = base();
        cli.list_tools = true;
        cli.dry_run = true;
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::ListToolsWithOtherFlags)
        ));
    }

    #[test]
    fn list_tools_with_tool_is_rejected() {
        let mut cli = base();
        cli.list_tools = true;
        cli.tool = Some("x".into());
        assert!(matches!(
            validate_shape(&cli),
            Err(UsageError::ListToolsWithOtherFlags)
        ));
    }

    #[test]
    fn tool_alone_is_fine() {
        let mut cli = base();
        cli.tool = Some("x".into());
        assert!(validate_shape(&cli).is_ok());
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
        assert!(!cli.list_tools);
        assert_eq!(cli.tool, None);
    }

    #[test]
    fn cli_parses_tool_and_list_tools_flags() {
        let cli = Cli::parse_from(["ccm", "--tool", "OmniRoute"]);
        assert_eq!(cli.tool, Some("OmniRoute".to_string()));

        let cli = Cli::parse_from(["ccm", "--list-tools"]);
        assert!(cli.list_tools);
    }

    #[test]
    fn short_flags_are_equivalent_to_their_long_forms() {
        let cli = Cli::parse_from(["ccm", "-t", "OmniRoute"]);
        assert_eq!(cli.tool, Some("OmniRoute".to_string()));

        let cli = Cli::parse_from(["ccm", "-l"]);
        assert!(cli.list_tools);

        let cli = Cli::parse_from(["ccm", "-g"]);
        assert!(cli.gen_config);

        let cli = Cli::parse_from(["ccm", "-c", "/tmp/cfg"]);
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/cfg")));

        let cli = Cli::parse_from(["ccm", "-d"]);
        assert!(cli.dry_run);
    }
}
