# Contextual Commit Message (ccm) - Design Doc

This document describes *how* `ccm` implements the behavior specified in `prd.md`, and
*why* it is implemented that way. `prd.md` is authoritative on user-visible behavior
(flags, prompts, progress lines, exit codes, config format); this document covers the
implementation choices, rejected alternatives, and accepted trade-offs behind it. Section
names below correspond to the `prd.md` sections (or paragraphs) they elaborate on.

## Tool/API selection: no availability probing

`ccm` selects the first `enabled` entry in `api.yaml` (or the one chosen via `--tool`/the
tool picker) without probing whether it will actually work. An earlier version of this
design had `ccm` check each entry's local "availability" — e.g. whether an `agent_cli`'s
`command` resolves on `$PATH` — before selecting it, skipping to the next `enabled` entry
otherwise. That mirrored the Lua script this replaces, where the same probing doubled as
an implicit fallback chain and, incidentally, as a way to auto-switch between
machine-specific tool profiles depending on what was installed locally. Neither reason
applies here: fallback is explicitly out of scope (`prd.md` Requirement 4), and
profile-switching is already handled directly by `api.yaml`/`prompts.yaml` being real
per-machine config files. So availability checking was dropped.

There is deliberately no local "availability" probe of any kind (no checking that an
`agent_cli`'s `command` resolves on `$PATH`, no reachability check for an `openai_api`'s
`base_url`, no validation of `model` against either tool's own model list): with no
fallback, there is no second choice to fall back to even if such a probe found a problem
— the only thing a probe could do is turn a selection-time failure (exit 6) into an
earlier, differently-coded one, which isn't worth the added mechanism.

## Repository detection

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

When both roots are found and differ, one is necessarily an ancestor of the other, since
both were found by walking up from the same starting directory — the two can't be
unrelated. That's why `prd.md` can resolve that case simply by treating the *deeper* root
as the actual repository.

## Diff generation

`ccm` captures the diff by running the underlying tool as a subprocess with piped (not
inherited) stdout, so the diff is never written to an actual terminal — this alone means
git's and jj's color and pager logic (both default to `"auto"`, i.e. only active when
stdout is a TTY) stay off. `ccm` additionally passes explicit flags (`--no-pager`,
`--no-color`/`--color=never`, see `prd.md` "Diff generation") as defense-in-depth against
a user's global config forcing color/pager on regardless of TTY (e.g.
`color.diff = always`).

Subprocesses are spawned and captured in one place (`vcs/exec.rs`), which pumps
stdin/stdout/stderr on separate threads so a large diff can't deadlock on a full pipe.

## Diff scope resolution

### Why not jj's fileset `~` operator

`jj diff` has no flag that means "these files minus these excluded ones", but jj's
fileset language does support set difference directly (the `~` operator, e.g.
`jj diff -- 'all() ~ file:"a.rs"'`; see `jj help -k filesets`). `ccm` doesn't use that
route: a `file:` pattern that matches nothing (e.g. a typo'd path) is silently accepted
by the fileset language rather than rejected, and `ccm` needs to surface a typo'd
`--include`/`--exclude` path as a usage error (exit code 2) instead of silently diffing
an empty or unintended scope. So `ccm` first enumerates the full set of changed paths
itself and resolves `--include`/`--exclude` against that list.

### The enumeration call

The enumeration produces one entry per changed file rather than a deduplicated set of
bare strings (see the rename/copy handling below for why that distinction matters). It
runs `jj --no-pager diff --color=never -T <template>`, using the same
color/pager-suppressing flags as "Diff generation" above since this output is parsed
programmatically too. `<template>` is jj's own template-expression language (`jj help -k
templates`), not a flag `ccm` invents: it renders one
`<status-char><SEP><source-path><SEP><target-path>` record per line (`\n`-terminated,
`SEP` the ASCII "unit separator" `\x1f`, chosen because no realistic path can ever
contain it) built from `TreeDiffEntry`'s own `status_char`/`source`/`target` accessors —
see `vcs::argv::ENUMERATE_TEMPLATE` for the exact expression. `.display()` in the
template reports each path relative to the current working directory, the same basis
`--include`/`--exclude` are given in. Parsing lives in `vcs/summary.rs`. The call is not
itself progress-logged: it runs fast enough, and often enough, that a dedicated pair of
lines would just be noise.

A rename/copy record's `<source>` and `<target>` fields are simply `<old>` and `<new>` —
jj's `TreeDiffEntry::source`/`::target` accessors report the true old/new path directly,
with no shared-prefix/suffix factoring or brace notation to recover them from. For a
plain modification/addition/deletion (`M`/`A`/`D`) record, `<source>` and `<target>` are
simply identical — jj's template emits both fields unconditionally, not just for a
rename/copy — so `ccm` discards `<source>` for those statuses rather than expose the
redundant duplicate; only a rename/copy's independent `<old>` name is ever useful
downstream.

### Why not `jj diff --summary`

`ccm` used to run `jj diff --summary` for this and parse its human-oriented output
instead, where a rename/copy line shows `<old>`/`<new>` factored into a shared
prefix/suffix with the differing middle in braces (e.g. `R src/{old.rs => new.rs}`).
That notation turned out to be genuinely ambiguous to parse back in several real cases —
jj does not escape a literal `{`/`}` a filename itself contains, so no amount of smarter
brace-matching (bracket-depth tracking, then boundary-adjacency to a path separator)
fully closed the gap; each fix in turn was defeated by a new real example, including one
found in this very repository (a `.github/workflow/` → `.github/workflows/` rename).
The template-based format sidesteps that whole class of problem rather than continuing
to out-clever it: `SEP` can never appear in a path, so recovering `<status>`/`<old>`/
`<new>` from a line is a plain three-way split, with no ambiguity to reason about at
all.

### Path normalization

Before comparison (`vcs/pathnorm.rs`), `ccm` lexically normalizes both the enumerated
paths and the `--include`/`--exclude` paths the same way: each is split into
`Path::components()`, any `CurDir` (`.`) component is dropped (a trailing separator,
e.g. `src/`, produces no trailing empty component either, so it normalizes the same as
`src`), and the remaining components are rejoined with a single `/`. This is purely
lexical — no filesystem access, no symlink resolution, and `..` components are left
as-is rather than collapsed — so it normalizes spelling, not identity.

### Matching is purely against the enumeration

`ccm` never checks the filesystem to decide whether an `--include`/`--exclude` entry "is
a directory" — there would be nothing to check if the directory itself, and everything
under it, was deleted — so the component-wise match against the enumerated paths (see
`prd.md` "Diff scope resolution") is the entire mechanism; a bare file entry and a
directory entry are resolved by the exact same rule, not two separate code paths
(`vcs/scope.rs`).

### Known limitation: enumeration vs. diff race

The enumeration call and the final `jj diff <files...>` invocation are two separate
subprocess calls, so the working copy can change between them — e.g. a file included
because it appeared in the enumeration could be reverted, or a new file could appear,
before the diff itself actually runs. `ccm` does not re-enumerate or otherwise guard
against this. This is the same class of time-of-check-to-time-of-use exposure already
accepted for the (larger) gap between message generation/editing and the final commit
(see `prd.md` "Default behavior"), not a new risk specific to `--include`/`--exclude`.

## Interactive file selection: why "select everything" is not "decline"

Marking every candidate in the interactive file picker is deliberately *not* treated the
same as declining the prompt (an unrestricted `Selection::All`), even though the file set
is the same at the moment of marking: `Selection::All` re-resolves to whatever the working
copy *currently* contains at each of the (separate, later) `jj diff`/`jj commit`
invocations, whereas the marked list is fixed at the moment the picker closes. Since an
interactive picker can leave the working copy open for arbitrarily long, a file that
starts changing after enumeration — one the user was never shown and never chose — would
otherwise be silently swept into the diff and the commit; pinning the exact enumerated
paths instead means such a file is simply never included, no matter how long the run
takes to finish. The cost is that "select everything" still triggers `jj split` rather
than `jj describe` even though nothing was actually excluded — accepted as the price of
not silently including an unseen file.

## Editor temp file and `$EDITOR` invocation

The `$EDITOR` temp file is created with
`tempfile::Builder::new().prefix("CCM_EDITMSG_").tempfile_in(std::env::temp_dir())` (see
"Preferences of Dependencies" item 6), which gives a collision-proof randomized suffix.
The name deliberately carries no timestamp: the file's own filesystem metadata
(creation/modification time, e.g. via `stat` or `ls -t`) already carries that.

`$EDITOR`'s value is word-split with `shell_words::split()` (item 5), the same class of
splitting `git` applies to `GIT_EDITOR`/`core.editor`.

`$EDITOR` (and `fzf`) are *not* run through `vcs/exec.rs`: they need the inherited
controlling terminal rather than captured stdio, and they deliberately avoid that
module's `process_group(0)` isolation.

## Subprocess argv safety

`ccm` never builds a shell command line or passes a git/jj invocation it runs itself
through `sh -c` (the one exception is `fzf`'s `--preview` command, which `fzf` runs — see
"Preferences of Dependencies" item 8).
The argument templates in `prd.md` ("git commit", "jj commit commands") read like shell
strings for readability only; each is a fixed argv list (built in `vcs/argv.rs`), and
`{{files}}`/`{{message}}` are substituted as literal `Command::arg()` entries — one argv
slot per file, and the full (possibly multi-line) commit message as a single argv slot
for `{{message}}`. This is what makes an unescaped multi-line message or an
`--include`d path containing spaces or shell metacharacters safe to pass through as-is —
and why neither `git commit` nor `jj commit` needs `-F <path>` instead of `-m`.

A literal `--` argument precedes `{{files}}` in the `jj commit`/`jj split` templates so
that a resolved path beginning with `-` (an unusual but valid filename) is still parsed
by jj as a positional fileset argument rather than mistaken for an unrecognized flag —
`--` marks the end of flag parsing the same way it does for any other clap-based CLI.
This is harmless even when `{{files}}` expands to zero arguments: a trailing `--` with
nothing after it is a no-op, so `jj commit`/`jj split` still act on the whole working
copy exactly as when `--include`/`--exclude` weren't given.

Progress lines render these argv lists back into a display string with `shell_words`
purely for logging (`vcs::argv::render_command`).

## `agent_cli` timeout enforcement

On expiry of an `agent_cli` entry's `timeout`, `ccm` sends `SIGTERM`, waits a 2-second
grace period for the process to exit on its own, then sends `SIGKILL` if it's still
running (`vcs/exec.rs`). Either way, the failure (exit code 10) is reported once the
process has actually been reaped, not while the grace period is still running.

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

3. The jj commit-command picker (see `prd.md` "jj commit commands") never offers more than two
    choices at a time — `jj commit` plus either `jj describe` or `jj split`, never both
    and never neither — so it doesn't warrant a fuzzy-finder crate like
    https://crates.io/crates/skim. It's implemented as a plain single-keypress stdin
    prompt (see `prd.md` "jj commit commands"), like the message review prompt, using only the
    standard library. This reasoning is specific to prompts with a small, fixed choice
    count: the jj commit-command picker here (never more than two) and the message review
    prompt (item 7, three actions). It deliberately does *not* extend to the tool picker
    (see `prd.md` "Selection"), whose candidate list is `api.yaml` itself and is therefore
    open-ended — see item 8.

4. Use the `clap` crate (https://crates.io/crates/clap), with the `derive` feature, for
    parsing `ccm`'s own command-line arguments (see `prd.md` CLI Interface).

5. Use the `shell-words` crate (https://crates.io/crates/shell-words), specifically
    `shell_words::split()`, to word-split `$EDITOR`'s value (see `prd.md` "Default behavior" under
    CLI Interface). It's a small, well-established implementation of POSIX-ish word
    splitting with quote and backslash-escape handling, avoiding a hand-rolled parser for
    something `ccm` doesn't otherwise need to reimplement.

6. Use the `tempfile` crate (https://crates.io/crates/tempfile) to create the `$EDITOR`
    temp file (see `prd.md` "Default behavior" under CLI Interface):
    `tempfile::Builder::new().prefix("CCM_EDITMSG_").tempfile_in(std::env::temp_dir())`.
    This gets `ccm` a collision-proof unique filename for free — `tempfile` appends its
    own randomized suffix after the `CCM_EDITMSG_` prefix — without `ccm` having to hand-roll
    uniqueness itself (e.g. via a timestamp or PID, both of which can collide across
    near-simultaneous runs). Since the temp file must survive process exit unconditionally
    (see `prd.md` "Default behavior"), `ccm` calls `.keep()` — or, if only the path is needed and
    not an open `File` handle, `.into_temp_path().keep()` — immediately after creating
    it. Either disarms `tempfile`'s delete-on-drop at the file's own already-generated
    path, converting it from a delete-on-drop `NamedTempFile` into a plain, ordinary
    file at that same path; `.persist(path)` isn't the right call here since that's for
    moving the file to a *different* path than the one `tempfile` picked, which `ccm`
    has no need to do. Every subsequent read/write against the file (writing the
    initial pre-populated content, `$EDITOR` editing it, `ccm` reading it back) happens
    through that same path like any other file, with no further involvement from
    `tempfile`.

7. The message review prompt and the jj commit-command picker (see `prd.md` "Message review
    prompt", "jj commit commands") each read a single keypress on a real terminal, which
    needs raw terminal mode (clearing `ICANON`/`ECHO` for the duration of that one read)
    — both go through the same helper. Rather than a terminal-UI crate like
    https://crates.io/crates/crossterm or https://crates.io/crates/console, `ccm` uses
    the `term` feature of `nix` (https://crates.io/crates/nix, already a dependency —
    see item 3's reasoning, which applies equally to these prompts' small fixed choice
    counts; the open-ended tool picker is the one deliberate exception, item 8) to call
    `tcgetattr`/`tcsetattr` directly. When stdin isn't a real terminal, the prompt falls
    back to reading a whole line instead, so this dependency is only ever exercised on a
    genuine interactive run.

8. The tool picker (see `prd.md` "Selection") and the interactive file picker (see `prd.md` "Interactive
    file selection") are the two deliberate exceptions to items 3 and 7's
    reasoning against a fuzzy-finder/TUI crate: their candidate lists — `api.yaml`,
    and the jj working copy's changed files — have no bounded size, so fuzzy search is a
    genuine usability win once either grows large — unlike the jj commit-command picker or the message
    review prompt, whose choice counts are small and fixed. Rather than embedding a
    fuzzy-finder *library* (rejected in item 3 for the jj picker, and a poor fit here too
    — evaluated and declined: https://crates.io/crates/skim pulls in an unconditional,
    whole-process `#[global_allocator]` override and a native-code build step just to
    link against it, regardless of which of its own Cargo features are enabled), `ccm`
    shells out to the `fzf` command-line tool (https://github.com/junegunn/fzf) as an
    optional subprocess, the same way it already shells out to `git`/`jj` themselves
    (item 2) rather than embedding a library for those either. Concretely:
    - For the tool picker: `ccm` writes the same candidate list `--list-tools` prints
      to `fzf`'s stdin and reads the selected line back from its stdout — the standard
      `command | fzf` idiom, just invoked directly rather than through a shell pipe.
      For the file picker: `ccm` writes one `<status>  <path>` line per changed file
      (`--multi`, so any number can be marked) and additionally sets `--preview=jj
      --no-pager diff --color=always -- {2..}` so the highlighted file's own diff
      renders live in a side pane — `{2..}` is `fzf`'s own placeholder for "every field
      from the highlighted line from the 2nd column onward" (the leading status
      character is column 1), substituted and shell-quoted by `fzf` itself before
      being handed to `$SHELL -c`; this is the one place `ccm` puts a value into a
      shell command line rather than a `Command::arg()`, unavoidable since `--preview`
      is `fzf`'s only mechanism for it, but the command text is a fixed literal `ccm`
      controls and the substituted value is always one of the paths `ccm` already
      validated by enumerating the working copy, never arbitrary input. Either way,
      `fzf`'s own interactive rendering and keyboard input go through `/dev/tty`
      directly, independent of how `ccm` has wired its stdin/stdout/stderr.
    - Both are only attempted when stdin is a real terminal (see `prd.md` "Interactive terminal
      requirement") and `CCM_FUZZY` isn't set to `0` (an escape hatch for a terminal
      that mishandles `fzf`'s inline TUI — see `prd.md` "Selection"); if `fzf` isn't found on
      `$PATH`, doesn't have a controlling terminal available to it, or fails for any
      other reason, `ccm` notes why on stderr and falls back to the numbered stdin
      prompt each picker has always had — behavior is never worse than without
      `fzf` installed.
    - This adds no new dependency to `Cargo.toml` at all: `ccm` builds and runs
      identically whether or not `fzf` happens to be on the user's machine, exactly
      like its optional `nvim`/`vim`/`vi` `$EDITOR` fallback chain (see `prd.md` "Default
      behavior") already works whether or not any of those happen to be installed.
    - It's scoped to these two prompts. The jj commit-command picker and the message
      review prompt are unaffected and stay exactly as items 3 and 7 describe.
