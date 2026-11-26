//! File names from an NFS or FTP server as text, with every byte kept.
//!
//! Both protocols send a name as raw bytes. Most servers use UTF-8, but a folder copied from an
//! older system can hold Latin-1, Windows-1252 or Shift-JIS names. Read as lossy UTF-8, such a
//! name became `U+FFFD` and could not be asked for again: the listing showed a name the server
//! did not have, opening the file failed, and an NFS folder holding one could not be listed at
//! all. Here each byte that is not part of valid UTF-8 stands for itself as one character of a
//! private-use range, and turning the text back into bytes gives the exact name again.
//!
//! The range is U+10FF00..=U+10FF7F (in Supplementary Private Use Area-B), one character per
//! byte 0x80..=0xFF. A real name that holds one of those characters (valid UTF-8, four bytes
//! each) is read byte by byte the same way, so it comes back unchanged too.
//!
//! Until 12.6.30 the range was U+10FF80..=U+10FFFF. Its last two, U+10FFFE and U+10FFFF, are
//! not characters at all, and macOS refuses a file name holding one: a name with byte 0xFE or
//! 0xFF (`ю` and `я` in Windows-1251, `þ` and `ÿ` in Latin-1) could be listed and opened on the
//! server but not copied to a Mac. A name written the old way (in a saved path) still turns
//! back into the same bytes.

/// The character that stands for byte 0x80; byte `b` is `ESCAPE_BASE + (b - 0x80)`.
const ESCAPE_BASE: u32 = 0x10_FF00;
/// Where the stand-ins were before: still read back, never written.
const OLD_ESCAPE_BASE: u32 = 0x10_FF80;

fn escape(b: u8) -> char {
    // Only bytes 0x80..=0xFF are ever escaped (ASCII is always valid UTF-8).
    char::from_u32(ESCAPE_BASE + u32::from(b.saturating_sub(0x80))).unwrap_or('\u{FFFD}')
}

fn unescape(c: char) -> Option<u8> {
    let n = u32::from(c);
    [ESCAPE_BASE, OLD_ESCAPE_BASE]
        .into_iter()
        .find(|base| (*base..=*base + 0x7F).contains(&n))
        .map(|base| (n - base + 0x80) as u8)
}

fn push_text(out: &mut String, text: &str) {
    for c in text.chars() {
        if unescape(c).is_some() {
            let mut buf = [0u8; 4];
            for &b in c.encode_utf8(&mut buf).as_bytes() {
                out.push(escape(b));
            }
        } else {
            out.push(c);
        }
    }
}

/// A name the server sent, as text: valid UTF-8 as it is, every other byte as its stand-in.
pub(crate) fn decode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                push_text(&mut out, text);
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                push_text(&mut out, std::str::from_utf8(good).unwrap_or_default());
                let n = e.error_len().unwrap_or(bad.len()).max(1);
                for &b in &bad[..n] {
                    out.push(escape(b));
                }
                rest = &bad[n..];
            }
        }
    }
    out
}

/// The bytes to send for `name`: what [`decode`] read, exactly.
pub(crate) fn encode(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    for c in name.chars() {
        match unescape(c) {
            Some(b) => out.push(b),
            None => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out
}

/// Names on an FTP server's control and data connections, through [`decode`] and [`encode`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Lossless;

impl suppaftp::Encoding for Lossless {
    fn decode(&self, bytes: &[u8]) -> String {
        decode(bytes)
    }

    fn encode(&self, text: &str) -> std::io::Result<Vec<u8>> {
        Ok(encode(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_names_are_unchanged() {
        for name in [
            "game.pkg",
            "[PS5] Hellblade [RUS]",
            "café",
            "ゲーム",
            "a\u{1F600}b",
        ] {
            assert_eq!(decode(name.as_bytes()), name);
            assert_eq!(encode(name), name.as_bytes());
        }
    }

    #[test]
    fn a_latin1_name_comes_back_byte_for_byte() {
        let raw = b"caf\xE9 \xFF\x80.bin";
        let text = decode(raw);
        assert!(!text.contains('\u{FFFD}'), "{text:?}");
        assert!(text.starts_with("caf") && text.ends_with(".bin"));
        assert_eq!(encode(&text), raw);
        // Two names that differ only in a byte that is not UTF-8 stay two names.
        assert_ne!(decode(b"a\xE9"), decode(b"a\xE8"));
    }

    #[test]
    fn a_name_holding_a_stand_in_character_also_comes_back() {
        let stand_in = char::from_u32(ESCAPE_BASE + 0x69).unwrap();
        let raw = format!("x{stand_in}y");
        let text = decode(raw.as_bytes());
        assert_eq!(encode(&text), raw.as_bytes());
    }

    /// No stand-in is one of the code points that are not characters (U+..FFFE, U+..FFFF):
    /// macOS refuses a file name holding one, so a name with byte 0xFE or 0xFF could not be
    /// copied to a Mac. A name written with the old stand-ins still gives its bytes back.
    #[test]
    fn no_stand_in_is_a_noncharacter_and_old_ones_still_read() {
        for b in 0x80..=0xFFu8 {
            let text = decode(&[b]);
            let c = text.chars().next().unwrap();
            assert_eq!(text.chars().count(), 1);
            assert!(u32::from(c) & 0xFFFE != 0xFFFE, "byte {b:#x} is {c:?}");
            assert_eq!(encode(&text), [b]);
            let old = char::from_u32(OLD_ESCAPE_BASE + u32::from(b - 0x80)).unwrap();
            assert_eq!(encode(&format!("a{old}")), [b'a', b]);
        }
        // A real name holding an old stand-in (valid UTF-8) comes back as it was, too.
        let raw = "x\u{10FFE9}y".as_bytes();
        assert_eq!(encode(&decode(raw)), raw);
        // The file system here takes such a name.
        let name = decode(b"\xFF\xFE.bin");
        let path = crate::test_tmp::temp_path(format!("ps5u-name-{}-{name}", std::process::id()));
        std::fs::write(&path, b"x").expect("a name with the stand-ins can be made here");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn every_byte_string_round_trips() {
        // A small deterministic generator: every length up to 12 with mixed bytes.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for len in 0..=12 {
            for _ in 0..2000 {
                let bytes: Vec<u8> = (0..len)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        // Weight towards high bytes and the stand-in range's lead bytes.
                        match state % 4 {
                            0 => b"\xF4\x8F\xBE\xBF"[(state >> 8) as usize % 4],
                            _ => (state >> 16) as u8,
                        }
                    })
                    .collect();
                assert_eq!(encode(&decode(&bytes)), bytes, "{bytes:02x?}");
            }
        }
    }
}
