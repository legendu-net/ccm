//! Resolution of "the config directory" (prd.md, `--config` under Flags).
//!
//! Default: `$XDG_CONFIG_HOME/ccm/` if `$XDG_CONFIG_HOME` is set to a non-empty value,
//! otherwise `~/.config/ccm/`. `--config <DIR>` overrides this entirely, given as any
//! path (absolute, or relative to the current working directory).

use crate::env::Environment;
use std::io;
use std::path::{Path, PathBuf};

/// Resolves the effective config directory: `cli_config` if given, otherwise the
/// default described above.
///
/// # Errors
/// Propagates a failure to read the current working directory, which is only needed
/// to resolve a *relative* `--config` path. Callers that already have `cwd` in hand
/// (e.g. after stage 2's repository detection) should use
/// [`resolve_config_dir_from`] instead, to avoid re-querying it.
pub fn resolve_config_dir(cli_config: Option<&Path>, env: &dyn Environment) -> io::Result<PathBuf> {
    let cwd = env.current_dir()?;
    Ok(resolve_config_dir_from(cli_config, &cwd, env))
}

/// Same as [`resolve_config_dir`], but takes an already-resolved `cwd` instead of
/// querying it — pure given its inputs.
#[must_use]
pub fn resolve_config_dir_from(
    cli_config: Option<&Path>,
    cwd: &Path,
    env: &dyn Environment,
) -> PathBuf {
    match cli_config {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        Some(dir) => cwd.join(dir),
        None => default_config_dir(env),
    }
}

fn default_config_dir(env: &dyn Environment) -> PathBuf {
    if let Some(xdg) = env.var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("ccm");
    }
    let home = env.var("HOME").unwrap_or_else(|| "/".to_string());
    PathBuf::from(home).join(".config").join("ccm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::FakeEnvironment;

    #[test]
    fn defaults_to_xdg_config_home_when_set_and_non_empty() {
        let env = FakeEnvironment::new().with_var("XDG_CONFIG_HOME", "/home/u/xdg");
        assert_eq!(
            resolve_config_dir(None, &env).unwrap(),
            PathBuf::from("/home/u/xdg/ccm")
        );
    }

    #[test]
    fn falls_back_to_home_dot_config_when_xdg_unset() {
        let env = FakeEnvironment::new().with_var("HOME", "/home/u");
        assert_eq!(
            resolve_config_dir(None, &env).unwrap(),
            PathBuf::from("/home/u/.config/ccm")
        );
    }

    #[test]
    fn falls_back_to_home_dot_config_when_xdg_is_empty_string() {
        let env = FakeEnvironment::new()
            .with_var("XDG_CONFIG_HOME", "")
            .with_var("HOME", "/home/u");
        assert_eq!(
            resolve_config_dir(None, &env).unwrap(),
            PathBuf::from("/home/u/.config/ccm")
        );
    }

    #[test]
    fn cli_config_absolute_wins_outright() {
        let env = FakeEnvironment::new().with_var("XDG_CONFIG_HOME", "/xdg");
        assert_eq!(
            resolve_config_dir(Some(Path::new("/explicit/dir")), &env).unwrap(),
            PathBuf::from("/explicit/dir")
        );
    }

    #[test]
    fn cli_config_relative_is_joined_to_cwd() {
        let mut env = FakeEnvironment::new();
        env.cwd = PathBuf::from("/work/proj");
        assert_eq!(
            resolve_config_dir(Some(Path::new("rel/dir")), &env).unwrap(),
            PathBuf::from("/work/proj/rel/dir")
        );
    }

    #[test]
    fn resolve_config_dir_from_uses_the_given_cwd_not_the_environments() {
        let env = FakeEnvironment::new(); // env.cwd is "/work", deliberately unused here
        let cwd = Path::new("/elsewhere");
        assert_eq!(
            resolve_config_dir_from(Some(Path::new("rel/dir")), cwd, &env),
            PathBuf::from("/elsewhere/rel/dir")
        );
    }
}
