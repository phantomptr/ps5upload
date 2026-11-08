//! Console file names this computer cannot hold, kept losslessly (#729).
//!
//! A PS5 file name may hold any byte but '/' and NUL: Red Dead Redemption 2 keeps a file named
//! `options\gabindings.xml` inside its save. On Windows a '\' is a separator (`a\..\b` climbs out
//! of the folder), a ':' a drive or a stream, `CON` a device, and a trailing '.' or space is
//! dropped. A download refuses such names (`host_unsafe_component`) so nothing lands where it
//! was not asked to; the saves code instead keeps them here under a reversible name and gives
//! the console back the exact one.
//!
//! The mapping is the one Samba (`catia`/`fruit:encoding = private`), Cygwin and Services for
//! Macintosh use: a character Windows refuses becomes U+F000 plus its code (`\` is U+F05C, `:`
//! U+F03A). A trailing '.' or space, and the first letter of a reserved device name, are mapped
//! the same way. Names that need none of it stay as they are, so every backup made before this
//! reads the same. A console name that already holds one of those private-use characters could
//! not be told from a mapped one: it is refused rather than changed.

use crate::transfer::host_unsafe_component;

/// The private-use block a mapped character lands in: U+F000 + its ASCII code.
const BASE: u32 = 0xF000;
/// What Windows refuses in a name besides the control characters.
const REFUSED: &str = "\"*:<>?\\|";

fn mapped(c: char) -> char {
    char::from_u32(BASE + c as u32).expect("an ASCII code plus U+F000 is a character")
}

/// The ASCII character `c` stands for, when it is a mapped one.
fn unmapped(c: char) -> Option<char> {
    let n = c as u32;
    (BASE + 1..=BASE + 0x7F)
        .contains(&n)
        .then(|| char::from_u32(n - BASE))
        .flatten()
}

fn needs_mapping(c: char) -> bool {
    c.is_ascii() && ((c as u32) < 0x20 || REFUSED.contains(c))
}

/// The name a console file `name` (one path component) is kept under on this computer: the same
/// unless it holds something a Windows, macOS or Linux folder can't. Err for a name that already
/// holds a mapped character (it would come back as another name).
pub fn escape_name(name: &str) -> Result<String, String> {
    if name.chars().any(|c| unmapped(c).is_some()) {
        return Err(format!(
            "{name:?} holds a character this app uses to keep names it can't save as they are"
        ));
    }
    let mut out: Vec<char> = name
        .chars()
        .map(|c| if needs_mapping(c) { mapped(c) } else { c })
        .collect();
    if let Some(last) = out.last_mut() {
        if *last == '.' || *last == ' ' {
            *last = mapped(*last);
        }
    }
    let mut s: String = out.iter().collect();
    // What is left is a reserved device name (`CON`, `aux.txt`, `COM1`): its first letter.
    if host_unsafe_component(&s).is_some() {
        let mut chars = s.chars();
        if let Some(first) = chars.next() {
            s = std::iter::once(mapped(first)).chain(chars).collect();
        }
    }
    debug_assert!(host_unsafe_component(&s).is_none(), "{s:?}");
    Ok(s)
}

/// The console name of a file kept as `kept` by [`escape_name`]. Err for a name `escape_name`
/// would not have made (so two kept names never stand for one console name), and for one that
/// would climb out of its folder (`..`, a '/').
pub fn unescape_name(kept: &str) -> Result<String, String> {
    let name: String = kept.chars().map(|c| unmapped(c).unwrap_or(c)).collect();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(format!("{kept:?} does not name a file"));
    }
    if escape_name(&name).as_deref() != Ok(kept) {
        return Err(format!("{kept:?} is not a name this app kept"));
    }
    Ok(name)
}

/// [`escape_name`] on every component of a '/'-separated relative path.
pub fn escape_path(path: &str) -> Result<String, String> {
    let parts: Result<Vec<String>, String> = path.split('/').map(escape_name).collect();
    Ok(parts?.join("/"))
}

/// [`unescape_name`] on every component of a '/'-separated relative path.
pub fn unescape_path(path: &str) -> Result<String, String> {
    let parts: Result<Vec<String>, String> = path.split('/').map(unescape_name).collect();
    Ok(parts?.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_windows_refuses_is_kept_and_comes_back_exactly() {
        for name in [
            "options\\gabindings.xml",
            "a:b",
            "C:evil",
            "what?*<>|\"",
            "tab\there",
            "trailing.",
            "trailing ",
            "x..",
            "CON",
            "con.txt",
            "Aux",
            "COM1.dat",
            "LPT9",
            "CONIN$",
            "..\\..\\x",
            "a\\..\\..\\b",
            "100%.txt",
            "%5C",
        ] {
            let kept = escape_name(name).unwrap();
            assert!(
                host_unsafe_component(&kept).is_none(),
                "{name:?} -> {kept:?}"
            );
            assert!(!kept.contains(['\\', ':', '/']), "{kept:?}");
            assert_eq!(unescape_name(&kept).unwrap(), name, "{kept:?}");
        }
    }

    #[test]
    fn ordinary_names_are_left_alone() {
        for name in [
            "sdimg_SAVEDATA00",
            "RDR2SAVE1.SAV",
            "100%.txt",
            "%5C",
            "über save.dat",
            ".hidden",
            "CONSOLE",
            "COM10",
            "con1",
        ] {
            assert_eq!(escape_name(name).unwrap(), name);
            assert_eq!(unescape_name(name).unwrap(), name);
        }
    }

    #[test]
    fn the_backslash_is_the_one_samba_uses() {
        assert_eq!(
            escape_name("options\\gabindings.xml").unwrap(),
            "options\u{F05C}gabindings.xml"
        );
        assert_eq!(escape_name("a:b").unwrap(), "a\u{F03A}b");
        assert_eq!(escape_name("CON").unwrap(), "\u{F043}ON");
    }

    #[test]
    fn a_name_that_could_be_read_two_ways_is_refused() {
        // Already holding a mapped character: it would come back as `a\b`.
        assert!(escape_name("a\u{F05C}b\\").is_err());
        assert!(escape_name("\u{F041}").is_err());
        // Not what `escape_name` makes: `A` mapped for no reason, an ordinary dot mapped.
        assert!(unescape_name("\u{F041}").is_err());
        assert!(unescape_name("a\u{F02E}b").is_err());
    }

    #[test]
    fn a_kept_name_never_climbs_out() {
        // `..` and `.` kept (a trailing dot mapped) must not come back as the real thing,
        // and a mapped '/' or NUL is never a name.
        for kept in [".\u{F02E}", "\u{F02E}", "a\u{F02F}b", "\u{F02F}", ""] {
            assert!(unescape_name(kept).is_err(), "{kept:?}");
        }
        assert!(unescape_path("a/\u{F02E}\u{F02E}/b").is_err());
    }

    #[test]
    fn paths_map_component_by_component() {
        let kept = escape_path("options\\x/a:b/plain").unwrap();
        assert_eq!(kept, "options\u{F05C}x/a\u{F03A}b/plain");
        assert_eq!(unescape_path(&kept).unwrap(), "options\\x/a:b/plain");
    }
}
