//! Shared integration-test fixture: a hermetic sandbox (repo + config dir + fake
//! `$PATH`) that drives the real `ccm` binary as a subprocess. Different test files use
//! different subsets of this API by design, so unused-helper warnings are expected and
//! suppressed here rather than in every test file.
#![allow(dead_code)]

use assert_cmd::Command;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

/// A hermetic sandbox for one test: its own repo, config dir, temp dir, and `$PATH`
/// (fixture bin dir first). Never touches the real `$HOME`/`$TMPDIR`/`git`/`jj` global
/// config, and never relies on `std::env::set_var` (unsafe under edition 2024, and
/// racy under `cargo test`'s thread parallelism regardless) — every bit of hermeticity
/// here is set via `Command::env` on the child process.
pub struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    /// Creates a fresh sandbox: `<root>/repo`, `<root>/config`, `<root>/tmp`,
    /// `<root>/bin` all created up front.
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("create fixture tempdir");
        for dir in ["repo", "config", "tmp", "bin"] {
            fs::create_dir_all(root.path().join(dir)).expect("create fixture subdir");
        }
        Fixture { root }
    }

    pub fn repo_dir(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    pub fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.path().join("tmp")
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    pub fn root_path(&self) -> &Path {
        self.root.path()
    }

    /// A path under the fixture root that does not exist yet — useful for
    /// `--gen-config --config <this>` tests that want to observe directory creation.
    pub fn root_config_target(&self) -> PathBuf {
        self.root.path().join("gen-config-target")
    }

    /// Where `--gen-config` (no `--config`) lands by default, given the `HOME`/
    /// `XDG_CONFIG_HOME` this fixture's `ccm()` sets.
    pub fn xdg_config_dir(&self) -> PathBuf {
        self.root.path().join("xdg-config").join("ccm")
    }

    /// A ready-to-run `ccm` invocation: hermetic environment, cwd set to the fixture
    /// repo. Callers add args and assertions.
    pub fn ccm(&self) -> Command {
        let mut cmd = Command::cargo_bin("ccm").expect("find ccm binary");
        cmd.current_dir(self.repo_dir())
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin_dir().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("xdg-config"))
            .env("TMPDIR", self.tmp_dir())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("JJ_CONFIG", self.jj_config_path());
        cmd
    }

    pub fn jj_config_path(&self) -> PathBuf {
        let path = self.root.path().join("jj.toml");
        if !path.exists() {
            fs::write(
                &path,
                "[user]\nname = \"ccm-test\"\nemail = \"ccm-test@example.com\"\n",
            )
            .expect("write jj config");
        }
        path
    }

    /// `panic!`s with an actionable message if `tool` isn't on `$PATH` — tests must
    /// never silently skip for a missing prerequisite.
    /// Symlinks the real `git` binary into this fixture's bin dir, so a test can
    /// restrict `PATH` to just `bin_dir()` (e.g. to hide `nvim`/`vim`/`vi` from the
    /// real `$PATH` and force a genuine "no editor available" outcome) while `git`
    /// itself still resolves for diff generation/commit.
    pub fn symlink_git_into_bin_dir(&self) -> &Self {
        let real_git = String::from_utf8(
            std::process::Command::new("which")
                .arg("git")
                .output()
                .expect("run which git")
                .stdout,
        )
        .expect("which git output is utf8");
        let target = self.bin_dir().join("git");
        if !target.exists() {
            std::os::unix::fs::symlink(real_git.trim(), target).expect("symlink git");
        }
        self
    }

    pub fn require_tool(tool: &str) {
        let found = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .any(|dir| dir.join(tool).is_file());
        assert!(found, "'{tool}' is required on PATH to run this test suite");
    }

    fn git(&self, args: &[&str]) -> &Self {
        let status = StdCommand::new("git")
            .args(args)
            .current_dir(self.repo_dir())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "ccm-test")
            .env("GIT_AUTHOR_EMAIL", "ccm-test@example.com")
            .env("GIT_COMMITTER_NAME", "ccm-test")
            .env("GIT_COMMITTER_EMAIL", "ccm-test@example.com")
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
        self
    }

    fn jj(&self, args: &[&str]) -> &Self {
        let status = StdCommand::new("jj")
            .args(args)
            .current_dir(self.repo_dir())
            .env("JJ_CONFIG", self.jj_config_path())
            .status()
            .expect("run jj");
        assert!(status.success(), "jj {args:?} failed");
        self
    }

    pub fn init_git(&self) -> &Self {
        Self::require_tool("git");
        self.git(&["init", "-q"]);
        self.git(&["config", "commit.gpgsign", "false"]);
        // Baked into the repo's local config (not just the GIT_AUTHOR_*/
        // GIT_COMMITTER_* env vars `self.git()` sets for setup-time commands like this
        // one) — `ccm`'s own `git commit` invocation runs with a from-scratch env via
        // `Fixture::ccm()` and needs an identity to actually commit against.
        self.git(&["config", "user.email", "ccm-test@example.com"]);
        self.git(&["config", "user.name", "ccm-test"]);
        self
    }

    pub fn init_jj(&self) -> &Self {
        Self::require_tool("jj");
        self.jj(&["git", "init", "--no-colocate"]);
        self
    }

    pub fn init_colocated(&self) -> &Self {
        Self::require_tool("git");
        Self::require_tool("jj");
        self.jj(&["git", "init", "--colocate"]);
        self
    }

    /// Runs `jj new -m <message>`: snapshots the current working-copy state into `@`
    /// (which becomes `@-`) and starts a fresh empty `@` on top. A checkpoint, so a
    /// later working-copy edit (e.g. a rename) diffs against real prior content instead
    /// of an empty parent.
    pub fn jj_new(&self, message: &str) -> &Self {
        self.jj(&["new", "-m", message]);
        self
    }

    /// Writes `body` to `<repo>/<rel>`, creating parent directories as needed.
    pub fn write(&self, rel: &str, body: &str) -> &Self {
        let path = self.repo_dir().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(&path, body).expect("write fixture file");
        self
    }

    /// `git add`s a path relative to the repo root.
    pub fn stage(&self, rel: &str) -> &Self {
        self.git(&["add", rel]);
        self
    }

    /// Writes a minimal always-valid `api.yaml`/`prompts.yaml` into `config_dir()`, and
    /// a fake `agent_cli` script (`ccm-test-agent`, on the fixture `$PATH`) that always
    /// succeeds with a fixed commit message — for tests whose focus is elsewhere (repo
    /// detection, diff generation, ...) but still need to get all the way through
    /// stages 4-7 to observe a real success. Not just stage 4/5: an `openai_api` entry
    /// would need a real or mocked HTTP endpoint, which most callers of this helper
    /// don't want to set up.
    pub fn write_valid_config(&self) -> &Self {
        self.write_script("ccm-test-agent", "echo 'feat: test commit message'");
        self.write_config(
            "- name: a\n  type: agent_cli\n  prompt: default\n  command: ccm-test-agent\n  model: m\n  args: []\n",
            "default:\n  template: write a commit message\n",
        )
    }

    pub fn write_config(&self, api_yaml: &str, prompts_yaml: &str) -> &Self {
        fs::write(self.config_dir().join("api.yaml"), api_yaml).expect("write api.yaml");
        fs::write(self.config_dir().join("prompts.yaml"), prompts_yaml)
            .expect("write prompts.yaml");
        self
    }

    /// Writes an executable shell script named `name` into the fixture's `$PATH`
    /// prefix dir, so it shadows any real program of the same name. `#!/bin/sh` is
    /// prepended automatically.
    pub fn write_script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.bin_dir().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fixture script");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod fixture script");
        path
    }

    /// All `CCM_EDITMSG_*` files left behind in the fixture's `$TMPDIR`.
    pub fn editmsg_files(&self) -> Vec<PathBuf> {
        fs::read_dir(self.tmp_dir())
            .expect("read tmp dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("CCM_EDITMSG_"))
            })
            .collect()
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
