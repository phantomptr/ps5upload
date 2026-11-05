//! Split packages: one package kept as several files. Two layouts exist, and the Collection, the
//! organizer and Move to Trash all treat either as one thing:
//! * `Game.pkg`, `Game.pkg.0`, `Game.pkg.1`… (what the engine's set parser reads);
//! * `Game_0.pkg`, `Game_1.pkg`, `Game_2.pkg`… (sets named by part).
//!
//! A name alone cannot tell the second layout from packages somebody happened to call
//! `Bloodborne_1.pkg` and `Bloodborne_2.pkg`. Only the first file of a set has a package header,
//! so a set is a part 0 and later parts that are not packages themselves.

use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// `Game_0.pkg`, `Game_1.pkg`…
    Named,
    /// `Game.pkg`, `Game.pkg.0`…
    Suffix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitSet {
    pub layout: Layout,
    /// The file with the header: it names the set.
    pub lead: String,
    /// The later parts in order, each with its number.
    pub parts: Vec<(String, u32)>,
}

impl SplitSet {
    /// A part is missing from the run (`_0`, `_2`). Numbers start after the lead: `_1`, or `.0`.
    pub fn has_gap(&self) -> bool {
        let first = match self.layout {
            Layout::Named => 1,
            Layout::Suffix => 0,
        };
        self.parts
            .iter()
            .enumerate()
            .any(|(i, (_, n))| u64::from(*n) != i as u64 + first)
    }
}

/// `name_N.pkg`: the stem and the part number.
pub fn named_part(name: &str) -> Option<(&str, u32)> {
    let cut = name.len().checked_sub(4)?;
    let stem = name.get(..cut)?;
    if !name.get(cut..)?.eq_ignore_ascii_case(".pkg") {
        return None;
    }
    let (head, num) = stem.rsplit_once('_')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head, num.parse().ok()?))
}

/// `name.pkg.N`: the lead's name and the part number.
pub fn suffix_part(name: &str) -> Option<(&str, u32)> {
    let (lead, num) = name.rsplit_once('.')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let cut = lead.len().checked_sub(4)?;
    if !lead.get(cut..)?.eq_ignore_ascii_case(".pkg") {
        return None;
    }
    Some((lead, num.parse().ok()?))
}

/// The split sets among the file names of one folder. `is_package` says whether a `_N` file
/// (N ≥ 1) is a package of its own; it is asked only when that file has a part 0 to belong to.
pub fn split_sets(names: &[&str], is_package: &mut dyn FnMut(&str) -> bool) -> Vec<SplitSet> {
    let mut sets = Vec::new();

    // `Game.pkg` + `Game.pkg.N`: these names are never packages themselves.
    let leads: BTreeMap<String, &str> = names
        .iter()
        .filter(|n| n.len() >= 4 && n.is_char_boundary(n.len() - 4))
        .filter(|n| n[n.len() - 4..].eq_ignore_ascii_case(".pkg"))
        .map(|n| (n.to_ascii_lowercase(), *n))
        .collect();
    let mut suffixed: BTreeMap<&str, Vec<(String, u32)>> = BTreeMap::new();
    for n in names {
        if let Some((lead, k)) = suffix_part(n) {
            if let Some(found) = leads.get(&lead.to_ascii_lowercase()) {
                suffixed
                    .entry(*found)
                    .or_default()
                    .push(((*n).to_string(), k));
            }
        }
    }
    let with_suffix_parts: HashSet<&str> = suffixed.keys().copied().collect();
    for (lead, mut parts) in suffixed {
        parts.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        sets.push(SplitSet {
            layout: Layout::Suffix,
            lead: lead.to_string(),
            parts,
        });
    }

    // `Game_0.pkg` + `Game_N.pkg`.
    let mut named: BTreeMap<&str, BTreeMap<u32, &str>> = BTreeMap::new();
    for n in names.iter().filter(|n| !with_suffix_parts.contains(*n)) {
        if let Some((stem, k)) = named_part(n) {
            // Two spellings of one number (`_1`, `_01`): the first is the part, the other stays
            // a file of its own.
            named.entry(stem).or_default().entry(k).or_insert(*n);
        }
    }
    for (_, numbered) in named {
        let Some(lead) = numbered.get(&0) else {
            continue;
        };
        let parts: Vec<(String, u32)> = numbered
            .iter()
            .filter(|(k, _)| **k > 0)
            .filter(|(_, n)| !is_package(n))
            .map(|(k, n)| ((*n).to_string(), *k))
            .collect();
        if !parts.is_empty() {
            sets.push(SplitSet {
                layout: Layout::Named,
                lead: (*lead).to_string(),
                parts,
            });
        }
    }
    sets
}

/// A file's first bytes: is this the start of a package (PS4, PS5 or PS3)?
pub fn is_package_head(head: &[u8; 4]) -> bool {
    let magic = u32::from_be_bytes(*head);
    magic == ps5upload_pkg::PKG_MAGIC
        || magic == ps5upload_pkg::PKG_MAGIC_FIH
        || *head == ps5upload_pkg::ps3::PS3_MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets(names: &[&str], packages: &[&str]) -> Vec<SplitSet> {
        split_sets(names, &mut |n| packages.contains(&n))
    }

    #[test]
    fn a_set_named_by_part_is_a_part_zero_and_files_with_no_header() {
        let got = sets(&["Big_0.pkg", "Big_1.pkg", "Big_2.pkg", "Other.pkg"], &[]);
        assert_eq!(
            got,
            vec![SplitSet {
                layout: Layout::Named,
                lead: "Big_0.pkg".into(),
                parts: vec![("Big_1.pkg".into(), 1), ("Big_2.pkg".into(), 2)],
            }]
        );
        assert!(!got[0].has_gap());
    }

    #[test]
    fn packages_that_only_look_like_parts_stay_apart() {
        // No part 0: two packages somebody named this way.
        assert!(sets(&["Bloodborne_1.pkg", "Bloodborne_2.pkg"], &[]).is_empty());
        // A part 0, and a later file with a header of its own.
        assert!(sets(&["Game_0.pkg", "Game_1.pkg"], &["Game_1.pkg"]).is_empty());
        // One real part beside one package: the set holds only the real part.
        let got = sets(&["G_0.pkg", "G_1.pkg", "G_2.pkg"], &["G_2.pkg"]);
        assert_eq!(got[0].parts, vec![("G_1.pkg".to_string(), 1)]);
    }

    #[test]
    fn a_missing_part_shows_as_a_gap_and_keeps_its_numbering() {
        let got = sets(&["Big_0.pkg", "Big_2.pkg"], &[]);
        assert_eq!(got[0].parts, vec![("Big_2.pkg".to_string(), 2)]);
        assert!(got[0].has_gap());
        let got = sets(&["Big.pkg", "Big.pkg.1"], &[]);
        assert!(got[0].has_gap());
    }

    #[test]
    fn a_set_with_numbered_suffixes_belongs_to_its_lead() {
        let got = sets(&["Game.PKG", "Game.PKG.1", "Game.PKG.0", "Lone.pkg.0"], &[]);
        assert_eq!(
            got,
            vec![SplitSet {
                layout: Layout::Suffix,
                lead: "Game.PKG".into(),
                parts: vec![("Game.PKG.0".into(), 0), ("Game.PKG.1".into(), 1)],
            }]
        );
        assert!(!got[0].has_gap());
    }

    #[test]
    fn two_spellings_of_one_number_leave_the_second_alone() {
        let got = sets(&["X_0.pkg", "X_01.pkg", "X_1.pkg"], &[]);
        assert_eq!(got[0].parts, vec![("X_01.pkg".to_string(), 1)]);
    }

    #[test]
    fn names_are_cut_on_character_boundaries() {
        for odd in [
            "é_0.pkg",
            "日本語.pkg.0",
            "é",
            "_",
            ".pkg",
            "_0.pkg",
            "a_.pkg",
        ] {
            let _ = named_part(odd);
            let _ = suffix_part(odd);
        }
        assert_eq!(named_part("日本語_3.PKG"), Some(("日本語", 3)));
        assert_eq!(suffix_part("日本語.pkg.7"), Some(("日本語.pkg", 7)));
        assert_eq!(named_part("a_x.pkg"), None);
        assert_eq!(suffix_part("a.txt.0"), None);
    }

    #[test]
    fn the_three_package_magics_are_packages_and_anything_else_is_not() {
        assert!(is_package_head(&ps5upload_pkg::PKG_MAGIC.to_be_bytes()));
        assert!(is_package_head(&ps5upload_pkg::PKG_MAGIC_FIH.to_be_bytes()));
        assert!(is_package_head(&ps5upload_pkg::ps3::PS3_MAGIC));
        assert!(!is_package_head(&[7, 7, 7, 7]));
        assert!(!is_package_head(&[0, 0, 0, 0]));
    }
}
