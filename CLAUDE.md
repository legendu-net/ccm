# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`ccm` ("Contextual Commit Message") is a Rust CLI that generates a commit message from
the current diff (via an OpenAI-compatible API or a local agent CLI), lets the user
regenerate/edit/accept it, and commits it — for both Git and Jujutsu (jj) repositories,
including colocated ones. Linux/macOS only.

**`prd.md` is the normative spec.** It is long (~1300 lines) and extremely detailed —
exact wording of every prompt, every exit code's precise trigger, every progress-log
line, argument-shape usage errors, etc. Source comments throughout `src/` cite it by
section name (e.g. `// prd.md "Diff scope resolution"`). When changing behavior:
1. Read the relevant `prd.md` section first — it is usually more precise than what the
   code "obviously" should do.
2. Update `prd.md` in the same change if behavior changes. It is treated as
   documentation, not an artifact that trails the code — several sections cross-reference
   each other by heading name (`see "Selection"`, `see "Interactive terminal
   requirement"`), so a partial update leaves it internally inconsistent.
3. `README.md` is a shorter, user-facing summary of the same behavior (flags, exit
   codes, config). Keep it in sync too, but `prd.md` is authoritative on any conflict.

## Commands

```sh
cargo build
cargo test                                    # full suite: unit + every tests/*.rs binary
cargo test --lib                              # unit tests only (fast, colocated in src/)
cargo test --test diff_jj_scope               # one integration test binary
cargo test some_test_name                     # a single test by (substring) name
cargo fmt
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

There is **no `.github/workflows`** in this repo — `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, and `cargo test` are the entire verification bar; run all
three before considering a change done. No custom `rustfmt.toml`/`clippy.toml` — plain
defaults.

`ccm --gen-config -c <dir>` and `ccm --dry-run` are useful for manually exercising a
build without touching the real `~/.config/ccm/`.

## Architecture

### The pipeline is one linear function

`src/pipeline.rs`'s `run_with` is the entire program logic: a fixed, numbered
"check-order" sequence of stages (argument shape → repo detection →
repo-dependent validation → config load → tool/API selection → diff generation → message
generation → review/editor/commit), each stage a `?`-early-return so the *source order
of that function is itself the normative check order* — the first failing stage wins,
matching `prd.md`'s "Check order" table exactly. When adding a stage or a new failure
mode, place it at the correct point in this sequence and check `prd.md`'s check-order
table for where that is.

`src/main.rs` is deliberately thin: parse args, call `pipeline::run`, map any error to
its exit code (`src/error.rs`'s `CcmError::exit_code`), print, exit. Every failure mode
maps to one documented exit code (see the table in `README.md`) — `error.rs` is the
single source of truth for that mapping; extend the `CcmError`/stage-specific error enums
there rather than reusing an unrelated variant for a new failure.

### Everything effectful is injected — production code has two entry points per seam

- `env::Environment` (a trait) abstracts `std::env`, `$PATH` lookups, `stdin_is_terminal`,
  the OS temp dir. Nothing outside `env::RealEnvironment` touches `std::env` directly.
- `fzf::Fzf` (a trait) abstracts the optional `fzf` subprocess front-end for interactive
  pickers. `fzf::RealFzf` shells out for real; tests substitute fakes.
- `pipeline::run` (production: `RealEnvironment` + `RealFzf`, real `stdin`/`stdout`/
  `stderr`) vs. `pipeline::run_with` (same logic, everything injected) exists purely so
  tests can drive the pipeline without a real terminal or real `fzf` binary. `run` itself
  has essentially no logic — never add behavior to it that isn't in `run_with`.

This is why unit tests can exercise "was the tool picker/file picker even attempted"
gating logic (see `pipeline.rs`'s `PanickingFzf`/`FakeFzf`/`RecordingFzf`/`FakeFilesFzf`
test doubles) without spawning a real `fzf` process, and why env-dependent behavior
(`$EDITOR` resolution, `$XDG_CONFIG_HOME` defaulting) is tested via `FakeEnvironment`
rather than mutating real process state (also: `std::env::set_var` is `unsafe` on this
edition and would be racy under `cargo test`'s thread parallelism regardless).

### git vs. jj: a two-armed match, not a trait

`repo.rs` classifies the working directory into `RepoContext`/`RepoHandling` (`Git` or
`Jj`, resolving colocation + `--git`). There is **no VCS trait** — `diff.rs` and
`commit.rs` each `match` on `RepoHandling` directly. This is deliberate (only ever two
variants); don't introduce a trait abstraction here without a real third backend to
justify it.

### `vcs/` — subprocess argv, execution, and jj scope resolution

- `vcs/argv.rs` — pure argv *builders* (`Vec<String>`) for every git/jj invocation. Never
  a shell command string — substitution (message, file paths) is always a literal
  `Command::arg()`, so an unescaped multi-line message or a path with spaces/shell
  metacharacters is safe. `render_command` (via `shell_words`) only *renders* args back
  into a display string for progress logging — note `shell_words` quotes anything
  containing `=` (e.g. `--color=never` renders as `'--color=never'`), which matters if
  you're pinning an exact progress-line string in a test.
- `vcs/exec.rs` — the one place `ccm` spawns and captures a subprocess (git/jj/agent_cli;
  *not* `$EDITOR`, which needs the inherited controlling terminal instead of captured
  stdio, and *not* `fzf`, for the same reason — see `fzf.rs`'s module doc for why `fzf`
  deliberately avoids this module's `process_group(0)` isolation). Pumps
  stdin/stdout/stderr on separate threads to avoid a 64 KiB-pipe deadlock on a large diff;
  enforces `agent_cli`'s configured `timeout` via SIGTERM-then-SIGKILL.
- `vcs/summary.rs` — parses `argv::jj_enumerate_args`'s `-T` template output, one
  `<status><SEP><source><SEP><target>` record per line. This replaced an earlier approach
  that ran `jj diff --summary` and parsed its human-oriented `R prefix{old => new}`
  rename/copy brace form, which turned out to be genuinely ambiguous (jj doesn't escape a
  literal `{`/`}` a filename itself contains) — see the module doc for why.
- `vcs/scope.rs` — resolves a `Selection` (`All` / `Include` / `Exclude` / `Explicit`)
  against parsed summary entries into the final file-argument list for `jj diff`/`jj
  commit`. A rename/copy is *matched* by either old or new name but always *contributes*
  the new name (old no longer exists to diff).
- `vcs/pathnorm.rs` — purely lexical path normalization (no filesystem access) so
  `--include`/`--exclude`/the interactive file picker compare consistently against jj's
  enumeration output.

### `config/` — three-layer validation

`raw.rs` (serde structs, every type-specific field `Option`, `deny_unknown_fields`) →
`validate.rs` (cross-validates into `model.rs`'s types, where invalid states are
unrepresentable — e.g. `EntryKind::OpenaiApi`/`AgentCli` instead of a struct with optional
fields for both) → `model.rs`. Plus `paths.rs` (config dir resolution), `loader.rs` (the
only I/O — reads+validates `prompts.yaml`/`api.yaml`), `listing.rs` (the shared
`"<status>  <name>  <default marker>"` line format used by `--list-tools`, the numbered
stdin picker, and the `fzf` picker), `gen_config.rs` (`--gen-config` scaffolding).

### `backend/` — pluggable message generation

`MessageGenerator` trait, two implementations: `openai_api` (via `litellm-rs`) and
`agent_cli` (a subprocess). Both return a `GenerationOutcome { message, resolved_model }`
— blank-message handling is a caller concern (`--dry-run` vs. the editor-review-loop
handle it differently), not the trait's.

### Interactive pickers: one consistent shape, three call sites

`picker.rs` (tool picker's numbered fallback, message review prompt, jj commit-command
picker, file-picker yes/no + numbered fallback) and `fzf.rs` (optional fuzzy/multi-select
front-ends for the tool picker and file picker) all follow the same pattern:
- A pure `interpret_*(byte_or_line, ...) -> Option<Action>` classifier, fully unit-tested
  with no I/O.
- A thin loop around it that prints the menu, reads one key/line via a shared `read_key`
  (raw single-keypress mode on a real terminal, whole-line otherwise), and reprints on an
  unrecognized answer.
- EOF or Ctrl-D always maps to `PickerError::Cancelled` → exit 14 (`picker::to_ccm_error`)
  — distinct from a *blank* line, which usually selects a stated default instead.
- All prompt output goes to **stderr** via `progress.rs`, never stdout (stdout is
  reserved for `--dry-run`'s message and `--gen-config`'s report — pinned by
  `tests/stdio.rs`).

`fzf` itself is a purely optional, runtime-detected subprocess — no `Cargo.toml`
dependency either way, same philosophy as the `$EDITOR` → `nvim`/`vim`/`vi` fallback
chain. Only attempted when stdin is a real terminal and `$CCM_FUZZY != "0"`; any failure
to run falls back to the numbered stdin prompt (`progress::fzf_unavailable` logs why).

## Testing patterns

- Unit tests are colocated in each `src/*.rs` module (`#[cfg(test)] mod tests`), and
  favor real subprocesses over mocks for git/jj (spin up a real repo in a `tempfile`
  TempDir — see the `git_repo()`/`jj_repo()` helpers repeated across `diff.rs`,
  `commit.rs`, `pipeline.rs`, `fileselect.rs`) — this is a deliberate project preference
  (see `prd.md`'s "Preferences of Dependencies"), not an oversight to "fix" by mocking.
- `tests/*.rs` integration tests drive the **real compiled binary** as a subprocess via
  `assert_cmd`, using the shared `Fixture` helper in `tests/common/mod.rs` (repo/config
  scaffolding, a sanitized `$PATH`, fake `$EDITOR` scripts). `tests/backend_openai.rs` is
  the one place a network dependency is actually mocked, via `wiremock`.
- **A test whose config sends real traffic to an unreachable `openai_api` backend (e.g.
  `base_url: http://x`) must set an explicit short `timeout` (in seconds) in that
  fake `api.yaml` entry.** The default is 60s, and a test that reaches message generation
  without bounding it risks a very slow (or, in a network-restricted sandbox,
  unpredictable) test run. Search `pipeline.rs` for `timeout: 1` for the established
  pattern.
- Prefer asserting on exact stderr progress-line text (`progress.rs`'s functions are the
  single source of that wording) over asserting only on exit codes — several existing
  tests use a stage's own "nothing to diff" exit 8 purely as a cheap way to stop the
  pipeline early without needing a real backend; don't mistake that for the thing under
  test.
