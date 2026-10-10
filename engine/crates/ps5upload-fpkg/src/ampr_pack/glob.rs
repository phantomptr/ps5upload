//! Shell-style patterns with Python `fnmatch.fnmatchcase` semantics, which drakmor's packer
//! uses for its rules: `*` matches any run of characters *including* `/`, `?` any one
//! character, `[seq]` / `[!seq]` a set; matching is case-sensitive and covers the whole path.
//! So `**/*.sprx` needs a `/` before the name, and `*/**` is "anything inside a folder".

/// Does `path` (relative to `/app0`) match any of `patterns`? Leading `/` and `\` are
/// normalised away on both sides, as drakmor's `glob_matches` does.
pub fn matches_any<S: AsRef<str>>(path: &str, patterns: &[S]) -> bool {
    let path = path.replace('\\', "/");
    let path: Vec<char> = path.trim_start_matches('/').chars().collect();
    patterns.iter().any(|p| {
        let p = p.as_ref().replace('\\', "/");
        let p: Vec<char> = p.trim_start_matches('/').chars().collect();
        fnmatch(&p, &path)
    })
}

fn fnmatch(p: &[char], s: &[char]) -> bool {
    // Iterative matcher with single-star backtracking: a later star supersedes an earlier one,
    // which is exact for `*` = "any run of characters".
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, si));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                '[' => {
                    if let Some((hit, next)) = set(p, pi, s[si]) {
                        if hit {
                            pi = next;
                            si += 1;
                            continue;
                        }
                    } else if s[si] == '[' {
                        // An unterminated `[` is a literal.
                        pi += 1;
                        si += 1;
                        continue;
                    }
                }
                c if c == s[si] => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, ss)) => {
                pi = sp + 1;
                si = ss + 1;
                star = Some((sp, ss + 1));
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// The set starting at `p[at] == '['`: whether `c` is in it and where the pattern resumes.
/// `None` when the set is unterminated (so `[` is a literal).
fn set(p: &[char], at: usize, c: char) -> Option<(bool, usize)> {
    let mut i = at + 1;
    let negate = p.get(i) == Some(&'!');
    if negate {
        i += 1;
    }
    // A `]` right after `[` or `[!` is a member, not the end.
    let start = i;
    let mut end = i;
    if p.get(end) == Some(&']') {
        end += 1;
    }
    while end < p.len() && p[end] != ']' {
        end += 1;
    }
    if end >= p.len() {
        return None;
    }
    let members = &p[start..end];
    let mut hit = false;
    let mut k = 0;
    while k < members.len() {
        if k + 2 < members.len() && members[k + 1] == '-' {
            if members[k] <= c && c <= members[k + 2] {
                hit = true;
            }
            k += 3;
        } else {
            if members[k] == c {
                hit = true;
            }
            k += 1;
        }
    }
    Some((hit != negate, end + 1))
}

#[cfg(test)]
mod tests {
    use super::matches_any;

    fn m(path: &str, pattern: &str) -> bool {
        matches_any(path, &[pattern])
    }

    /// Cases checked against CPython's `fnmatch.fnmatchcase`.
    #[test]
    fn it_matches_like_python_fnmatchcase() {
        assert!(m("a/b/c.sprx", "**/*.sprx"));
        assert!(!m("c.sprx", "**/*.sprx"));
        assert!(m("c.sprx", "*.sprx"));
        assert!(m("a/b/c.sprx", "*.sprx"), "* crosses /");
        assert!(m("d/x", "*/**"));
        assert!(!m("toc", "*/**"));
        assert!(m("assets/x/y.bin", "assets/**"));
        assert!(!m("assetsx", "assets/**"));
        assert!(m("/data/a.bin", "data/*"));
        assert!(m("data\\a.bin", "/data/*"));
        assert!(!m("Data/a.bin", "data/*"), "case-sensitive");
        assert!(m("d/soundbank.fr", "d/soundbank.*"));
        assert!(!m("d/soundbank", "d/soundbank.*"));
        assert!(m("file.001", "file.00?"));
        assert!(m("a1", "a[0-9]"));
        assert!(!m("ab", "a[0-9]"));
        assert!(m("ab", "a[!0-9]"));
        assert!(m("a]", "a[]]"));
        assert!(m("a[", "a["), "an unterminated set is literal");
        assert!(
            m("a-", "a[a-]") && !m("b", "a[a-]"),
            "a trailing - is a member"
        );
        assert!(m("", "*"));
        assert!(m("abc", "a*b*c"));
        assert!(m("aXbYbc", "a*b*c"));
        assert!(!m("abcd", "a*b*c"));
        assert!(m("ampr_assets-007.pak", "ampr_assets-*.pak"));
    }
}
