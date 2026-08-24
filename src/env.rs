//! An injectable view of process environment (cwd, env vars, `$PATH` lookups, the OS
//! temp directory).
//!
//! Nothing in this crate reads `std::env` directly outside of [`RealEnvironment`].
//! Every other module takes `&dyn Environment` (or a concrete injected value), which is
//! what lets tests exercise env-dependent behavior (`$EDITOR` resolution, `$XDG_CONFIG_HOME`
//! defaulting, `$PATH` lookups) without mutating real process state — edition 2024 made
//! `std::env::set_var` `unsafe`, and it would be racy under `cargo test`'s thread
//! parallelism regardless.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// An injectable view of the bits of process environment `ccm` depends on.
pub trait Environment {
    /// The current working directory.
    fn current_dir(&self) -> io::Result<PathBuf>;

    /// The value of an environment variable, if set.
    fn var(&self, key: &str) -> Option<String>;

    /// The OS temp directory (`$TMPDIR` if set, `/tmp` otherwise).
    fn temp_dir(&self) -> PathBuf;

    /// Resolves `program` the way a shell would: if it contains a `/`, treat it as a
    /// path (absolute or relative to [`Environment::current_dir`]) and check it exists
    /// and is executable; otherwise search `$PATH` in order for an executable of that
    /// name. Returns the resolved path, or `None` if nothing matched.
    fn find_program(&self, program: &str) -> Option<PathBuf>;
}

/// The real process environment, backed directly by `std::env`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealEnvironment;

impl Environment for RealEnvironment {
    fn current_dir(&self) -> io::Result<PathBuf> {
        std::env::current_dir()
    }

    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }

    fn find_program(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            let path = PathBuf::from(program);
            let candidate = if path.is_absolute() {
                path
            } else {
                self.current_dir().ok()?.join(path)
            };
            return is_executable_file(&candidate).then_some(candidate);
        }
        let path_var = self.var("PATH")?;
        std::env::split_paths(&path_var).find_map(|dir| {
            let candidate = dir.join(program);
            is_executable_file(&candidate).then_some(candidate)
        })
    }
}

/// A fully in-memory [`Environment`] for unit tests: no filesystem or real env access
/// at all (aside from what the test itself sets up via `programs`).
#[derive(Debug, Default, Clone)]
pub struct FakeEnvironment {
    pub cwd: PathBuf,
    pub vars: std::collections::HashMap<String, String>,
    pub temp_dir: PathBuf,
    /// Bare program names resolvable via a `$PATH`-style lookup, in search order.
    pub path_programs: Vec<(String, PathBuf)>,
}

impl FakeEnvironment {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cwd: PathBuf::from("/work"),
            vars: std::collections::HashMap::new(),
            temp_dir: PathBuf::from("/tmp"),
            path_programs: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_var(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(key.to_string(), value.to_string());
        self
    }

    #[must_use]
    pub fn with_program(mut self, name: &str, path: &str) -> Self {
        self.path_programs
            .push((name.to_string(), PathBuf::from(path)));
        self
    }
}

impl Environment for FakeEnvironment {
    fn current_dir(&self) -> io::Result<PathBuf> {
        Ok(self.cwd.clone())
    }

    fn var(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn temp_dir(&self) -> PathBuf {
        self.temp_dir.clone()
    }

    fn find_program(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            return self
                .path_programs
                .iter()
                .find(|(name, _)| name == program)
                .map(|(_, path)| path.clone());
        }
        self.path_programs
            .iter()
            .find(|(name, _)| name == program)
            .map(|(_, path)| path.clone())
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_environment_resolves_registered_program() {
        let env = FakeEnvironment::new().with_program("nvim", "/usr/bin/nvim");
        assert_eq!(
            env.find_program("nvim"),
            Some(PathBuf::from("/usr/bin/nvim"))
        );
        assert_eq!(env.find_program("vim"), None);
    }

    #[test]
    fn fake_environment_var_lookup() {
        let env = FakeEnvironment::new().with_var("EDITOR", "code --wait");
        assert_eq!(env.var("EDITOR").as_deref(), Some("code --wait"));
        assert_eq!(env.var("MISSING"), None);
    }

    #[test]
    fn real_environment_finds_a_program_that_must_exist_in_ci() {
        // `sh` is about as safe a bet as any for "definitely on PATH".
        let env = RealEnvironment;
        assert!(env.find_program("sh").is_some());
        assert!(
            env.find_program("definitely-not-a-real-program-xyz")
                .is_none()
        );
    }

    #[test]
    fn real_environment_resolves_an_absolute_path_directly_bypassing_path() {
        // `/bin/sh` (or a symlink to it) exists on any POSIX system this crate targets.
        // This specifically exercises the slash-containing branch of find_program,
        // which the FakeEnvironment double (used everywhere else in this crate's
        // tests) can't stand in for: it treats every program name as an opaque
        // registered key, regardless of whether it contains a slash.
        let env = RealEnvironment;
        assert_eq!(env.find_program("/bin/sh"), Some(PathBuf::from("/bin/sh")));
    }

    #[test]
    fn real_environment_rejects_an_absolute_path_that_does_not_exist() {
        let env = RealEnvironment;
        assert_eq!(env.find_program("/definitely/not/a/real/binary"), None);
    }

    #[test]
    fn real_environment_relative_slash_path_that_does_not_exist_returns_none() {
        // Exercises the other half of the slash-containing branch (joined against
        // current_dir() rather than treated as already-absolute) — a relative path
        // pointing nowhere real must not resolve.
        let env = RealEnvironment;
        assert_eq!(env.find_program("./no/such/relative/binary"), None);
    }
}
