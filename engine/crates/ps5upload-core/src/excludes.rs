//! Filename / path exclusion patterns for folder uploads.
//!
//! Without filtering, editor-backup and OS-detritus files end up
//! shipped to the PS5: macOS ships `.DS_Store` into every directory,
//! Windows ships `Thumbs.db` + `desktop.ini`, and anyone who's opened
//! the folder in a game engine editor ships thousands of `*.esbak`
//! backups.
//!
//! The 2.1 default: on. User can opt out per-upload via the Upload
//! screen's exclude-rules panel.
//!
//! Pattern grammar (kept deliberately small — no glob crate):
//!   - Literal filename:     "Thumbs.db"       matches any file whose basename is exactly "Thumbs.db"
//!   - Suffix wildcard:      "*.esbak"         matches any path ending in ".esbak"
//!   - Prefix wildcard:      "eboot.*"         matches any basename starting with "eboot."
//!   - Component literal:    ".git"            matches any path with `.git` as a directory component
//!   - Component + star:     ".git/**"         same — the `/**` suffix is accepted for readability,
//!     treated as "any descendant of a `.git` dir"
//!
//! We iterate patterns once per path so `contains(...)` with a small Vec
//! is cheap enough not to need a compiled matcher struct for typical
//! default-rules sizes (<20 entries).

use std::path::Path;

/// The defaults applied when the user hasn't customized the exclude list.
/// Keep this list in sync with `client/src/state/upload.ts`'s
/// `defaultExcludes` so host-side plan + UI preview agree.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    "*.esbak",
    ".git/**",
];

/// Returns true when `path` matches any of the supplied patterns.
///
/// `path` can be an absolute or relative path — matching is on path
/// components / basename only, so it doesn't matter which.
pub fn is_excluded(path: &Path, patterns: &[&str]) -> bool {
    // `to_string_lossy` over `.to_str()`-then-`.unwrap_or_default()` so
    // non-UTF-8 paths on Windows (CP-1252 filenames from legacy tools)
    // still pattern-match. Over-matching a mojibake basename is far
    // safer than letting a `.DS_Store` variant with one stray byte slip
    // past the default excludes into the PS5 upload.
    let basename_owned = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let components_owned: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let components: Vec<&str> = components_owned.iter().map(String::as_str).collect();
    let basename = basename_owned.as_str();

    for raw in patterns {
        let pat = strip_glob_suffix(raw);
        if matches_pattern(pat, basename, &components) {
            return true;
        }
    }
    false
}

/// Convenience: match against an owned `String` slice (what the UI sends
/// over IPC/HTTP).
pub fn is_excluded_strings(path: &Path, patterns: &[String]) -> bool {
    let refs: Vec<&str> = patterns.iter().map(String::as_str).collect();
    is_excluded(path, &refs)
}

fn strip_glob_suffix(s: &str) -> &str {
    // Accept "foo/**" as equivalent to "foo" — treating both as "any
    // descendant of a `foo` dir." Letting users write the "/**" form
    // matches gitignore intuition.
    if let Some(stripped) = s.strip_suffix("/**") {
        return stripped;
    }
    if let Some(stripped) = s.strip_suffix('/') {
        return stripped;
    }
    s
}

fn matches_pattern(pat: &str, basename: &str, components: &[&str]) -> bool {
    if pat.is_empty() {
        return false;
    }

    // Component-literal (e.g. ".git"): any path component matches the pattern
    // literally. This is the case when the pattern has no wildcard and is not
    // a typical filename ("Thumbs.db" is a valid basename and also a
    // component; it matches both ways).
    if !pat.contains('*') {
        if components.contains(&pat) {
            return true;
        }
        // Also match when basename exactly equals pattern (handles
        // "Thumbs.db" for a top-level match when we don't iterate
        // components beyond the path).
        if basename == pat {
            return true;
        }
        return false;
    }

    // Wildcard handling. We support a single `*` and anchor it on one side.
    if let Some(suffix) = pat.strip_prefix('*') {
        // "*.esbak" → suffix = ".esbak". Match against basename only.
        if !suffix.is_empty() {
            return basename.ends_with(suffix);
        }
        return true; // bare "*" — matches everything; probably a user mistake.
    }
    if let Some(prefix) = pat.strip_suffix('*') {
        if !prefix.is_empty() {
            return basename.starts_with(prefix);
        }
        return true;
    }
    // "foo*bar" not supported; fall back to literal match.
    basename == pat
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn literal_basename_match() {
        assert!(is_excluded(&p("/game/sub/.DS_Store"), DEFAULT_EXCLUDES));
        assert!(is_excluded(&p("Thumbs.db"), DEFAULT_EXCLUDES));
        assert!(is_excluded(&p("/game/Thumbs.db"), DEFAULT_EXCLUDES));
        assert!(!is_excluded(&p("/game/eboot.bin"), DEFAULT_EXCLUDES));
    }

    #[test]
    fn suffix_wildcard_match() {
        assert!(is_excluded(&p("/x/foo.esbak"), DEFAULT_EXCLUDES));
        assert!(is_excluded(&p("map_A.esbak"), DEFAULT_EXCLUDES));
        assert!(!is_excluded(&p("/x/foo.pak"), DEFAULT_EXCLUDES));
    }

    #[test]
    fn git_dir_excluded_at_any_depth() {
        assert!(is_excluded(&p("/game/.git/HEAD"), DEFAULT_EXCLUDES));
        assert!(is_excluded(
            &p("/game/deep/nested/.git/objects/aa/bb"),
            DEFAULT_EXCLUDES
        ));
        assert!(!is_excluded(
            &p("/game/deep/nested/ok.bin"),
            DEFAULT_EXCLUDES
        ));
    }

    #[test]
    fn custom_prefix_wildcard() {
        let patterns: &[&str] = &["eboot.*"];
        assert!(is_excluded(&p("/game/eboot.bin"), patterns));
        assert!(is_excluded(&p("eboot.esbak"), patterns));
        assert!(!is_excluded(&p("/game/nope.bin"), patterns));
    }

    #[test]
    fn glob_suffix_variants_equivalent() {
        // These three patterns should all exclude the same .git file.
        for pat in &[".git", ".git/**", ".git/"] {
            let patterns: &[&str] = &[pat];
            assert!(
                is_excluded(&p("/game/.git/HEAD"), patterns),
                "pattern {:?} should exclude .git/HEAD",
                pat
            );
        }
    }

    #[test]
    fn owned_strings_wrapper_works() {
        let patterns: Vec<String> = vec!["*.esbak".into(), "Thumbs.db".into()];
        assert!(is_excluded_strings(&p("/a/b/foo.esbak"), &patterns));
        assert!(is_excluded_strings(&p("Thumbs.db"), &patterns));
        assert!(!is_excluded_strings(&p("eboot.bin"), &patterns));
    }

    #[test]
    fn empty_patterns_never_match() {
        assert!(!is_excluded(&p("/anything.bin"), &[]));
    }
}
