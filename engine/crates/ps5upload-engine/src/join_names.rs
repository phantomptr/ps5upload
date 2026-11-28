//! Names are unique inside a Play Room (design: play-rooms 3.5).
//!
//! The host engine checks every name at the hello and the knock: the host's own name, everyone in,
//! everyone asking and everyone in the resume window count. Two names are the same when their
//! **compare keys** are: Unicode NFKC, case folded, invisible format characters dropped, spaces
//! collapsed and trimmed, then each look-alike mapped to one letter (a small vendored subset of the
//! Unicode confusables skeleton, UTS #39, for Latin, Cyrillic and Greek, plus the digit and
//! letter-pair look-alikes). So "Sam", "SAM " and "Ѕаm" (Cyrillic) are one name, and the second
//! person is offered "Sam (2)".
//!
//! The key is only ever compared, never shown: the guest keeps the name as they typed it.

use unicode_normalization::UnicodeNormalization;

/// Longest name a guest may have (characters); a suggestion never goes past it.
pub(crate) const NAME_MAX: usize = 24;

fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{FFF0}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}')
        || matches!(c, '\u{0300}'..='\u{036F}') // combining marks: "Sām" reads as "Sam"
}

/// One character's look-alike in plain Latin (after case folding), or itself.
fn skeleton_char(c: char) -> char {
    match c {
        // Cyrillic
        'а' => 'a',
        'в' => 'b',
        'с' | 'ϲ' => 'c',
        'ԁ' => 'd',
        'е' | 'ё' | 'є' => 'e',
        'һ' | 'н' => 'h',
        'і' | 'ї' | 'ӏ' => 'l',
        'ј' => 'j',
        'к' | 'κ' => 'k',
        'м' | 'μ' => 'm',
        'о' | 'ο' | 'σ' | '0' => 'o',
        'р' | 'ρ' => 'p',
        'ԛ' => 'q',
        'ѕ' => 's',
        'т' | 'τ' => 't',
        'у' | 'γ' => 'y',
        'ѵ' | 'ν' => 'v',
        'ԝ' | 'ω' => 'w',
        'х' | 'χ' => 'x',
        // Greek
        'α' => 'a',
        'β' => 'b',
        'ε' => 'e',
        'η' => 'n',
        'ι' | 'ı' | 'i' | '1' | '|' | '!' => 'l',
        'υ' => 'u',
        'ζ' => 'z',
        // Digits that pass for letters
        '3' => 'e',
        '5' => 's',
        '8' => 'b',
        _ => c,
    }
}

/// The compare key of `name` (see the module notes). Empty for a name with nothing visible.
pub(crate) fn compare_key(name: &str) -> String {
    let folded: String = name
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfd()
        .filter(|c| !invisible(*c))
        .collect();
    let mut out = String::with_capacity(folded.len());
    let mut space = false;
    for c in folded.chars() {
        if c.is_whitespace() || c == '_' || c == '-' || c == '.' {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(skeleton_char(c));
    }
    // Letter pairs that read as one letter (UTS #39: "rn" → "m", "vv" → "w", "cl" → "d").
    out.replace("rn", "m").replace("vv", "w").replace("cl", "d")
}

/// True when `a` and `b` read as the same name.
pub(crate) fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (compare_key(a), compare_key(b));
    !a.is_empty() && a == b
}

/// The name a guest who gave none is shown as (the host's app says "Friend" in its language; the
/// compare key needs one word).
pub(crate) const UNNAMED: &str = "Friend";

/// `name` as the room compares it: an empty one is [`UNNAMED`].
pub(crate) fn effective(name: &str) -> &str {
    if name.trim().is_empty() {
        UNNAMED
    } else {
        name
    }
}

/// `None` when `name` is free among `taken`, else the lowest free "name (n)" (n ≥ 2), cut so it
/// stays within [`NAME_MAX`] characters.
pub(crate) fn suggest<'a>(name: &str, taken: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let keys: Vec<String> = taken
        .into_iter()
        .map(|t| compare_key(effective(t)))
        .filter(|k| !k.is_empty())
        .collect();
    let base = effective(name).trim();
    if !keys.contains(&compare_key(base)) {
        return None;
    }
    (2..=99).find_map(|n| {
        let tail = format!(" ({n})");
        let room = NAME_MAX.saturating_sub(tail.chars().count());
        let head: String = base.chars().take(room).collect();
        let s = format!("{}{tail}", head.trim_end());
        (!keys.contains(&compare_key(&s))).then_some(s)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_space_and_look_alikes_are_one_name() {
        assert!(same_name("Sam", "SAM "));
        assert!(same_name("Sam", "  s a m".replace(' ', "").as_str()));
        assert!(same_name("Sam", "Ѕаm")); // Cyrillic Dze and a
        assert!(same_name("Mia", "Μіа")); // Greek Mu, Cyrillic i
        assert!(same_name("Bob", "B0b"));
        assert!(same_name("Lee", "l33"));
        assert!(same_name("Sam", "S\u{200B}am"));
        assert!(same_name("Sam", "Sām"));
        assert!(same_name("Mom", "rnom"));
        assert!(same_name("Alex Kim", "alex   kim"));
        assert!(same_name("Ｓａｍ", "Sam")); // full width, NFKC
        assert!(!same_name("Sam", "Sami"));
        assert!(!same_name("Mia", "Max"));
        assert!(!same_name("", ""));
        // Other scripts keep their letters.
        assert!(same_name("さくら", "さくら"));
        assert!(!same_name("さくら", "さくろ"));
    }

    #[test]
    fn suggestions_take_the_lowest_free_number() {
        assert_eq!(suggest("Sam", ["Mia", "Lee"]), None);
        assert_eq!(suggest("Sam", ["sam"]).as_deref(), Some("Sam (2)"));
        assert_eq!(
            suggest("Sam", ["Sam", "Sam (2)", "SAM (3)"]).as_deref(),
            Some("Sam (4)")
        );
        assert_eq!(suggest("", ["Friend"]).as_deref(), Some("Friend (2)"));
        assert_eq!(suggest("Friend", [""]).as_deref(), Some("Friend (2)"));
        let long = "A".repeat(30);
        let s = suggest(&long, [long.as_str()]).unwrap();
        assert!(s.chars().count() <= NAME_MAX, "{s}");
        assert!(s.ends_with(" (2)"));
    }
}
