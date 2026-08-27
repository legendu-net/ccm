# Contextual Commit Message (ccm) - Product Requirement Doc

## Goal
I have a Lua script at https://github.com/legendu-net/AstroNvim_template/blob/main/lua/plugins/commit_msg.lua,
which wraps command-line agent CLI to generate commit messages inside Neovim.
The Lua script is ugly, hard to understand and not extensible.
I'd like to reimplement the fundamental functionality of generating commit messages in Rust
and make the Lua script a thin wrapper of it.
This product design doc focuses on the rust implementation for generating commit messages.
The binary is named `ccm`, short for "Contextual Commit Message."

## Existing Tools
I've already done some research and found existing tools for this purposes.
For example,
there is https://github.com/dikkadev/turboCommit implemented in Rust.
However,
turboCommit is seriously limited.
It supports only GPT-5.4 
while I need a more flexible command-line application 
which supports both OpenAI compatible API and local agent CLI.

There is also https://github.com/tak-bro/aicommit2 which is implemented in TypeScript.
aicommit2 is very close to what I need but not exactly.
When used directly, 
aicommit2 uses `jj describe` for a jj repository
while there are many jj commands (e.g., `jj commit`, `jj describe` and `jj split`)
which require commit messages.
It's more flexible if aicommit2 asks for the jj command to commit the message.
I thought about wrapping aicommit2 into a thin Lua script,
but there are 2 issues.
First, aicommit2 is based on TypeScript and requires a nodejs runtime.
Second, aicommit2 doesn't have an option to specify a list of files 
based on which to generate the difference (even though it has an --exclude option).
This is inconvenient if i want to generate commit messages for `jj split` or `jj commit -i`.

## Requirements

Supported platforms: Linux and macOS only. Windows is out of scope — no
`cmd.exe`/PowerShell handling, no Windows-specific path or environment-variable (e.g.
`%TEMP%`/`%TMP%`) behavior, and no testing on or accommodation for it.

1. Support both Git and Jujutsu (jj) repositories.
    If a directory is both a git and a jj repository (colocated), prefer jj unless
    `--git` is passed to force git handling (see CLI Interface).

2. [High Priority] Supports generating commit messages by calling OpenAI compatible APIs.
    There is no need to support fallbacks 
    as my main use case will be calling a local OmniRoute server
    which handles LLM API routing and fallbacks.
  
3. [Low Priority] Supports generating commit messages using local Agent CLI, e.g., GEMINI CLI.
    There is no need to support fallbacks for this mode either.
    This is a legacy mode.
    Its main use case is for situations where LLM APIs aren't easily accessible 
    but there are local agent CLIs to leverage.

4. Allow the user to configure a list of tools/APIs to use via YAML (`api.yaml`, see Configuration).
    The command-line app selects the first `enabled` entry in the list — nothing more.
    There's no fallback and no probing of whether the entry will actually work: if it
    fails, let it fail. (An earlier version of this design had `ccm` check each
    entry's local "availability" — e.g. whether an `agent_cli`'s `command` resolves on
    `$PATH` — before selecting it, skipping to the next `enabled` entry otherwise. That
    mirrored the Lua script this replaces, where the same probing doubled as an
    implicit fallback chain and, incidentally, as a way to auto-switch between
    machine-specific tool profiles depending on what was installed locally. Neither
    reason applies here: fallback is explicitly out of scope per this requirement, and
    profile-switching is already handled directly by `api.yaml`/`prompts.yaml` being
    real per-machine config files. So availability checking was dropped — see
    Selection in Configuration.)
    See Error Handling for how failures are reported.

    The user can also list what's configured (`--list-tools`) and choose the tool/API a
    different way for a single run — an explicit `--tool <NAME>`, or, in default mode
    when the automatic first-enabled rule can't resolve to a single entry on its own, a
    tool-picker prompt — see CLI Interface and Selection. Neither is a fallback
    mechanism: exactly one entry is still tried per run, just chosen a different way.

5. Allow the user to customize the prompt to use for each tool/API.
    Since most tools/APIs might use the same prompt,
    the best way is to allow the user to configure a set of named prompts (`prompts.yaml`, see Configuration)
    that can be referenced from the tool/API configuration.
    The commit message format/style (e.g. Conventional Commits, line-length limits) is not
    enforced by `ccm` — it's entirely up to what the user writes into a prompt's `system`/`template`.

6. Support `--include` and `--exclude` flags, supported only for jj repositories, to
    control which files the diff is computed from (see CLI Interface for exact
    semantics). They restrict the working-copy diff set directly. Default to the
    entire current working copy when neither is given. A colocated repository run
    with `--git` is treated as a git repository for this purpose, so passing
    `--include`/`--exclude` there is a usage error (see CLI Interface).

7. Default to reviewing: after generation, prompt the user to regenerate, edit, or
    accept the generated message (see "Message review prompt" in CLI Interface) —
    defaulting to accept (or edit, when generation came back blank, since there is
    nothing worth accepting), giving the user a chance to write their own even when
    generation came back blank. Accept commits the message as-is; edit opens it in
    `$EDITOR` first, same as before; regenerate re-runs generation against the same
    selected entry and the same diff, then prompts again. After a non-blank message is
    settled on, commit it automatically.
    Support `--dry-run` to print the message to stdout instead,
    without showing the review prompt, opening the editor, or committing. Under
    `--dry-run` a blank generated message has no such chance to be fixed up, and is a
    hard failure (see Error Handling, exit code 15).
    - use `git commit` to commit the message for a git repository.
    - prompt the user to select the jj command to use to commit the message for a jj
      repository, and pass the same `--include`/`--exclude`-resolved files into that
      command (see "jj commit commands" in CLI Interface) so the commit matches what the
      message describes.

8. Define a stable, documented set of process exit codes covering each distinct failure mode
    (see Error Handling), so the thin Lua wrapper can branch on the failure
    and show an appropriate message instead of a generic error.

9. Log the same kind of step-by-step progress the existing Lua script reports (e.g.
    "generating diff…", "generating commit message using \<tool\> (\<model\>)…",
    "commit message generated by \<tool\> (\<model\>)" — see "Progress logging" in CLI
    Interface for the full list). All of it goes to stderr, never stdout — stdout is
    reserved for the generated message under `--dry-run`, and progress lines must not
    end up mixed into it (e.g. if stdout is captured/piped).

## Non-Goals

- No special handling for diffs that exceed a tool/API's context window (no truncation,
    chunking, or summarization). If a diff is too large, the call simply fails with the
    normal tool/API failure exit code.
- Not a non-goal, to be clear: a hung network call or a hung agent CLI is bounded by the
    per-entry `timeout` field (see `timeout` under Common fields in Configuration), so it
    doesn't join the list above as an unhandled case.

## CLI Interface

Binary name: `ccm`.

### Repository detection

`ccm` determines the repository type by walking the directory tree itself, rather than
shelling out to `git`/`jj` for detection — deliberately: requiring the *other* tool's
binary to be installed just to determine which one applies would be backwards, and jj in
particular is far less universally installed than git.

`ccm` canonicalizes the current working directory once (resolving symlinks), then walks
upward through it and its ancestors toward the filesystem root, looking for a `.git` entry
and a `.jj` entry directly inside each directory visited:

- A `.git` entry may be a directory (a normal repository) or a file containing a `gitdir:
    <path>` pointer (a linked worktree or a submodule) — `ccm` only checks that *something*
    named `.git` exists there, not that it's specifically a directory, so worktrees and
    submodules are detected the same as an ordinary repository.
- A `.jj` entry is checked for existence the same generic way, for the same robustness,
    even though in current jj versions it's always a directory.
- The walk tracks the git marker and the jj marker independently: the first (nearest to the
    starting directory) ancestor containing a `.git` entry is the git root, and separately
    the first ancestor containing a `.jj` entry is the jj root — a single directory can
    supply both, one, or neither. The walk stops once both have been found, or once it
    reaches the filesystem root having found only one or neither.
- Because the starting directory was already canonicalized and each step up is a plain
    parent-directory move on an already-resolved path, every root found this way is already
    fully resolved — no separate canonicalization pass is needed before comparing them
    (contrast with the two-independent-subprocess approach this replaces, which had to
    canonicalize each tool's reported root separately before comparing).
- A bare repository (no working tree — the repository root itself holds `HEAD`, `objects/`,
    etc. directly, with no `.git` subdirectory) is therefore never detected as a git
    repository by this walk. That matches `git rev-parse --show-toplevel`, which also fails
    inside a bare repository, so this isn't a behavior change from the subprocess-based
    approach it replaces.

This intentionally does not replicate git's own ownership check (`safe.directory` /
"detected dubious ownership") — a directory owned by another user is still detected as a
git repository at this stage. That's fine: this walk only decides *which* codepath handles
the directory; the actual `git`/`jj` subprocess invocations later (diff generation, commit)
still shell out to the real binaries, which enforce that check themselves and fail normally
if it isn't satisfied.

When both a git root and a jj root are found, `ccm` compares them:

- If they're equal, the directory is genuinely colocated — jj handling by default, git
    handling if `--git` is passed (Requirement 1).
- If they differ, one is necessarily an ancestor of the other, since both were found by
    walking up from the same starting directory — the two can't be unrelated. `ccm` treats
    the *deeper* (more nested) root as the actual repository for this directory, and the
    shallower one as an unrelated outer repository that just happens to contain it:
    - git root is deeper — e.g. an independent git checkout nested inside a larger
        jj-managed tree — treated as a plain git repository: git handling; `--git` is a
        harmless no-op.
    - jj root is deeper — e.g. a jj project nested inside a git-tracked dotfiles
        directory — treated as a plain jj repository: jj handling; `--git` is a usage
        error (exit code 2).

| Git root found | Jj root found | Roots | Result |
|---|---|---|---|
| yes | yes | equal | Colocated — jj handling by default, git handling if `--git` is passed (Requirement 1) |
| yes | yes | differ, git root deeper | git repository (nested under an unrelated outer jj repo) — git handling; `--git` is a harmless no-op |
| yes | yes | differ, jj root deeper | jj repository (nested under an unrelated outer git repo) — jj handling; `--git` is a usage error (exit code 2) |
| no | yes | — | jj repository — jj handling; `--git` is a usage error (exit code 2) |
| yes | no | — | git repository — git handling; `--git` is a harmless no-op |
| no | no | — | Not a git or jj repository — exit code 4 |

This means repo-type detection never requires the git binary or the jj binary to be
installed — only whichever one is actually needed afterward (diff generation and commit)
has to be present, and that's enforced naturally when `ccm` tries to run it, not during
detection.

Flags:

- `--include <FILE|DIR>...` — Controls which files the diff is computed from, given as
    file or directory paths relative to the current working directory — a directory
    entry matches every changed file nested under it, recursively (including
    sub-directories), the same convention `jj split <dir>` already uses; see "Diff scope
    resolution" below for the exact matching rule. Repeatable and/or space-separated.
    Mutually exclusive with `--exclude`. Supported only in a jj repository — see "Diff
    scope resolution" below: it restricts the diff to just these files. Usage error
    (exit code 2) if the repository is being handled as git (a plain git repository, or
    a colocated repository with `--git` passed).
- `--exclude <FILE|DIR>...` — Same file/directory-selection shape as `--include`,
    mutually exclusive with it. Supported only in a jj repository: removes these
    files/directories from the working-copy diff set. Same git usage error as
    `--include`.
- `--git` — Force git handling in a colocated (git + jj) repository, overriding the
    default preference for jj (see Requirement 1). Harmless no-op if the directory is a
    plain git repository (not colocated). Usage error (exit code 2) if the current
    directory isn't *treated as* a git repository at all — see the repo-detection table
    below: this includes the case where a git root does technically exist somewhere in
    the ancestry but a deeper jj root wins, so the directory is still resolved as a jj
    repository.
- `--dry-run` / `-d` — print the generated commit message to stdout only;
    do not open `$EDITOR` and do not commit. Unlike default mode, this does not require
    an interactive terminal (see "Interactive terminal requirement" below).
- `--config <DIR>` / `-c <DIR>` — Use `<DIR>` as the config directory instead of the default —
    both for reading `prompts.yaml`/`api.yaml` in the normal generation flow, and as the
    directory `--gen-config` creates/populates (see below). Given as any path, absolute
    or relative to the current working directory. This is the one exception to
    `--gen-config` being "mutually exclusive with every other flag" below: `--config`
    combines with it to target `--gen-config` at a directory other than the default,
    which is what makes it practical to generate a throwaway config (e.g. for a test
    fixture) without touching the default config directory. The default (used whenever
    `--config` isn't passed) is `$XDG_CONFIG_HOME/ccm/` if `$XDG_CONFIG_HOME` is set to a
    non-empty value, otherwise `~/.config/ccm/`. Every other reference to "the config
    directory" or "the default config directory" elsewhere in this document means this
    resolved default unless `--config` was passed, in which case it means the directory
    `--config` names.
- `--gen-config` / `-g` — create the config directory (the default described under `--config`
    above, or the directory given via `--config`) if it doesn't already exist, write the filled
    example `prompts.yaml` and `api.yaml` shown in Configuration for any of the two that
    don't already exist, then exit with code 0. Never overwrites an existing file. For
    each of the two files, `ccm` prints to stdout which outcome applied — created, or
    already existed and was left untouched — so a partial run (e.g. `prompts.yaml`
    created because it was missing, `api.yaml` left alone because it was already there)
    reports both outcomes explicitly rather than only mentioning the one that was
    created. Does not require being run inside a git or jj repository. Mutually
    exclusive with every other flag except `--config` (see above). This flow is separate
    from "Progress logging" (whose stderr-only rule applies to the default/`--dry-run`
    generation flow, not `--gen-config`): `--gen-config` writes its output to stdout,
    reserving stderr for error messages (e.g. a filesystem failure below). If both
    `prompts.yaml` and `api.yaml` already exist, nothing is created; `ccm` prints the
    "already existed" line for both and still exits 0 — this is not an error. Any
    filesystem failure along the way (permission denied creating the directory or
    writing a file, disk full, the config directory already existing as a
    non-directory, etc.) is reported to stderr and exits with code 3 rather than
    partially succeeding silently.
- `--list-tools` / `-l` — print every `api.yaml` entry (from the config directory resolved the
    same way as above), one per line, in file order — name, `type`, an
    `[enabled]`/`[disabled]` marker, and a `(default)` marker on the first `enabled: true`
    entry (the one `select_first_enabled` would pick, absent `--tool`) — then
    exit with code 0. Like `--gen-config`, this
    short-circuits every later stage and does not require being run inside a git or jj
    repository; unlike `--gen-config`, it still goes through Config load & validation
    (exit code 5 on a malformed `prompts.yaml`/`api.yaml`), since there's nothing to list
    otherwise. Writes to stdout, same as `--gen-config`. Mutually exclusive with every
    other flag except `--config`.
- `--tool <NAME>` / `-t <NAME>` — use the `api.yaml` entry named `NAME` for this run, in place of the
    normal "first `enabled: true` entry" rule and, in default mode, in place of the tool
    picker (see Selection) — including selecting an entry whose `enabled` is `false`.
    Matches `name:` exactly, byte-for-byte and case-sensitive, the same comparison
    `api.yaml`'s own name-uniqueness check uses. If no entry has that name, that's a
    selection failure (exit code 6), same as "every entry disabled." A `NAME` of `''`
    (an empty string) is a sentinel instead of a name to match: it forces the tool
    picker for this run, regardless of `--dry-run` or how many entries are enabled — see
    Selection and "Interactive terminal requirement."

Passing both `--include` and `--exclude` in the same invocation is a usage error (exit code 2).
Passing `--include` or `--exclude` while the repository is being handled as git (a plain
git repository, or a colocated repository with `--git` passed) is also a usage error
(exit code 2).
Passing `--gen-config` together with any flag other than `--config` is also a usage
error (exit code 2).
Passing `--git` when the current directory isn't a git repository at all is also a usage
error (exit code 2).
Passing `--list-tools` together with any flag other than `--config` is also a usage error
(exit code 2).

### Interactive terminal requirement

Default mode (no `--dry-run`) requires a real interactive terminal: the message review
prompt (see "Message review prompt"), the jj-command picker (a stdin prompt, see "jj
commit commands"), and `$EDITOR` itself all need genuine interactive stdio to do their
jobs — a picker reading a line (or, for the review prompt, a single keypress on a real
terminal) from stdin, an editor attaching to a controlling terminal to actually let the
user edit. `ccm` runs no explicit upfront check for this; instead, the requirement
surfaces naturally through existing failure modes when it isn't met — e.g. stdin
already at EOF (piped from `/dev/null`, or a script with nothing left to send) trips the
review prompt's or the jj-command picker's EOF-cancels-the-prompt behavior (exit code
14, see "Message review prompt" and "jj commit commands"), and an editor that can't
attach to a controlling terminal typically exits non-zero, which `ccm` treats as an
aborted edit (exit code 13, see "Default behavior").

Default mode can add a fourth stdin prompt — the tool picker (see "Selection") — at
stage 5, before diff generation: whenever the automatic "first `enabled: true` entry"
rule can't resolve to a single entry on its own (zero enabled, or 2+ enabled) and no
non-empty `--tool <NAME>` was given. This prompt has two front-ends (see "Selection" and
"Preferences of Dependencies" item 8) — `fzf` when it's on `$PATH`, stdin is a real
usable terminal, and `CCM_FUZZY` isn't `0`; a numbered stdin prompt otherwise — but the
interactive-terminal requirement and the EOF/abort-cancels-with-exit-14 behavior are
identical either way.

`--dry-run` otherwise has no interactive-terminal requirement at all: absent `--tool`, it
never shows the message review prompt, never opens `$EDITOR`, never reads the
jj-command picker's stdin prompt, and never shows the tool picker either — it always
takes the first `enabled: true` entry regardless of how many entries are enabled (exit
code 6 if none are). So it runs correctly with stdin/stdout piped, redirected, or absent
entirely (see "Default behavior", "jj commit commands", and "Selection"). This split is
deliberate, matching the two ways `ccm` is meant to be used (see Goal): default mode is
for direct interactive use of `ccm` from a shell, so it's free to ask when the tool
choice is ambiguous or the message needs a second look, while `--dry-run` is what a
non-interactive caller — chiefly the thin Neovim Lua wrapper described in Goal, which
only needs the generated message text on stdout to build its own UI around — is expected
to use instead, so it never prompts for anything on its own; a caller with 2+ enabled
entries that wants to run `--dry-run` unattended selects one explicitly with `--tool
<NAME>`.

The one exception: `--tool ''` (see CLI Interface) forces the tool picker regardless of
`--dry-run`, since typing it is a deliberate, one-off request — not something an
unattended wrapper does by accident — so `--dry-run --tool ''` still needs interactive
stdin for that prompt (whichever front-end applies — cancelling it, by EOF or an explicit
abort, is exit 14 either way), even though `--dry-run` otherwise skips every prompt in
the run. Whether `fzf` happens to be installed changes nothing about this: absent
`--tool` entirely, `--dry-run` never reaches the tool picker at all, so there's nothing
for `fzf` to be tried against on that path regardless.

### Diff generation

`ccm` captures the diff by running the underlying tool as a subprocess with piped (not
inherited) stdout, so the diff is never written to an actual terminal — this alone means
git's and jj's color and pager logic (both default to `"auto"`, i.e. only active when
stdout is a TTY) stay off. `ccm` additionally passes explicit flags as defense-in-depth
against a user's global config forcing color/pager on regardless of TTY (e.g.
`color.diff = always`):

- git: `git --no-pager diff --no-color --staged` (no file arguments — `--include`/
    `--exclude` aren't supported for git, see above). This means the user must `git add`
    whatever they want a message generated for before running `ccm` — unstaged changes
    are invisible to it, the same precondition `git commit` itself has; if nothing is
    staged, this surfaces at runtime as exit code 8 (see Error Handling).
- jj: `jj --no-pager diff --color=never` plus the resolved file arguments, if any (see
    "Diff scope resolution" below).

This is what actually gets run; the `Generating diff using: <command>` progress line
(see Progress logging) reports this exact invocation, including these flags.

Diff scope resolution (jj only — `--include`/`--exclude` are a usage error under git handling):

| `--include` | `--exclude` | Diff scope |
|---|---|---|
| absent | absent | all files in the current working copy |
| present | absent | only the included files |
| absent | present | all working-copy files minus the excluded ones |
| present | present | usage error — mutually exclusive |

`jj diff` has no flag that means "these files minus these excluded ones", but jj's
fileset language does support set difference directly (the `~` operator, e.g.
`jj diff -- 'all() ~ file:"a.rs"'`; see `jj help -k filesets`). `ccm` doesn't use that
route: a `file:` pattern that matches nothing (e.g. a typo'd path) is silently accepted
by the fileset language rather than rejected, and `ccm` needs to surface a typo'd
`--include`/`--exclude` path as a usage error (exit code 2, see below) instead of
silently diffing an empty or unintended scope. So to resolve `--exclude` (and to
validate both `--include` and `--exclude` paths per the rule below), `ccm` first
enumerates the full set of changed
paths in the jj working copy, one entry per `jj diff --summary` line rather than a
deduplicated set of bare strings (see the rename/copy handling below for why that
distinction matters) — e.g. via `jj --no-pager diff --color=never --summary`, using the
same color/pager-suppressing flags as "Diff generation" above since this output is
parsed programmatically too. This enumeration call is a real subprocess invocation in
its own right, distinct from the final `jj diff <files...>` call below, and can fail the
same way a diff invocation can — e.g. `jj` not found, or erroring inside a jj repository
for some other reason — which is reported the same way: exit code 7 (see Error
Handling), and via its own progress lines, `Enumerating working-copy files using:
<command>` before it runs and `Working-copy files enumerated by: <command>` once it
succeeds (see Progress logging), separate from the `Generating diff using: <command>`
line for the final synthesized command below. `jj diff --summary` reports paths relative to the current
working directory, the same basis `--include`/`--exclude` are given in (see Flags above).

Known limitation, left unaddressed: the enumeration call and the final `jj diff
<files...>` invocation below are two separate subprocess calls, so the working copy can
change between them — e.g. a file included because it appeared in the enumeration could
be reverted, or a new file could appear, before the diff itself actually runs. `ccm` does
not re-enumerate or otherwise guard against this. This is the same class of
time-of-check-to-time-of-use exposure already accepted for the (larger) gap between
message generation/editing and the final commit — see the "Known limitation, left
unaddressed" note under "Default behavior" — not a new risk specific to
`--include`/`--exclude`.

Before comparison, `ccm` lexically normalizes both the enumerated paths and the
`--include`/`--exclude` paths the same way: each is split into `Path::components()`,
any `CurDir` (`.`) component is dropped (a trailing separator, e.g. `src/`, produces no
trailing empty component either, so it normalizes the same as `src`), and the
remaining components are rejoined with a single `/` — so e.g. `./src/main.rs` and
`src//main.rs` both normalize to `src/main.rs`, and a harmless difference in how the
user spelled a path doesn't fail to match jj's own (already-clean) `--summary` output
and wrongly trip the unmatched-path usage error below. This is purely lexical — no
filesystem access, no symlink resolution, and `..` components are left as-is rather
than collapsed — so it normalizes spelling, not identity: a path that only resolves to
the same file after resolving a symlink or a `..` segment (e.g.
`--include ../repo/src/main.rs`) is still compared literally and can still trip the
unmatched-path check.

An `--include`/`--exclude` entry may name a directory as well as a single file — the
same convention `jj split <dir>` already supports — in which case it matches every
enumerated path nested under it, recursively (sub-directories included), not just
direct children. Matching is component-wise, not a raw string prefix: normalized entry
`E` matches normalized enumerated path `P` if `P` equals `E` exactly, or if `P`'s
components begin with all of `E`'s components followed by at least one more — so
`--include src` matches `src/main.rs` and `src/sub/mod.rs` but not `src-old/main.rs`
(plain string prefixing would wrongly match that last one). `ccm` never checks the
filesystem to decide whether an entry "is a directory" — there would be nothing to
check if the directory itself, and everything under it, was deleted — so this
component-wise rule against the enumerated paths is the entire mechanism; a bare file
entry and a directory entry are resolved by the exact same rule, not two separate code
paths. Each entry must match at least one enumerated path (exactly, or as a directory
prefix) to be valid — one that matches zero paths (a typo'd file name, or a directory
with no changed files under it) is the unmatched-path usage error described below.
`--include` restricts the enumerated set to the union of paths matched by any entry;
`--exclude` removes the union of paths matched by any entry from the enumerated set.
The resulting
path list is then passed as explicit file arguments to the actual
`jj --no-pager diff --color=never <files...>` invocation. This synthesized file list —
not a literal `--exclude` flag, which doesn't exist — is what appears in the `Generating
diff using: <command>` progress line (see Progress logging) whenever `--include` or
`--exclude` was given.

Any `--include`/`--exclude` entry that doesn't match at least one file in the jj
working copy — whether given as an exact file path or as a directory prefix — (e.g. a
typo'd file name, or a directory containing no changed files) is a usage error (exit
code 2), not silently ignored.

A `jj diff --summary` line for a rename or copy does not show `<old>` and `<new>` as two
independent full paths. jj factors out the longest common leading path they share (split
on `/`, cwd-relative, possibly empty) and shows only the differing suffixes in braces:

```
R <prefix>{<old-suffix> => <new-suffix>}
C <prefix>{<old-suffix> => <new-suffix>}
```

e.g. `R src/{old.rs => new.rs}` (shared `src/` factored out), or `R {a.txt => sub/b.txt}`
when there's no shared prefix at all — `<prefix>` is then empty and the line starts
directly with `{`. `ccm` recovers `<old>` and `<new>` from a line like this by taking
everything between the status character's trailing space and the line's first `{` as
`<prefix>`; everything between that `{` and the line's final `}` (always the line's last
character) split once on the literal substring `" => "` as `<old-suffix>`/`<new-suffix>`;
then concatenating: `<old> = <prefix> + <old-suffix>`, `<new> = <prefix> + <new-suffix>`.
This is unambiguous for any path that doesn't itself contain a literal `{`, `}`, or the
exact substring `" => "` — the same class of edge-case risk the `#CCM: ` comment-prefix
stripping elsewhere in this doc already accepts, not a new kind of limitation. Lines for
a plain modification/addition/deletion (`M`/`A`/`D`) never use this braced form — the
whole remainder of the line after the status character is the one path, as already
described above.

Once `<old>` and `<new>` are recovered this way, they are matched by `--include`/`--exclude`
against either name — using the same exact-match-or-directory-prefix rule described
above, so a directory entry covering `<old>` or `<new>` matches the line just as a bare
file entry would — naming either path includes/excludes the line as a whole, contributing
`<new>` (never `<old>`) to the resulting file-argument list: for a rename, `<old>` no
longer exists in the working copy to diff; for a copy, `<old>` is unchanged by
definition (an unchanged copy source has nothing of its own to diff), so `<new>` is the
only path with content to show either way. The resulting file-argument list is
deduplicated before being passed to `jj diff <files...>`: naming both `<old>` and
`<new>` of the same rename/copy in one `--include` still matches the one line just
once, contributing `<new>` a single time, not twice. Matching is done per summary line, not by
deduplicating path strings across lines: a copy's source can also carry independent
changes of its own, reported as its own separate summary line for `<old>`, and that line
is included/excluded on its own terms — contributing `<old>` itself to the
file-argument list — regardless of the `C` line's outcome, so an independently-modified
copy source is never silently dropped. This also means naming `<old>` in
`--include`/`--exclude` affects both lines that mention it, the copy's `<new>` mapping
and its own independent entry alike; to affect only the copy mapping, name `<new>`
instead.

For a git repository (including a colocated repository run with `--git`), `ccm`
always diffs/commits the current stage as-is; `--include`/`--exclude` are not
accepted (see the usage-error note above).

Default behavior (no `--dry-run`): compute the diff, generate the commit message,
open it in `$EDITOR` — always, even if generation came back blank — and after the user
saves and exits, commit it automatically — `git commit` for git (see "git commit"
below for the exact template), or a prompt to pick the jj command (`jj commit`, `jj
describe`, `jj split`) for jj, invoked per "jj commit commands" below. The `nvim`/`vim`/`vi` fallback only applies when
`$EDITOR` is unset: `ccm` then falls back in order to `nvim`, then `vim`, then `vi`
(whichever is found first on `$PATH`). If `$EDITOR` is set but doesn't resolve to an
executable (typo, moved binary, etc.), that's an error — exit code 12 — not a trigger
for the fallback chain. If `$EDITOR` is unset and none of `nvim`/`vim`/`vi` are found
on `$PATH` either, that's the same error, exit code 12.

The temp file is created directly in the OS temp directory (i.e. whatever
`std::env::temp_dir()` resolves to — `$TMPDIR` if set, `/tmp` otherwise), never inside
the git/jj repository, with a name starting
with the prefix `CCM_EDITMSG_` followed by a randomized suffix for uniqueness (see
"Preferences of Dependencies" for how this is generated) — not a timestamp: the file's
own filesystem metadata (creation/modification time, e.g. via `stat` or `ls -t`) already
carries that, so it isn't duplicated into the name. Once created — immediately before
invoking `$EDITOR` — `ccm` never deletes it, regardless of how the run ends: a
successful commit (exit 0), an aborted editor (exit 13), a cancelled jj-command picker
(exit 14), a blank cleaned message (exit 15), or any other later failure. This is
deliberate: the file is left behind on every path so the user always has a copy of what
was generated and/or edited to recover from. A consequence of this — accepted, not
addressed — is that `CCM_EDITMSG_*` files accumulate indefinitely in the OS temp
directory across runs; `ccm` never prunes old ones, so cleanup (if ever wanted) is left
to the OS's own temp-directory conventions (e.g. `systemd-tmpfiles`, a reboot clearing
`/tmp`) or to the user. An exit code that occurs before the temp
file is ever created (e.g. exit code 12, `$EDITOR` failing to resolve at all) naturally
has nothing to leave behind.

If creating the temp file fails (e.g. `tempfile::Builder::new().prefix("CCM_EDITMSG_")
.tempfile_in(std::env::temp_dir())` erroring because the OS temp directory is
unwritable or the disk is full — see "Preferences of Dependencies"), or if writing the
pre-populated content (see "Message pre-population and cleanup" below) into it fails
partway through, that's a distinct failure from exit code 12: `$EDITOR`/fallback
resolution may have already succeeded by this point, and the failure is in creating or
populating the file itself, not in resolving an editor to open it with. This is exit
code 17.

This only applies to the default editing flow —
`--dry-run` never creates a temp file at all (see "Message pre-population and
cleanup").

`$EDITOR` commonly holds more than a bare program name (e.g. `EDITOR="code --wait"`,
`EDITOR="emacsclient -t"`), so `ccm` splits its value into words using the `shell-words`
crate's `shell_words::split()` (see "Preferences of Dependencies") — unquoted whitespace
separates words; a single- or double-quoted substring is one word with the quotes
stripped, so a quoted path containing spaces isn't split apart; a backslash outside a
single-quoted substring escapes the character that follows it (so e.g. `\ ` embeds a
literal space in an otherwise-unquoted word) per that crate's rules. This is the same
class of splitting `git` applies to `GIT_EDITOR`/`core.editor`. The first word is the
program; any remaining words are leading arguments
passed to it before the temp file path, which `ccm` always appends last:
`<program> <leading-args...> <tempfile>`. The "resolves to an executable" check for exit
code 12 applies to this first word (on `$PATH`, or as an absolute/relative path), not to
the raw `$EDITOR` string. If `$EDITOR` is set but splits to zero words (empty or
all-whitespace), `ccm` treats that the same as `$EDITOR` being unset — the fallback chain
applies — since there's no program name to resolve at all. The `nvim`/`vim`/`vi`
fallback values are each invoked as a bare program with no leading arguments, just the
appended temp file path.

If the `$EDITOR` process itself exits with a non-zero status (crashed, force-killed,
etc.), `ccm` treats that as an abort, git-style: it does not read the temp file's
content, does not run the blank-message check, and does not commit, regardless of what
was or wasn't written to the file before the editor died. This is exit code 13, distinct
from exit code 15 (a zero-exit editor leaving a blank file) — a non-zero exit means the
editor itself is reporting failure, not that the user chose to submit an empty message.

If `$EDITOR` instead exits zero but the temp file is then missing or unreadable (deleted
or otherwise interfered with between the editor closing and `ccm` reading it back — not
a scenario `$EDITOR`'s own exit status can report), that's outside both exit code 13 (no
non-zero exit occurred) and exit code 15 (there's no content to judge blank or not): it
falls to exit code 1, the generic/unexpected-error catch-all.

Known limitation, left unaddressed: the file scope `--include`/`--exclude` resolve to
(and, for git, the staged content itself) is fixed before `$EDITOR` opens, but the
actual `git commit`/`jj commit`\|`describe`\|`split` invocation happens only after the
user closes the editor — a gap that can be arbitrarily long. If the user (or another
process) changes staging or the working copy during that window, the eventual commit
can diverge from what the generated/edited message describes. `ccm` does not re-check
or re-resolve anything at commit time to guard against this — the same
time-of-check-to-time-of-use exposure plain `git commit -e`/`jj describe` already have,
not a new risk `ccm` introduces.

### Response cleanup

Immediately after stage 7 (Generation) gets a response back from the selected tool/API,
and before that response is used for anything else — printed under `--dry-run`, or shown
at the message review prompt and, if edit is chosen there, pre-populated into the
`$EDITOR` temp file (see "Message review prompt" and "Message pre-population and
cleanup" below) — `ccm` runs it through one cleanup pass: it trims leading/trailing
whitespace unconditionally, then strips a single Markdown code fence wrapping the whole
response, then a single pair of matching quotation marks wrapping the whole response or
just its first line. Some LLMs answer with the message inside a ```…``` block, or as a
quoted string (e.g. `"feat: add x"`), regardless of what the prompt asks for; a chatty
LLM or an `echo`-based `agent_cli` (see Requirement 3) also routinely leaves a trailing
newline or other stray whitespace even without either wrapper. Centralizing this cleanup
here means every caller — the default `$EDITOR` flow, `--dry-run`, and any other
consumer of `ccm`'s output — gets it for free instead of having to reimplement the same
cleanup itself (this used to live in the Neovim wrapper described in Goal, ad hoc,
before the fundamental generation logic moved into `ccm`).

- Code fence: the response's first line must be a bare opening fence — a run of at least
    3 backticks, optionally followed by a language tag, and nothing else (a line with
    text after the backticks, e.g. "```feat: add x", is the message itself, not a
    wrapper) — and the first following line that's a bare closing fence — a run of at
    least as many backticks, and nothing else — must be the response's very last line. If
    a closing fence appears earlier, the response holds other content or additional code
    blocks rather than one wrapping fence, and is left unchanged; same if there's no
    closing fence at all, or the first line isn't a bare opening fence.
- Quotation marks: straight quotes (`"`, `'`, `` ` ``) and typographic "smart" quotes
    (`“ ”`, `‘ ’`) are recognized. Only a genuinely enclosing pair is stripped — one whose
    delimiters do not reappear inside — so an apostrophe within the text (e.g. `fix:
    don't crash`) or two separate quoted spans (e.g. `"foo" and "bar"`) are left alone.
    The whole response is tried first; if that doesn't unwrap, just its first line is
    tried, so a quoted subject with an unquoted body is still cleaned up.

This pass runs unconditionally, in both `--dry-run` and the default `$EDITOR` flow alike
— a fenced or quoted response is equally unwanted either way, and it runs before the
exit-code-15 blank check either mode applies (a response that's exactly an empty fenced
block, e.g. "``` ```", cleans down to blank and is correctly treated as one, and a
response that's entirely whitespace trims down to blank the same way). The
leading/trailing-whitespace trim is unconditional — the result is always at least
`raw.trim()`, whether or not a fence or quote-wrap also applied — so `--dry-run`'s
stdout, the message review prompt's displayed text, and `$EDITOR`'s pre-population never
carry incidental surrounding whitespace the tool/API happened to answer with.

This step is orthogonal to the `#CCM: `-comment cleanup described next: that one strips
scaffolding `ccm` itself writes into the `$EDITOR` temp file, while this one cleans up
the tool/API's own response before it ever reaches either destination.

### Message review prompt

In default mode, once stage 7 produces a cleaned message (see "Response cleanup"
above), `ccm` shows a review prompt — on stderr, like every other prompt — before doing
anything else with it: regenerate, edit, or accept. This replaces the old fixed
behavior of always opening `$EDITOR`, so a good-enough message can be committed with a
single keypress, and a poor one can be thrown away and re-requested without leaving
`ccm` at all.

```
[R]egenerate  [E]dit  [Space/Enter] accept:
```

or, when generation came back blank (nothing worth accepting yet):

```
[R]egenerate  [Space/Enter] edit:
```

- **Regenerate** (`r`/`R`) re-runs stage 7 (message generation) against the same
    already-selected `api.yaml` entry and the same already-computed diff — no re-running
    diff generation, and no re-showing the tool picker — then shows the review prompt
    again with the new result. There is no limit on how many times this can happen in one
    run.
- **Edit** (`e`/`E`) opens `$EDITOR` on the current message exactly as `ccm` always did
    before this prompt existed (see "Message pre-population and cleanup" below).
- **Accept** (a space, or Enter/blank line) commits the current message as-is, skipping
    `$EDITOR` entirely. It runs through the same `#CCM: `-comment cleanup and blank check
    `$EDITOR`'s own save would (see "Message pre-population and cleanup" below), so an
    accepted message commits byte-identically to an edit that changed nothing, and the
    pathological case of a message that's nothing but `#CCM: ` lines still hits exit 15
    rather than committing garbage.
- Accept is only offered when the current message is non-blank: a blank generation has
    nothing worth accepting (accepting it would just be exit 15 with extra steps), so the
    prompt instead offers only regenerate/edit, with a space or Enter defaulting to edit
    instead of accept — preserving the "always give the user a chance to write their own"
    guarantee (Requirement 7) a blank generation has always had.

On a real terminal, the prompt reads a single keypress — no Enter needed for `r`/`e`;
Space or Enter accepts (or edits, per the blank-message rule above) immediately. When
stdin isn't a real terminal (piped, redirected, or any non-interactive caller), the
prompt instead reads a whole line and inspects only its first character, the same
line-oriented style as every other picker in this document — so a scripted caller
driving `ccm` still works by writing `"r\n"`/`"e\n"`/`"\n"` one line at a time. Any other
input reprints the prompt and re-reads, the same reprint-and-retry behavior every other
picker in this document uses. EOF (stdin closed before a valid selection) cancels the
whole run — exit code 14, the same "an interactive picker was cancelled" code the tool
picker and the jj commit-command picker use (see "Selection", "jj commit commands").

This prompt never appears under `--dry-run` (see "Interactive terminal requirement"
above): `--dry-run` prints stage 7's cleaned response to stdout immediately and never
commits, so there is nothing here to regenerate, edit, or accept.

### Message pre-population and cleanup

This section describes what the message review prompt's edit action reaches (see
"Message review prompt" above); its accept action skips `$EDITOR` and the temp file
entirely, but still runs the message through the same cleanup step described below
before committing, so an accepted message and an edit that changed nothing produce an
identical commit.

The temp file `ccm` opens in `$EDITOR` is pre-populated with the generated message (if
any), followed by a blank line and a comment block whose lines are each prefixed with
`#CCM: ` (not a bare `#`, git-style), stating whether a message was generated or
generation came back blank — e.g.:

```
<generated message>

#CCM: message generated above. Edit as needed, save, and exit to commit.
```

or, when generation came back blank:

```

#CCM: no message was generated (the tool/API returned a blank response).
#CCM: Write your own commit message above, or leave this blank to abort.
```

Before use, `ccm` applies a cleanup step to the file `$EDITOR` saves in default mode (or,
for the review prompt's accept action, to the message being accepted, with no file
involved): lines starting with the literal `#CCM: ` prefix are removed, then leading and
trailing blank lines are trimmed from what remains — a bare `#` is not a comment marker,
so a legitimately generated line like `#123 fixes the thing` or a markdown heading is
left alone, and a blank line in the middle of an otherwise non-blank message is left
alone too (only leading/trailing blank lines are trimmed, not interior ones). If the
cleaned result is blank, `ccm` exits with code 15 instead of committing; for edit, this
is only known after the editor closes, since the user may have written their own message
over a blank generation — accept can only be chosen over a message the review prompt
already knows is non-blank (see "Message review prompt" above), but the same check still
runs, since a message that's entirely `#CCM: `-prefixed lines would otherwise slip
through. Only once the cleaned result is confirmed non-blank does `ccm` proceed —
straight to `git commit` for a git repository, or to the jj commit-command picker for a
jj repository (see "jj commit commands" below) — so the picker is never shown for a
message that's about to be discarded as blank.
This match is unconditional and unescaped: a generated or user-written line that happens
to start with the literal `#CCM: ` prefix is stripped like any other, with no way to
express a literal one — an accepted limitation, the same class of risk `git` itself
accepts with plain `#` comment lines, not an oversight.

This `#CCM: `-comment cleanup step does not apply under `--dry-run`: there's no such
comment block to strip in the first place (that scaffolding only ever gets written into
the `$EDITOR` temp file), so `ccm` prints stage 7's already-cleaned response (see
"Response cleanup" above) to stdout exactly as `generation::generate` returned it, with
no further stripping or trimming of any kind. The exit-code-15 blank check still applies
under `--dry-run` — a response that is empty or all whitespace, after the response-cleanup
pass, is blank — but that check only decides whether to print at all; it never alters what
gets printed. Since there's no editor to give the user a chance to fix up a blank
response, that check fails immediately with the same code.

### Progress logging

`ccm` reports the same kind of step-by-step progress the existing Lua script does,
adapted to `ccm`'s no-fallback selection (Requirement 4: the first enabled tool/API
is used, and if it fails, it fails — there's no "try the next one" step to report).
Every line below is written to stderr, never stdout, regardless of `--dry-run` — stdout
is reserved solely for the generated message under `--dry-run`, so progress output
never ends up mixed into it even when stdout is piped/captured. Roughly, in order:

- `Enumerating working-copy files using: <command>` — jj only, before running the `jj
    diff --summary` enumeration call, only when `--include` or `--exclude` was given (see
    "Diff scope resolution").
- `Working-copy files enumerated by: <command>` — once it succeeds.
- `Generating diff using: <command>` — before running the `git diff`/`jj diff` invocation.
- `Diff generated by: <command>` — once it succeeds.
- `Generating commit message using <name> (<model>)…` — before calling the selected
    `api.yaml` entry.
- `Generated empty message using <name> (<model>)` — if the tool/API's response, after
    the response-cleanup pass (see "Response cleanup" below), is blank (see exit code 15).
- `Commit message generated by <name> (<model>)` — once a non-blank cleaned message comes
    back.
- `Committing using: <command>` — default (non-`--dry-run`) mode only, once `$EDITOR`
    closes with a non-blank message: before running the `git commit`/`jj
    commit`\|`describe`\|`split` invocation (see "git commit"/"jj commit commands").
- `Committed using: <command>` — once it succeeds.

### git commit

For a git repository (including a colocated repository run with `--git`), `ccm` commits
with a single fixed argument template, using the same `{{message}}` placeholder the jj
templates below use — no `{{files}}` slot: git handling never accepts
`--include`/`--exclude` (see "Diff scope resolution" above), so there's no file scope to
pass, and the commit always covers the current stage as-is:

| Argument template |
|---|
| `commit -m {{message}}` |

Same argv-safety guarantee as "jj commit commands" below: `ccm` never builds a shell
command line or passes this through `sh -c`; `{{message}}` is substituted as a single
literal `Command::arg()` entry, so an unescaped multi-line commit message is passed
through as-is with no shell quoting/escaping needed — the same reason `jj commit -m
{{message}}` doesn't need `-F <path>` either, and why `ccm` doesn't need it for git
either. No `--no-verify`: `git commit` runs with the repository's hooks exactly as it
normally would (e.g. `pre-commit`, `commit-msg`) — `ccm` doesn't suppress or otherwise
special-case them, the same as it doesn't for jj's own hooks. A failure here (including
a hook rejecting the commit) is exit code 16, the same commit-invocation failure code jj's
commands use (see Error Handling).

### jj commit commands

The picker described in this section only runs once the cleaned, edited message has
been confirmed non-blank (see "Message pre-population and cleanup" above and "Check
order" below) — a message that turns out blank aborts with exit code 15 before the
picker is ever shown.

Each jj subcommand `ccm` can invoke is built from a fixed argument template using two
placeholders: `{{files}}` expands to zero or more file-path arguments — the paths
resolved from `--include`/`--exclude`, or nothing at all if neither was given (the
command then acts on the whole working copy, jj's own default) — and `{{message}}`
expands to the final commit message (as edited and saved, or as generated under
`--dry-run`'s preview before it's discarded — `--dry-run` never actually runs these).

The templates below read like shell strings for readability only. `ccm` never builds a
shell command line or passes them through `sh -c`; each template is a fixed argv list,
and `{{files}}`/`{{message}}` are substituted as literal `Command::arg()` entries — one
argv slot per file, and the full (possibly multi-line) commit message as a single argv
slot for `{{message}}`. This is what makes an unescaped multi-line message or a
`--include`d path containing spaces or shell metacharacters safe to pass through as-is.
A literal `--` argument precedes `{{files}}` in the `jj commit`/`jj split` templates so
that a resolved path beginning with `-` (an unusual but valid filename) is still parsed
by jj as a positional fileset argument rather than mistaken for an unrecognized flag —
`--` marks the end of flag parsing the same way it does for any other clap-based CLI.
This is harmless even when `{{files}}` expands to zero arguments: a trailing `--` with
nothing after it is a no-op, so `jj commit`/`jj split` still act on the whole working
copy exactly as when `--include`/`--exclude` weren't given.

| Pick | Argument template |
|---|---|
| `jj commit` | `commit -m {{message}} -- {{files}}` |
| `jj describe` | `describe -m {{message}}` |
| `jj split` | `split -m {{message}} -- {{files}}` |

`jj commit -i` is deliberately not offered: it's just `jj commit`'s interactive mode
(prompting the user to hunk-select within each file), and none of the commands `ccm`
invokes to commit the generated message run interactively — the file scope is already
settled by `--include`/`--exclude` before the picker ever runs.

`jj describe` has no `{{files}}` slot: it only sets the description of the current
working-copy revision and doesn't take a file pathspec, so `--include`/`--exclude`
affect what diff the message is generated from but not what `describe` records. This is
a footgun when the two scopes diverge — a message describing only the included/excluded
subset would end up attached to the whole working-copy revision regardless — so when
`--include` or `--exclude` was given, `ccm` omits `jj describe` from the picker
entirely: only `jj commit` and `jj split` are offered, both of which take `{{files}}`
and so stay scoped to what the message actually describes.

`jj split` has the opposite footgun when neither `--include` nor `--exclude` was given:
`{{files}}` then expands to zero file arguments, and `jj split` treats an empty fileset
as "no filesets provided," which per its own `-i`/`--interactive` help text ("This is the
default if no filesets are provided") falls back to `jj`'s interactive diff-editor —
exactly the interactive prompt this section guarantees `ccm` never triggers. So when
neither flag was given, `ccm` omits `jj split` from the picker instead: only `jj commit`
and `jj describe` are offered, both of which act non-interactively on the whole working
copy. In short, exactly one of `jj describe`/`jj split` is available at a time — never
both, and never neither: `--include`/`--exclude` given → `jj commit` + `jj split`;
neither given → `jj commit` + `jj describe`.

Since there are never more than two choices on offer, the picker is a plain numbered
prompt rather than a fuzzy-finder UI: `ccm` prints the two choices to stderr as `0) jj
commit` and `1) jj describe` (or `1) jj split`, whichever applies), then reads a line
from stdin. The line is trimmed of leading/trailing whitespace before being checked —
so e.g. a trailing `\r` from a CRLF terminal, or an accidental leading/trailing space,
doesn't turn a valid `0`/`1` into an invalid entry — and if the trimmed result is exactly
`0` or `1` that selects the corresponding command.

`jj commit` (index 0) is always the picker's default, marked as such in the printed menu
— `0) jj commit  (default)` — since it's the one choice that's always on offer regardless
of scope (see the argument-template table above). A blank line (the user just pressing
Enter, with no index typed) selects it directly, without needing to type `0`. Any other
invalid input (anything that isn't exactly `0`, `1`, or blank — e.g. `2`, `y`) still
reprints the two choices and re-prompts. True EOF (Ctrl-D) on stdin is a separate signal
from a blank line and is unaffected by the default: it still cancels the picker outright
regardless of whether a default is available. If the user cancels — EOF without having
entered a valid index — `ccm` aborts immediately — a blank *line* would have selected the
default, but EOF is not a blank line — and does not commit — and exits with code 14.

## Configuration

Config directory: the default described under `--config` in Flags
(`$XDG_CONFIG_HOME/ccm/` if `$XDG_CONFIG_HOME` is set to a non-empty value, otherwise
`~/.config/ccm/`), or the directory given via `--config` — containing two files.

### `prompts.yaml` — named prompt templates

A top-level mapping keyed by prompt name — referenced from `api.yaml`'s `prompt:` field.

```yaml
default:                       # key = prompt name, referenced from api.yaml's `prompt:` field
  system: |                    # optional; system/instruction message
    You are an expert software engineer who writes clear, conventional commit messages.
  template: |                  # required; plain text, no placeholders
    Write a commit message for the following changes.
```

Fields (per key):

- `system` (string, optional) — system/instruction message. Used as-is for `openai_api`
    entries, sent as the `system` role message (see Message construction below). For
    `agent_cli` entries, substituted into `{{system}}` wherever that placeholder appears
    in the entry's `args` (see `args` under `type: agent_cli` fields) — whether the
    underlying agent CLI supports a system prompt at all is entirely up to whether its
    `args` template includes `{{system}}`. If `args` has no `{{system}}` placeholder
    (the agent CLI has no such flag to wire up), `system` has no effect and is silently
    unused; to get equivalent content into such an entry, put it directly in that
    entry's `template` instead — e.g. as a `\n\n`-separated preamble at the top of it.
    This is purely a user-authoring convention with nothing for `ccm` to parse or
    enforce; it just echoes the `\n\n` `ccm` itself inserts between `template` and the
    diff for `openai_api` entries (see Message construction), a mechanism that doesn't
    apply to `agent_cli` at all (its diff goes to the child process's stdin, never
    concatenated into `template`).
- `template` (string, required) — the prompt body, used as-is with no placeholder
    substitution — it has neither a `{{diff}}` nor a `{{files}}` placeholder, since the
    diff already names every changed file. Instead, `ccm` appends the diff after the
    template when building the `openai_api` user message (see Message construction
    below); for `agent_cli` entries the diff is written to the child process's stdin
    instead (see `args` under `type: agent_cli` fields), keeping it out of the
    command-line argument that carries the template and avoiding `ARG_MAX`.

### `api.yaml` — ordered list of tools/APIs to try

A top-level list — the first entry that is `enabled` wins (see Selection).

```yaml
- name: OmniRoute              # unique id, used in error messages/logs
  type: openai_api             # openai_api | agent_cli
  enabled: true
  prompt: default              # references a prompts.yaml key
  base_url: http://localhost:20128/v1
  model: gpt-5.4
  api_key:
    env: OMNIROUTE_API_KEY     # exactly one of: env | value
  max_tokens: 300
  temperature: 0.2

- name: gemini-cli
  type: agent_cli
  enabled: true
  prompt: default
  command: gemini
  model: gemini-2.5-pro
  args: ["-m", "{{model}}", "-p", "{{prompt}}"]
```

Common fields (all entry types):

- `name` (string, required, unique) — used in error messages/logs.
- `type` (`openai_api` | `agent_cli`, required).
- `enabled` (bool, default `true`) — set `false` to skip without deleting the entry.
- `prompt` (string, required) — name of a `prompts.yaml` key.
- `timeout` (integer, optional, seconds, default `60`) — wall-clock limit on the tool/API
    call once this entry is selected. Since Requirements 2 and 3 rule out any fallback,
    this is what stands between a hung network call or a hung agent CLI and `ccm` never
    returning at all. Applies per entry, so a slow agent CLI can be given more headroom
    than a fast HTTP endpoint without affecting every other entry. Exceeding it is a hard
    failure like any other tool/API-call failure — exit code 10 (see Error Handling), not
    a special case — and the stderr message states the configured timeout so it reads
    differently from a fast failure like a connection refusal. Enforcement differs by
    `type`: see `timeout` under `type: openai_api` fields and `type: agent_cli` fields
    below.

`type: openai_api` fields:

Message construction: the resolved prompt's `system` (if set) is sent as the `system`
role message. The `user` role message is the `template` text, verbatim (no trimming of
its own trailing whitespace), followed by a `\n\n` separator `ccm` inserts, followed by
the computed diff verbatim — the template text itself never contains the diff or a file
list, and `ccm` never inserts a label like `Diff:` before it, since the diff output
itself (git's or jj's, whichever "Diff generation" produced) already makes clear where
it starts — in a standard chat completion request.

- `base_url` (string, required).
- `model` (string, required).
- `api_key` (object, required) — exactly one of:
    - `env: <VAR_NAME>` — read from an environment variable.
    - `value: <literal>` — inline key; discouraged, mainly for local testing.
- `max_tokens` (integer, optional).
- `temperature` (float, optional).
- `headers` (map<string,string>, optional) — extra HTTP headers.
- `timeout` (see `timeout` under Common fields above) — applied as the total request
    timeout (connect + send + receive) for the HTTP call to `base_url`. A response that
    hasn't fully arrived within this window is aborted and reported as exit code 10, the
    same as a network error.

`type: agent_cli` fields:

- `command` (string, required) — binary name or path, resolved on `$PATH` (or as an
    absolute/relative path) only when `ccm` actually spawns it — not checked earlier at
    selection time (see Selection). If it doesn't resolve, that's a tool/API call
    failure like any other, exit code 10.
- `model` (string, required) — the model name to request from the agent CLI, passed
    through exactly as configured (via `{{model}}` substitution below) with no local
    validation against the agent CLI's own model list. If the agent CLI rejects it
    (e.g. because it's stale or renamed), that surfaces at call time as a tool/API call
    failure — see exit code 10.
- `args` (list<string>, required) — arguments passed to the command; `{{prompt}}`,
    `{{model}}`, and `{{system}}` are substituted with the rendered prompt, the
    configured model, and the resolved prompt's `system` text respectively. The rendered
    prompt substituted for `{{prompt}}` is just the `template` text — it never contains
    the diff or a file list. `{{system}}` is optional to include: an agent CLI with a
    native system-prompt flag wires it up by including `{{system}}` in `args` (e.g.
    `"--system"`, `"{{system}}"`); an agent CLI without one simply omits `{{system}}`
    from `args` entirely, in which case `system` (if set in `prompts.yaml`) has no
    effect (see `system` under `prompts.yaml` fields). If `{{system}}` is present in
    `args` but the resolved prompt has no `system` set, it's substituted with an empty
    string. The computed diff is instead written to the child process's stdin and stdin
    is then closed, so the agent CLI reads it separately from the `-p`/`{{prompt}}`
    argument; this keeps a large diff out of `args` entirely and avoids `ARG_MAX`.
- `env` (map<string,string>, optional) — extra environment variables for the child process.
- `timeout` (see `timeout` under Common fields above) — wall-clock limit measured from
    spawning the subprocess. This is the safety net for an agent CLI that hangs for a
    reason `ccm` can't prevent directly — a network call stuck inside the agent CLI
    itself, or an interactive prompt of its own despite stdin already being closed (see
    `args` above) — which would otherwise hang forever, since Requirement 4 rules out a
    fallback that could paper over it. On expiry, `ccm` sends `SIGTERM`, waits a
    2-second grace period for the process to exit on its own, then sends `SIGKILL` if
    it's still running; either way, this is reported as exit code 10 once the process
    has actually been reaped, not while the grace period is still running.

### Selection

Before selection begins, `ccm` validates every entry's `name:` for uniqueness and its
`prompt:` field (both `openai_api` and `agent_cli`) against `prompts.yaml`. Both checks
compare strings exactly, byte-for-byte — case-sensitive, no trimming or normalization —
so e.g. `omniroute` and `OmniRoute` count as distinct names, not a duplicate. If any two
entries share a `name`, or any entry references a prompt name that isn't defined in
`prompts.yaml`, that's a config error (exit 5) — `ccm` doesn't attempt selection at all,
and it doesn't matter whether the offending entry would otherwise have been enabled.

`api.yaml` is a priority-ordered list. Absent a non-empty `--tool <NAME>` (below), `ccm`
selects the first entry with `enabled: true` — nothing more — whenever that's
unambiguous: under `--dry-run`, always (unless `--tool ''` forces the picker, below); in
default mode, whenever exactly one entry is enabled. There is
deliberately no local "availability" probe of any kind here (no checking that an
`agent_cli`'s `command` resolves on `$PATH`, no reachability check for an `openai_api`'s
`base_url`, no validation of `model` against either tool's own model list): Requirement 4
rules out fallback entirely, so there is no second choice to fall back to even if such a
probe found a problem — the only thing a probe could do is turn a selection-time failure
(exit 6) into an earlier, differently-coded one, which isn't worth the added mechanism
(see Requirement 4 for why an earlier design that *did* probe per-entry availability was
dropped).

`--tool <NAME>` (see CLI Interface) chooses the entry a different way for one run,
without touching `api.yaml` itself — this is a per-invocation override, not a second
fallback tier, so Requirement 4's "no fallback" still holds: exactly one entry is still
tried, it's just chosen a different way. A non-empty `NAME` selects the entry whose
`name` matches it exactly — the same byte-for-byte, case-sensitive comparison used for
the uniqueness check above — regardless of its `enabled` flag; a disabled entry is a
legitimate target. No match is a selection failure (exit 6), same as "every entry
disabled." (Since entry names aren't required to be non-empty, an entry literally named
`""` — not a config anyone would write intentionally — couldn't be reached this way;
it would still be reachable through the picker itself, just not by typing its name.)

An empty `NAME` (`--tool ''`) is not a name to match at all: it's a sentinel that forces
the picker below for this run, regardless of `--dry-run` or `enabled_count` — a
deliberate, one-off way to browse every entry, including disabled ones, without editing
`api.yaml` (e.g. to try a normally-disabled tool/API).

In default mode absent a non-empty `--tool`, whenever the first-enabled rule can't
resolve to a single entry on its own — zero entries enabled, or 2+ enabled — or,
regardless of mode, whenever `--tool ''` was given, `ccm` instead shows the tool picker:
the same name/type/enabled listing `--list-tools` does, in `api.yaml` order, and selects
whichever entry the user picks, regardless of `enabled` (so a disabled entry stays
selectable this way, same as a named `--tool`). Cancelling the prompt is exit 14, not exit
6, since a tool *was* available to select — the user simply didn't finish picking one
(see "Interactive terminal requirement"). Note the mode split: absent `--tool` entirely,
this prompt never appears under `--dry-run`, which always applies the first-enabled rule
regardless of how many entries are enabled; `--tool ''` is the one way to reach it under
`--dry-run` too (see "Interactive terminal requirement").

The tool picker has two front-ends over that one candidate list (see "Preferences of
Dependencies" item 8): `fzf` (https://github.com/junegunn/fzf), shelled out to as a
subprocess when it's found on `$PATH`, stdin is a real terminal, and the environment
variable `CCM_FUZZY` is not set to `0`; and a plain numbered stdin prompt otherwise —
the only front-end that exists at all if `fzf` isn't installed, and the one every
scripted/piped caller always gets regardless. The two differ only in how a selection is
made and are otherwise identical: same candidate order, same disabled-entry
selectability, same exit 14 on cancellation (EOF for the numbered prompt; an explicit
abort — Esc, Ctrl-C, or confirming with nothing matched — for `fzf`). If `fzf` can't run
for any reason (not found, stdin isn't actually a working terminal despite
`stdin_is_terminal()` reporting true, or it exits with anything other than a clean
selection or an explicit abort), `ccm` notes why on stderr and falls back to the
numbered prompt on stdin — behavior is never worse than without `fzf` installed.

`CCM_FUZZY=0` is an escape hatch for the opposite case: a terminal where `fzf` *does*
start but its inline TUI renders or behaves badly. Setting it to exactly `0` makes `ccm`
skip the `fzf` front-end entirely and go straight to the numbered prompt, with no
"fzf unavailable" note on stderr (nothing went wrong — it wasn't attempted). Any other
value, including unset or empty, leaves `fzf` enabled. It has no effect on any other
part of the run and none on `--dry-run` without `--tool ''` (which never reaches the
tool picker at all).

One behavior is specific to the numbered prompt and doesn't carry over: a blank line
(Enter with no index typed) selects the entry marked `(default)` in the listing — the
same one `select_first_enabled` would pick automatically when there's exactly one enabled
entry — without needing to type its number; when nothing is enabled there is no
`(default)` entry, so a blank line simply re-prompts there too, the same as any other
invalid input; EOF is a separate signal from a blank line and is unaffected by the
default — it still cancels even when one is available. `fzf` has no equivalent of a blank
line: Enter always confirms whichever candidate is highlighted, and the initial highlight
is the first entry in `api.yaml` order (not necessarily the `(default)`-marked one),
though that marker is still visible in the list either way.

Either way, the name-uniqueness and prompt-reference validation above still runs first,
unconditionally — a `--tool` run or a tool-picker prompt is never reached with an
otherwise-invalid `api.yaml`.

Once an entry is selected, everything else is a hard failure with no fallback to the
next entry — including the configured `agent_cli` `command` not resolving at all,
API key resolution failing, network/connection errors, exceeding the entry's `timeout`
(see `timeout` under Common fields), non-2xx or API-level error responses, and non-zero
exits from an agent CLI (e.g. an agent CLI rejecting the configured `model` with
something like `Error: invalid model selection` — see exit code 10).

## Error Handling

`ccm` exits with a distinct, documented code per failure mode, and prints a
human-readable message to stderr, so the Lua wrapper can branch on the exit code
without parsing message text.

### Check order

These checks run in a fixed pipeline; the first failure wins. When more than one
failure condition applies at once (e.g. running outside any repo, with `api.yaml`
missing, and `--include` given), the earliest stage below determines the exit code —
later stages are never reached:

1. **Argument shape** — usage errors that don't depend on repo state: `--include` and
   `--exclude` together, `--gen-config` combined
   with any flag other than `--config`, `--list-tools` combined with any flag other than
   `--config` (exit code 2). If `--gen-config` is present and passes this check, `ccm`
   creates the config directory (the default described under `--config` in Flags, or the
   directory given via `--config`) and the missing config file(s) (exit code 3 on
   failure) and exits, skipping every stage below. Otherwise, if `--list-tools` is
   present and passes this check, `ccm` loads and validates
   `prompts.yaml`/`api.yaml` from the config directory (exit code 5 on failure, same as
   stage 4 below), prints the entry listing (exit code 0), and exits, likewise skipping
   every stage below — including repository detection.
2. **Repository detection** — the git/jj checks in "Repository detection" (exit code 4
   if neither succeeds).
3. **Repo-dependent argument validation** — usage errors that need the repo type from
   step 2: `--git` outside a git repository, `--include`/`--exclude` when the repository
   is being handled as git (exit code 2).
4. **Config load & validation** — reading, parsing, and cross-validating
   `prompts.yaml`/`api.yaml` from the config directory (the default described under
   `--config` in Flags, or the directory given via `--config`) (exit code 5).
5. **Tool/API selection** — if a non-empty `--tool <NAME>` was given, the entry it names
   (exit code 6 if no entry has that name); if `--tool ''` was given, whichever entry the
   tool picker selects (`fzf` if it's on `$PATH`, stdin is a real usable terminal, and
   `CCM_FUZZY` isn't `0`; a numbered stdin prompt otherwise — see "Selection"),
   regardless of mode (exit code 14 if cancelled); otherwise the first `enabled` entry in
   `api.yaml`, taken automatically
   whenever that's unambiguous (always under `--dry-run`; in default mode, whenever
   exactly one entry is enabled — exit code 6 if none are enabled under `--dry-run`), or,
   in default mode when it isn't unambiguous (zero or 2+ entries enabled), whichever
   entry the tool picker selects (exit code 14 if cancelled — see "Selection"). This runs
   before diff generation: it's a trivial list scan (or, in default mode, or under
   `--tool ''` in any mode, possibly an interactive picker of one front-end or the other)
   with no probing of any kind, so a failure here — an `api.yaml` with every entry
   disabled under `--dry-run`, an unmatched non-empty `--tool` name, or a cancelled
   tool-picker prompt — fails fast without first paying for a potentially large diff.
6. **Diff generation** — for jj with `--include`/`--exclude`, first running the `jj diff
   --summary` enumeration call (exit code 7 if this subprocess itself fails, see "Diff
   scope resolution"); then resolving `--include`/`--exclude` against the enumerated
   paths, where an unmatched path is a usage error (exit code 2, jj only); then running
   the final `git diff`/`jj diff` invocation (exit code 7 as well) and confirming the
   result is non-empty (exit code 8).
7. **Generation** — API key resolution (exit code 9), the tool/API call itself (exit
   code 10), response parsing (exit code 11), and the response-cleanup pass (see
   "Response cleanup") — the last of which cannot itself fail, only change what a later
   blank check (exit code 15, stage 8) sees.
8. **Review, editor & commit** — under `--dry-run`, this stage is just the empty-message
   check (exit code 15) — no review prompt, editor, picker, or commit ever runs.
   Otherwise: the message review prompt (exit code 14 if cancelled — see "Message review
   prompt"), looping on regenerate (each iteration re-running stage 7, any of whose
   failure modes — exit codes 9, 10, 11 — can recur) until edit or accept is chosen; for
   edit, `$EDITOR`/fallback resolution (exit code 12), creating and pre-populating the
   temp file (exit code 17), running `$EDITOR` (exit code 13), reading back the temp
   file it saved (exit code 1 if the file has gone missing or unreadable by then, since
   neither exit 13 nor exit 15 apply); for either edit or accept, cleanup and the
   empty-message check (exit code 15); then — only once the cleaned message is confirmed
   non-blank — the jj commit-command picker (exit code 14, jj only) and the final
   `git commit`/`jj commit` invocation (exit code 16).

So for the example above — no repo, missing `api.yaml`, `--include` given — step 2
fails first with exit code 4; the `--include` usage error (step 3) and the config error
(step 4) are never evaluated.

Rows below are ordered to match the check-order pipeline above (cross-cutting codes 0
and 1 first, then each stage in sequence, renumbered so the code column is strictly
increasing top to bottom); exit code 2 recurs across three stages (argument shape,
repo-dependent argument validation, and diff-scope path validation) so it's listed
once, at its earliest possible stage. Two codes are exceptions to the strictly-increasing
ordering, both kept at their numeric position in the table (rather than their pipeline
position) to avoid renumbering the rest of the editor/commit-stage codes: exit code 17
(temp file creation/write failure) falls between exit codes 12 and 13 in the pipeline
above but is listed at the end of the table, after 16; and exit code 14 (the jj
commit-command picker) falls after exit code 15 in the pipeline — the picker only runs
once the cleaned message is confirmed non-blank, see "Check order" above — but is listed
before 15 in the table.

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic/unexpected error |
| 2 | Usage error — invalid or conflicting CLI arguments |
| 3 | `--gen-config` failed — could not create the config directory (the default described under `--config` in Flags, or the directory given via `--config`) or write `prompts.yaml`/`api.yaml` (e.g. permission denied, disk full, path exists as a non-directory) |
| 4 | Not a git or jj repository |
| 5 | Config error — `api.yaml`/`prompts.yaml` missing, unreadable (e.g. permission denied), fails to parse, fails validation (including a duplicate `name` across `api.yaml` entries), references an unknown prompt, or `api.yaml` is an empty list; looked up in the config directory (the default described under `--config` in Flags, or the directory given via `--config`) |
| 6 | Tool/API selection failed — under `--dry-run` with no `--tool`, every `api.yaml` entry is disabled (default mode instead prompts in this case — see "Selection"), or a non-empty `--tool <NAME>` matched no entry |
| 7 | Diff generation failed — the underlying `git diff`/`jj diff` invocation itself errored; for jj with `--include`/`--exclude` this also covers the `jj diff --summary` enumeration call failing (see "Diff scope resolution") |
| 8 | Nothing to diff — no staged changes (git), or an empty working-copy diff (jj) after applying `--include`/`--exclude` |
| 9 | API key resolution failed |
| 10 | Tool/API call failed — network error, exceeding the entry's `timeout` (see `timeout` under Common fields; for `agent_cli` the subprocess is killed via `SIGTERM`, then `SIGKILL` if it hasn't exited within the grace period, before this is reported, and any stderr output it had produced up to that point is still included in the reported error, same as any other `agent_cli` failure), non-zero exit, or an explicit API-level error response. For `agent_cli`, this also covers the agent CLI rejecting the configured `model` at run time (e.g. `Error: invalid model selection`), since `model` is passed through unvalidated — `ccm` includes the agent CLI's stderr output in the reported error so the user can tell a bad model apart from other tool failures. For `openai_api`, a non-2xx response's body is surfaced the same way — included in the reported error alongside the HTTP status code, not just the status code alone — so the user can see the actual routing/provider error text (e.g. from OmniRoute) behind a failure, the same diagnostic value the `agent_cli` stderr inclusion provides. |
| 11 | Malformed response — `openai_api` only: the response could not be parsed, or parsed but missing the expected message content. Not applicable to `agent_cli`, whose output is raw stdout text; a bad `agent_cli` result surfaces as exit code 10 (non-zero exit) or exit code 15 (blank result), never 11. |
| 12 | Editor unavailable — `$EDITOR` is set but doesn't resolve to an executable, or `$EDITOR` is unset and none of `nvim`/`vim`/`vi` are found on `$PATH` |
| 13 | Editor aborted — `$EDITOR` (or the `nvim`/`vim`/`vi` fallback) exited with a non-zero status. The temp file's content is not read, and nothing is committed, regardless of what was saved before the editor exited. |
| 14 | Aborted — an interactive picker was cancelled (EOF before a valid selection, or — in the tool picker's `fzf` front-end — an explicit abort): the tool picker at stage 5 — default mode, or any mode when `--tool ''` was given (see "Selection") — or, at stage 8, the message review prompt (see "Message review prompt") or the jj-command picker (see "jj commit commands"). In the stage-8 cases, nothing is committed. |
| 15 | Empty commit message — blank after the applicable check (see "Message pre-population and cleanup"): under `--dry-run`, the tool/API response — after the response-cleanup pass (see "Response cleanup") — is empty or all whitespace, checked immediately; otherwise, the `$EDITOR`-saved file is blank after cleanup, checked after `$EDITOR` closes, since the user may have written one in over a blank generation. |
| 16 | Commit failed — `git commit` / `jj commit`\|`describe`\|`split` invocation failed |
| 17 | Temp file creation/write failed — creating the `$EDITOR` temp file (e.g. `tempfile::Builder::new().prefix("CCM_EDITMSG_").tempfile_in(...)` erroring because the OS temp directory is unwritable or the disk is full) or writing the pre-populated content into it failed. Falls between exit codes 12 and 13 in the pipeline (see "Check order" above); listed here, out of numeric sequence, to avoid renumbering 13–16. |

## Preferences of Dependencies    

1. I prefer the Rust crate https://crates.io/crates/litellm-rs
    for making API calls, used as a plain client library (e.g. `litellm_rs::completion(...)`)
    rather than its gateway/server functionality. Depend on it with
    `default-features = false, features = ["lite"]` to avoid pulling in the
    gateway/server-side dependency tree. `litellm-rs` is the Rust counterpart of the
    widely-used Python `litellm` library — despite being a young crate, it's not an
    immaturity risk on that basis.

2. Use the `jj` and `git` command-line tools (invoked as subprocesses) for working with
    jj and git repositories, rather than the `jj-lib` or `gitoxide` crates. The one
    exception is repository *detection* itself, which walks the filesystem directly
    instead of shelling out to either tool (see "Repository detection") — that's what lets
    `ccm` tell git and jj repositories apart without requiring both binaries installed.

3. The jj commit-command picker (see "jj commit commands") never offers more than two
    choices at a time — `jj commit` plus either `jj describe` or `jj split`, never both
    and never neither — so it doesn't warrant a fuzzy-finder crate like
    https://crates.io/crates/skim. It's implemented as a plain numbered stdin prompt
    (see "jj commit commands") using only the standard library. This reasoning is
    specific to prompts with a small, fixed choice count: the jj commit-command picker
    here (never more than two) and the message review prompt (item 7, three actions). It
    deliberately does *not* extend to the tool picker (see "Selection"), whose candidate
    list is `api.yaml` itself and is therefore open-ended — see item 8.

4. Use the `clap` crate (https://crates.io/crates/clap), with the `derive` feature, for
    parsing `ccm`'s own command-line arguments (see CLI Interface).

5. Use the `shell-words` crate (https://crates.io/crates/shell-words), specifically
    `shell_words::split()`, to word-split `$EDITOR`'s value (see "Default behavior" under
    CLI Interface). It's a small, well-established implementation of POSIX-ish word
    splitting with quote and backslash-escape handling, avoiding a hand-rolled parser for
    something `ccm` doesn't otherwise need to reimplement.

6. Use the `tempfile` crate (https://crates.io/crates/tempfile) to create the `$EDITOR`
    temp file (see "Default behavior" under CLI Interface):
    `tempfile::Builder::new().prefix("CCM_EDITMSG_").tempfile_in(std::env::temp_dir())`.
    This gets `ccm` a collision-proof unique filename for free — `tempfile` appends its
    own randomized suffix after the `CCM_EDITMSG_` prefix — without `ccm` having to hand-roll
    uniqueness itself (e.g. via a timestamp or PID, both of which can collide across
    near-simultaneous runs). Since the temp file must survive process exit unconditionally
    (see "Default behavior"), `ccm` calls `.keep()` — or, if only the path is needed and
    not an open `File` handle, `.into_temp_path().keep()` — immediately after creating
    it. Either disarms `tempfile`'s delete-on-drop at the file's own already-generated
    path, converting it from a delete-on-drop `NamedTempFile` into a plain, ordinary
    file at that same path; `.persist(path)` isn't the right call here since that's for
    moving the file to a *different* path than the one `tempfile` picked, which `ccm`
    has no need to do. Every subsequent read/write against the file (writing the
    initial pre-populated content, `$EDITOR` editing it, `ccm` reading it back) happens
    through that same path like any other file, with no further involvement from
    `tempfile`.

7. The message review prompt (see "Message review prompt") reads a single keypress on a
    real terminal, which needs raw terminal mode (clearing `ICANON`/`ECHO` for the
    duration of that one read). Rather than a terminal-UI crate like
    https://crates.io/crates/crossterm or https://crates.io/crates/console, `ccm` uses
    the `term` feature of `nix` (https://crates.io/crates/nix, already a dependency —
    see item 3's reasoning, which applies equally to this prompt's fixed three-way
    choice; the open-ended tool picker is the one deliberate exception, item 8) to call
    `tcgetattr`/`tcsetattr` directly. When stdin isn't a real terminal, the prompt falls
    back to reading a whole line instead (see "Message review prompt"), so this
    dependency is only ever exercised on a genuine interactive run.

8. The tool picker (see "Selection") is the one deliberate exception to items 3 and 7's
    reasoning against a fuzzy-finder/TUI crate: its candidate list is `api.yaml` itself,
    which has no bounded size, so fuzzy search over tool names is a genuine usability win
    once many tools are configured — unlike the jj commit-command picker or the message
    review prompt, whose choice counts are small and fixed. Rather than embedding a
    fuzzy-finder *library* (rejected in item 3 for the jj picker, and a poor fit here too
    — evaluated and declined: https://crates.io/crates/skim pulls in an unconditional,
    whole-process `#[global_allocator]` override and a native-code build step just to
    link against it, regardless of which of its own Cargo features are enabled), `ccm`
    shells out to the `fzf` command-line tool (https://github.com/junegunn/fzf) as an
    optional subprocess, the same way it already shells out to `git`/`jj` themselves
    (item 2) rather than embedding a library for those either. Concretely:
    - `ccm` writes the same candidate list `--list-tools` prints to `fzf`'s stdin and
      reads the selected line back from its stdout — the standard `command | fzf`
      idiom, just invoked directly rather than through a shell pipe. `fzf`'s own
      interactive rendering and keyboard input go through `/dev/tty` directly,
      independent of how `ccm` has wired its stdin/stdout/stderr.
    - It's only attempted when stdin is a real terminal (see "Interactive terminal
      requirement") and `CCM_FUZZY` isn't set to `0` (an escape hatch for a terminal
      that mishandles `fzf`'s inline TUI — see "Selection"); if `fzf` isn't found on
      `$PATH`, doesn't have a controlling terminal available to it, or fails for any
      other reason, `ccm` notes why on stderr and falls back to the numbered stdin
      prompt the tool picker has always had — behavior is never worse than without
      `fzf` installed.
    - This adds no new dependency to `Cargo.toml` at all: `ccm` builds and runs
      identically whether or not `fzf` happens to be on the user's machine, exactly
      like its optional `nvim`/`vim`/`vi` `$EDITOR` fallback chain (see "Default
      behavior") already works whether or not any of those happen to be installed.
    - It's scoped to this one prompt. The jj commit-command picker and the message
      review prompt are unaffected and stay exactly as items 3 and 7 describe.
