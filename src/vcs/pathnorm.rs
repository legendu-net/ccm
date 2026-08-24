//! Lexical path normalization and component-wise matching, shared by both sides of
//! `--include`/`--exclude` resolution (prd.md, "Diff scope resolution").
//!
//! Purely lexical: no filesystem access, no symlink resolution, and `..` components are
//! left as-is rather than collapsed — this normalizes *spelling*, not identity. A path
//! that only resolves to the same file after resolving a symlink or a `..` segment is
//! still compared literally and can still fail to match.

use std::path::{Component, Path};

/// A path that has been split into `Path::components()`, had any `CurDir` (`.`)
/// component dropped, and been rejoined with a single `/` — so `./src/main.rs` and
/// `src//main.rs` both normalize to `src/main.rs`, and a trailing separator (`src/`)
/// produces no trailing empty component.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedPath(String);

impl NormalizedPath {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NormalizedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Normalizes `path` per the rules above.
///
/// Built by hand rather than via `.collect::<Vec<_>>().join("/")`: `RootDir`'s own
/// `as_os_str()` is already `"/"`, so naively joining every component with an extra
/// `"/"` separator would double up the leading slash of an absolute path (`"//home/..."`
/// instead of `"/home/..."`).
#[must_use]
pub fn normalize(path: &Path) -> NormalizedPath {
    let mut result = String::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::RootDir => result.push('/'),
            other => {
                if !result.is_empty() && !result.ends_with('/') {
                    result.push('/');
                }
                result.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }
    NormalizedPath(result)
}

/// Convenience wrapper for `normalize(Path::new(s))`.
#[must_use]
pub fn normalize_str(s: &str) -> NormalizedPath {
    normalize(Path::new(s))
}

/// Component-wise (not raw string-prefix) match: normalized `entry` matches normalized
/// `candidate` if `candidate` equals `entry` exactly, or if `candidate`'s components
/// begin with all of `entry`'s components followed by at least one more — so `entry`
/// "src" matches "src/main.rs" and "src/sub/mod.rs" but not "src-old/main.rs" (plain
/// string prefixing would wrongly match that last one). `entry` doubles as both a bare
/// file pattern and a directory-prefix pattern; there is no separate mechanism for
/// "is this a directory" (the caller never checks the filesystem for that either).
#[must_use]
pub fn matches(entry: &NormalizedPath, candidate: &NormalizedPath) -> bool {
    // A bare "." normalizes to zero components (an empty string, per `normalize`'s own
    // CurDir-dropping rule) — applying the "entry's components followed by at least
    // one more" rule literally to zero components means it's a prefix of every
    // non-empty candidate, i.e. `--include .` means "everything", matching the
    // intuitive git/shell convention rather than failing to match anything.
    if entry.0.is_empty() {
        return !candidate.0.is_empty();
    }
    if entry.0 == candidate.0 {
        return true;
    }
    candidate
        .0
        .strip_prefix(&entry.0)
        .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_leading_curdir_component() {
        assert_eq!(normalize_str("./src/main.rs").as_str(), "src/main.rs");
    }

    #[test]
    fn an_absolute_path_keeps_a_single_leading_slash() {
        // Regression test: RootDir's own as_os_str() is already "/", so a naive
        // join("/") over all components would double it up to "//home/...".
        assert_eq!(
            normalize_str("/home/user/repo/src/main.rs").as_str(),
            "/home/user/repo/src/main.rs"
        );
    }

    #[test]
    fn bare_root_normalizes_to_a_single_slash() {
        assert_eq!(normalize_str("/").as_str(), "/");
    }

    #[test]
    fn a_bare_curdir_pattern_matches_every_non_empty_candidate() {
        // Regression test: `ccm --include .` is natural git/shell muscle-memory for
        // "everything under here" and must not silently match nothing.
        let entry = normalize_str(".");
        assert!(matches(&entry, &normalize_str("src/main.rs")));
        assert!(matches(&entry, &normalize_str("a.txt")));
    }

    #[test]
    fn collapses_doubled_separators() {
        assert_eq!(normalize_str("src//main.rs").as_str(), "src/main.rs");
    }

    #[test]
    fn drops_trailing_separator() {
        assert_eq!(normalize_str("src/").as_str(), "src");
    }

    #[test]
    fn leaves_parent_dir_components_as_is() {
        assert_eq!(
            normalize_str("../repo/src/main.rs").as_str(),
            "../repo/src/main.rs"
        );
    }

    #[test]
    fn plain_bare_name_is_unchanged() {
        assert_eq!(normalize_str("src").as_str(), "src");
    }

    #[test]
    fn matches_exact_file() {
        let entry = normalize_str("src/main.rs");
        assert!(matches(&entry, &normalize_str("src/main.rs")));
    }

    #[test]
    fn directory_entry_matches_direct_child() {
        let entry = normalize_str("src");
        assert!(matches(&entry, &normalize_str("src/main.rs")));
    }

    #[test]
    fn directory_entry_matches_nested_grandchild() {
        let entry = normalize_str("src");
        assert!(matches(&entry, &normalize_str("src/sub/mod.rs")));
    }

    #[test]
    fn directory_entry_does_not_match_a_sibling_with_shared_string_prefix() {
        // The string-prefix trap: "src-old/main.rs" starts with the raw string "src"
        // but is not nested under the "src" directory.
        let entry = normalize_str("src");
        assert!(!matches(&entry, &normalize_str("src-old/main.rs")));
    }

    #[test]
    fn a_file_entry_does_not_match_its_own_parent_directory() {
        let entry = normalize_str("src/main.rs");
        assert!(!matches(&entry, &normalize_str("src")));
    }

    #[test]
    fn unrelated_paths_do_not_match() {
        let entry = normalize_str("src/main.rs");
        assert!(!matches(&entry, &normalize_str("lib/main.rs")));
    }
}
