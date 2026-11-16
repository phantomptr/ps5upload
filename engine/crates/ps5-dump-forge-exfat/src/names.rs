//! exFAT names: what a component may hold, how the volume upcases it, and its hash.
//!
//! The volume is case-insensitive through its up-case table, so two names that upcase to
//! the same UTF-16 units are one name. The table written into the image is the one used
//! here, which keeps the collision check, the name hashes and the driver in agreement.

use std::sync::OnceLock;

use crate::upcase_table::COMPRESSED;

/// Longest file name, in UTF-16 units (FileNameLength is one byte).
pub(crate) const MAX_NAME_UNITS: usize = 255;

/// Why `name` cannot be an exFAT file name, or `None` when it can.
pub(crate) fn problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("empty name".into());
    }
    if name == "." || name == ".." {
        return Some("reserved name".into());
    }
    let mut why = Vec::new();
    if let Some(c) = name.chars().find(|&c| forbidden(c)) {
        why.push(format!("forbidden character {c:?}"));
    }
    // The scanners map undecodable bytes to U+FFFD; writing it would rename the file.
    if name.contains('\u{FFFD}') {
        why.push("name contains U+FFFD — it was renamed lossily somewhere".into());
    }
    // macOS hands out decomposed names; the game looks its files up by the composed
    // spelling, and exFAT compares units, not canonical equivalents.
    if !unicode_normalization::is_nfc(name) {
        why.push("not in Unicode NFC".into());
    }
    let units = name.encode_utf16().count();
    if units > MAX_NAME_UNITS {
        why.push(format!("{units} UTF-16 units long (max {MAX_NAME_UNITS})"));
    }
    (!why.is_empty()).then(|| why.join("; "))
}

/// Label characters follow the file-name rules; the field holds 11 units.
pub(crate) fn label_problem(label: &str) -> Option<String> {
    let units = label.encode_utf16().count();
    if units > 11 {
        return Some(format!("{units} UTF-16 units long (max 11)"));
    }
    label
        .chars()
        .find(|&c| forbidden(c))
        .map(|c| format!("forbidden character {c:?}"))
}

/// The exFAT FileName rule (spec §7.7.3): no control characters and none of `"*/:<>?\|`.
fn forbidden(c: char) -> bool {
    c < '\u{20}' || matches!(c, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|')
}

/// `name` as the volume compares it: UTF-16, every unit through the up-case table.
/// Surrogate halves map to themselves, as the table leaves them.
pub(crate) fn upcase(name: &str) -> Vec<u16> {
    let table = table();
    name.encode_utf16().map(|u| table[usize::from(u)]).collect()
}

/// NameHash of the Stream Extension entry (spec §7.6.4): over the up-cased name's bytes.
pub(crate) fn name_hash(upcased: &[u16]) -> u16 {
    let mut hash: u16 = 0;
    for unit in upcased {
        for byte in unit.to_le_bytes() {
            hash = hash.rotate_right(1).wrapping_add(u16::from(byte));
        }
    }
    hash
}

/// The compressed table as it is stored in the image.
pub(crate) fn table_bytes() -> Vec<u8> {
    COMPRESSED.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// TableChecksum of the Up-case Table entry (spec §7.2.2): over the stored bytes.
pub(crate) fn table_checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |sum, &b| {
        sum.rotate_right(1).wrapping_add(u32::from(b))
    })
}

/// The table expanded to one entry per BMP code unit. `0xFFFF, n` is a run of `n`
/// identity mappings; a trailing `0xFFFF` with nothing after it is the literal mapping
/// of U+FFFF, which is how the recommended table ends.
fn table() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| expand(&COMPRESSED))
}

/// An up-case table as stored (compressed or not) expanded to one entry per BMP code unit;
/// units past its end map to themselves.
pub(crate) fn expand(stored: &[u16]) -> Vec<u16> {
    let mut out: Vec<u16> = (0..=u16::MAX).collect();
    let mut at = 0usize;
    let mut i = 0usize;
    while i < stored.len() && at < out.len() {
        if stored[i] == 0xFFFF && i + 1 < stored.len() {
            at += usize::from(stored[i + 1]);
            i += 2;
        } else {
            out[at] = stored[i];
            at += 1;
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_the_recommended_one() {
        let bytes = table_bytes();
        assert_eq!(bytes.len(), 5836);
        assert_eq!(table_checksum(&bytes), 0xE619_D30D);
    }
}
