# ccm — Contextual Commit Message

`ccm` generates a commit message from your current diff (via an OpenAI-compatible API
or a local agent CLI), opens it in `$EDITOR` for review, and commits it — for both Git
and Jujutsu (jj) repositories, including colocated ones. See `prd.md` for the full
normative specification this implementation follows.

Supported platforms: Linux and macOS. Windows is out of scope.

## Quick start

```sh
ccm --gen-config          # writes prompts.yaml/api.yaml into the default config dir
$EDITOR ~/.config/ccm/api.yaml   # fill in a base_url/api_key (or an agent CLI command)
ccm                       # generate a message, edit it, commit it
ccm --dry-run             # just print the generated message to stdout
```

## Configuration

Config directory: `$XDG_CONFIG_HOME/ccm/` if set to a non-empty value, otherwise
`~/.config/ccm/` — or the directory passed via `--config <DIR>`. It holds two files:

- **`prompts.yaml`** — named prompt templates, referenced by `api.yaml` entries.
- **`api.yaml`** — a priority-ordered list of tools/APIs to try. `ccm` selects the
  first entry with `enabled: true` and calls it; there is no fallback.

Run `ccm --gen-config` to create the directory and write filled example versions of
both files (never overwriting an existing one). See `prd.md`'s "Configuration" section
for the full field reference (`type: openai_api` vs `type: agent_cli`, `headers`,
`timeout`, `{{prompt}}`/`{{model}}`/`{{system}}` substitution, etc.).

Run `ccm --list-tools` to see what's configured. `--tool <NAME>` picks a named entry
outright for a single run (even a disabled one). Absent `--tool`, `ccm` uses the first
enabled entry whenever that's unambiguous; in default mode (not `--dry-run`), if zero or
2+ entries are enabled, it prompts you to pick one from a numbered menu instead.

## CLI flags

| Flag | Meaning |
|---|---|
| `--include <FILE\|DIR>...` | Restrict the diff to these paths (jj only). |
| `--exclude <FILE\|DIR>...` | Exclude these paths from the diff (jj only). |
| `--git` | Force git handling in a colocated (git + jj) repository. |
| `-d`, `--dry-run` | Print the generated message to stdout; don't open `$EDITOR` or commit. |
| `-c`, `--config <DIR>` | Use `<DIR>` as the config directory instead of the default. |
| `-g`, `--gen-config` | Create the config directory and example files, then exit. |
| `-l`, `--list-tools` | List the `api.yaml` entries (name, type, enabled/disabled), then exit. |
| `-t`, `--tool <NAME>` | Use this `api.yaml` entry for this run, regardless of its `enabled` flag. |

## Exit codes

`ccm` exits with a distinct, documented code per failure mode, so a wrapper script can
branch on the code without parsing stderr text. Checks run in a fixed order (see
`prd.md`, "Check order") — when more than one failure condition applies at once, the
earliest-listed stage below wins.

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic/unexpected error |
| 2 | Usage error — invalid or conflicting CLI arguments |
| 3 | `--gen-config` failed to create the config directory or write a file |
| 4 | Not a git or jj repository |
| 5 | Config error — `api.yaml`/`prompts.yaml` missing, unreadable, malformed, or fails cross-validation |
| 6 | Tool/API selection failed — every `api.yaml` entry is disabled, or `--tool <NAME>` matched no entry |
| 7 | Diff generation failed — the underlying `git diff`/`jj diff` (or jj's enumeration call) itself errored |
| 8 | Nothing to diff — no staged changes (git), or an empty scope after `--include`/`--exclude` (jj) |
| 9 | API key resolution failed |
| 10 | Tool/API call failed — network error, timeout, non-zero exit, or an API-level error response |
| 11 | Malformed response (`openai_api` only) |
| 12 | Editor unavailable — `$EDITOR` doesn't resolve, or is unset with no `nvim`/`vim`/`vi` on `$PATH` |
| 13 | Editor aborted — `$EDITOR` exited non-zero |
| 14 | Aborted — an interactive picker (the tool picker in default mode, or the jj commit-command picker) was cancelled (EOF on stdin) |
| 15 | Empty commit message |
| 16 | Commit failed — the `git commit`/`jj commit`\|`describe`\|`split` invocation failed |
| 17 | Temp file creation/write failed |

All progress output ("Generating diff using: ...", etc.) goes to stderr; stdout is
reserved for `--dry-run`'s message and `--gen-config`'s report.

## Development

```sh
cargo build
cargo test
cargo fmt
cargo clippy --all-targets -- -D warnings
```

The test suite drives the real binary as a subprocess for integration coverage
(`tests/*.rs`, via `assert_cmd`), alongside unit tests for the pure logic (path
normalization, diff-scope resolution, message cleanup, config validation, ...) colocated
with each module. `tests/backend_openai.rs` mocks the OpenAI-compatible endpoint with
`wiremock`; every other subprocess dependency (`git`, `jj`, `$EDITOR`, `agent_cli`) is
exercised for real against fixtures under a temp directory.
