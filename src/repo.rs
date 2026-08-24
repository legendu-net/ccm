//! Repository detection (prd.md "Repository detection").
//!
//! `ccm` determines the repository type by walking the directory tree itself, rather
//! than shelling out to `git`/`jj` — requiring the *other* tool's binary just to figure
//! out which one applies would be backwards. [`locate_roots`] is the only I/O in this
//! module (canonicalizing `cwd` and checking marker existence); [`classify`] and
//! [`apply_git_flag`] are pure functions over the resulting [`RepoRoots`], so the full
//! detection truth table (see prd.md's table under "Repository detection") is
//! unit-testable without touching a filesystem at all.

use crate::error::{NotARepo, UsageError};
use std::io;
use std::path::{Path, PathBuf};

/// The nearest ancestor (if any) containing a `.git` entry, and separately the nearest
/// ancestor (if any) containing a `.jj` entry — tracked independently, since a single
/// directory can supply both, one, or neither.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoRoots {
    pub git: Option<PathBuf>,
    pub jj: Option<PathBuf>,
}

/// Walks upward from `start` (canonicalized once) through its ancestors toward the
/// filesystem root, looking for a `.git` entry and a `.jj` entry directly inside each
/// directory visited. Stops once both have been found, or once the walk reaches the
/// filesystem root having found only one or neither.
///
/// Only checks that *something* named `.git`/`.jj` exists at each level (via
/// `symlink_metadata`, so even a broken symlink counts) — not that it's specifically a
/// directory, so worktrees, submodules (a `.git` *file* with a `gitdir:` pointer), and
/// ordinary repositories are all detected the same way. A bare repository (no `.git`
/// subdirectory of its own) is therefore never detected as a git repository by this
/// walk — matching `git rev-parse --show-toplevel`, which also fails inside one.
///
/// # Errors
/// Propagates a failure to canonicalize `start` (e.g. the working directory has been
/// deleted, or a permissions error).
pub fn locate_roots(start: &Path) -> io::Result<RepoRoots> {
    let start = start.canonicalize()?;
    let mut roots = RepoRoots::default();
    for dir in start.ancestors() {
        if roots.git.is_none() && marker_exists(dir, ".git") {
            roots.git = Some(dir.to_path_buf());
        }
        if roots.jj.is_none() && marker_exists(dir, ".jj") {
            roots.jj = Some(dir.to_path_buf());
        }
        if roots.git.is_some() && roots.jj.is_some() {
            break;
        }
    }
    Ok(roots)
}

fn marker_exists(dir: &Path, name: &str) -> bool {
    std::fs::symlink_metadata(dir.join(name)).is_ok()
}

/// How `ccm` has determined the current directory should be treated, before `--git` is
/// applied. Colocated (both a git and a jj root were found, at the same directory) is
/// the interesting case: everything else already has a single unambiguous handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoContext {
    /// A git root and a jj root were found at the same directory.
    Colocated { root: PathBuf },
    /// Plain git handling applies outright (no jj root, or a jj root that is a
    /// shallower, unrelated outer repository).
    Git { root: PathBuf },
    /// Plain jj handling applies outright (no git root, or a git root that is a
    /// shallower, unrelated outer repository).
    Jj { root: PathBuf },
}

/// Stage 2 of the check-order pipeline: classifies [`RepoRoots`] into a [`RepoContext`],
/// implementing the detection table from prd.md's "Repository detection". Pure — no
/// filesystem access.
///
/// # Errors
/// Returns [`NotARepo`] (exit 4) if neither a git nor a jj root was found.
pub fn classify(roots: &RepoRoots) -> Result<RepoContext, NotARepo> {
    match (&roots.git, &roots.jj) {
        (None, None) => Err(NotARepo),
        (Some(git), None) => Ok(RepoContext::Git { root: git.clone() }),
        (None, Some(jj)) => Ok(RepoContext::Jj { root: jj.clone() }),
        (Some(git), Some(jj)) if git == jj => Ok(RepoContext::Colocated { root: git.clone() }),
        (Some(git), Some(jj)) => {
            // Both were found by walking up from the same starting directory, so one is
            // necessarily an ancestor of the other; the deeper (more nested — more path
            // components) root is the real repository for this directory, and the
            // shallower one is an unrelated outer repository that happens to contain it.
            if depth(git) > depth(jj) {
                Ok(RepoContext::Git { root: git.clone() })
            } else {
                Ok(RepoContext::Jj { root: jj.clone() })
            }
        }
    }
}

fn depth(path: &Path) -> usize {
    path.components().count()
}

/// Where `ccm` will actually look for changes and commit to, once `--git` has been
/// applied to a [`RepoContext`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoHandling {
    Git { root: PathBuf },
    Jj { root: PathBuf },
}

/// Stage 3 of the check-order pipeline (the `--git`-specific part): applies `--git` to
/// a [`RepoContext`], per prd.md's `--git` flag description. Pure.
///
/// # Errors
/// Returns [`UsageError::GitFlagOutsideGitRepo`] (exit 2) if `--git` was passed but the
/// current directory isn't *treated as* a git repository at all — including the case
/// where a git root technically exists somewhere in the ancestry, but a deeper jj root
/// wins.
pub fn apply_git_flag(ctx: RepoContext, force_git: bool) -> Result<RepoHandling, UsageError> {
    match ctx {
        RepoContext::Colocated { root } => Ok(if force_git {
            RepoHandling::Git { root }
        } else {
            RepoHandling::Jj { root }
        }),
        // `--git` is a harmless no-op on a plain git repository either way.
        RepoContext::Git { root } => Ok(RepoHandling::Git { root }),
        RepoContext::Jj { root } => {
            if force_git {
                Err(UsageError::GitFlagOutsideGitRepo)
            } else {
                Ok(RepoHandling::Jj { root })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn roots(git: Option<&str>, jj: Option<&str>) -> RepoRoots {
        RepoRoots {
            git: git.map(PathBuf::from),
            jj: jj.map(PathBuf::from),
        }
    }

    // ---- classify(): the detection truth table from prd.md ----

    #[test]
    fn neither_found_is_not_a_repo() {
        assert!(matches!(classify(&roots(None, None)), Err(NotARepo)));
    }

    #[test]
    fn only_git_found_is_a_git_repo() {
        assert_eq!(
            classify(&roots(Some("/a/b"), None)).unwrap(),
            RepoContext::Git {
                root: "/a/b".into()
            }
        );
    }

    #[test]
    fn only_jj_found_is_a_jj_repo() {
        assert_eq!(
            classify(&roots(None, Some("/a/b"))).unwrap(),
            RepoContext::Jj {
                root: "/a/b".into()
            }
        );
    }

    #[test]
    fn equal_roots_is_colocated() {
        assert_eq!(
            classify(&roots(Some("/a/b"), Some("/a/b"))).unwrap(),
            RepoContext::Colocated {
                root: "/a/b".into()
            }
        );
    }

    #[test]
    fn deeper_git_root_nested_in_shallower_jj_root_is_a_git_repo() {
        // e.g. an independent git checkout nested inside a larger jj-managed tree.
        assert_eq!(
            classify(&roots(Some("/a/b/c"), Some("/a"))).unwrap(),
            RepoContext::Git {
                root: "/a/b/c".into()
            }
        );
    }

    #[test]
    fn deeper_jj_root_nested_in_shallower_git_root_is_a_jj_repo() {
        // e.g. a jj project nested inside a git-tracked dotfiles directory.
        assert_eq!(
            classify(&roots(Some("/a"), Some("/a/b/c"))).unwrap(),
            RepoContext::Jj {
                root: "/a/b/c".into()
            }
        );
    }

    // ---- apply_git_flag(): --git semantics per context ----

    #[test]
    fn colocated_defaults_to_jj() {
        let ctx = RepoContext::Colocated { root: "/a".into() };
        assert_eq!(
            apply_git_flag(ctx, false).unwrap(),
            RepoHandling::Jj { root: "/a".into() }
        );
    }

    #[test]
    fn colocated_with_git_flag_uses_git() {
        let ctx = RepoContext::Colocated { root: "/a".into() };
        assert_eq!(
            apply_git_flag(ctx, true).unwrap(),
            RepoHandling::Git { root: "/a".into() }
        );
    }

    #[test]
    fn plain_git_repo_git_flag_is_harmless_noop() {
        let ctx = RepoContext::Git { root: "/a".into() };
        assert_eq!(
            apply_git_flag(ctx.clone(), false).unwrap(),
            RepoHandling::Git { root: "/a".into() }
        );
        assert_eq!(
            apply_git_flag(ctx, true).unwrap(),
            RepoHandling::Git { root: "/a".into() }
        );
    }

    #[test]
    fn plain_jj_repo_without_git_flag_uses_jj() {
        let ctx = RepoContext::Jj { root: "/a".into() };
        assert_eq!(
            apply_git_flag(ctx, false).unwrap(),
            RepoHandling::Jj { root: "/a".into() }
        );
    }

    #[test]
    fn plain_jj_repo_with_git_flag_is_a_usage_error() {
        let ctx = RepoContext::Jj { root: "/a".into() };
        assert!(matches!(
            apply_git_flag(ctx, true),
            Err(UsageError::GitFlagOutsideGitRepo)
        ));
    }

    // ---- locate_roots(): real filesystem walk ----

    #[test]
    fn locate_roots_finds_a_plain_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let sub = repo.join("a/b");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();

        let found = locate_roots(&sub).unwrap();
        assert_eq!(found.git, Some(repo.canonicalize().unwrap()));
        assert_eq!(found.jj, None);
    }

    #[test]
    fn locate_roots_treats_a_git_file_pointer_as_a_marker_too() {
        // A linked worktree/submodule: `.git` is a file containing `gitdir: <path>`.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();

        let found = locate_roots(&repo).unwrap();
        assert_eq!(found.git, Some(repo.canonicalize().unwrap()));
    }

    #[test]
    fn locate_roots_finds_the_nearest_marker_not_a_farther_one() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        let sub = inner.join("a/b");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir(outer.join(".jj")).unwrap();
        fs::create_dir(inner.join(".jj")).unwrap();

        let found = locate_roots(&sub).unwrap();
        assert_eq!(found.jj, Some(inner.canonicalize().unwrap()));
    }

    #[test]
    fn locate_roots_finds_nothing_outside_any_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plain");
        fs::create_dir_all(&dir).unwrap();

        let found = locate_roots(&dir).unwrap();
        assert_eq!(found, RepoRoots::default());
    }

    #[test]
    fn locate_roots_does_not_detect_a_bare_repository() {
        // A bare repo holds HEAD/objects/... directly, with no .git subdirectory.
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("bare.git");
        fs::create_dir_all(bare.join("objects")).unwrap();
        fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let found = locate_roots(&bare).unwrap();
        assert_eq!(found, RepoRoots::default());
    }
}
