//! What a package is (a game, its update, or add-on content) and the version helpers a game
//! collection needs. The rules are PS Game Library's (`pspkg.py`), verified there against every
//! package on a real library.

use serde::{Deserialize, Serialize};

/// The three things a package can be to its game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Base,
    Patch,
    Dlc,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Base => "base",
            Kind::Patch => "patch",
            Kind::Dlc => "dlc",
        }
    }
}

/// `content_type` values the container header carries, with their platform.
pub fn content_type_platform(content_type: u32) -> Option<&'static str> {
    match content_type {
        0x1A | 0x1B => Some("PS4"),
        0x20 | 0x21 | 0x26 => Some("PS5"),
        _ => None,
    }
}

const DLC_CONTENT_TYPES: [u32; 2] = [0x1B, 0x21];
/// 0x40000000 a later patch, 0x00100000 the first patch.
const PATCH_FLAG_MASK: u32 = 0x4010_0000;

/// What the header says the package is, whether that is certain, and why.
///
/// The header (`content_type`, `content_flags`) decides. On PS4 the `param.sfo` CATEGORY is a
/// second witness: when it disagrees the header still wins, but the answer is marked unsure,
/// and the reason names both.
pub fn classify(content_type: u32, content_flags: u32, category: &str) -> (Kind, bool, String) {
    let known = content_type_platform(content_type).is_some();
    let (kind, why) = if DLC_CONTENT_TYPES.contains(&content_type) {
        (
            Kind::Dlc,
            format!("content_type={content_type:#04x} is additional content"),
        )
    } else if content_flags & PATCH_FLAG_MASK != 0 {
        (
            Kind::Patch,
            format!(
                "content_flags={content_flags:#010x} has the patch bit {:#010x}",
                content_flags & PATCH_FLAG_MASK
            ),
        )
    } else {
        (
            Kind::Base,
            format!(
                "content_type={content_type:#04x}, content_flags={content_flags:#010x} has no patch bit"
            ),
        )
    };
    if !known {
        return (
            kind,
            false,
            format!("content_type={content_type:#04x} unrecognized; {why}"),
        );
    }
    let sfo_kind = match category.trim().to_ascii_lowercase().as_str() {
        "gd" => Some(Kind::Base),
        "gp" => Some(Kind::Patch),
        "ac" => Some(Kind::Dlc),
        _ => None,
    };
    if let Some(sfo) = sfo_kind {
        if sfo != kind {
            return (
                kind,
                false,
                format!(
                    "header says {} ({why}) but param.sfo CATEGORY={category} says {}",
                    kind.as_str(),
                    sfo.as_str()
                ),
            );
        }
    }
    (kind, true, why)
}

/// What a game folder or image is, from its PARAM category alone (`gd`, `gp`, `ac`, PS5
/// `gde`/`gdc`…, PS3 `GD`/`HG`/`AC`/`AP`).
pub fn classify_category(category: &str) -> (Kind, bool, String) {
    let c = category.trim();
    let kind = match c.to_ascii_lowercase().as_str() {
        "gp" | "ap" => Some(Kind::Patch),
        "ac" => Some(Kind::Dlc),
        "gd" | "hg" | "gde" | "gdc" | "gda" | "gdb" | "gdd" => Some(Kind::Base),
        _ => None,
    };
    match kind {
        Some(k) => (k, true, format!("CATEGORY={c}")),
        None => (Kind::Base, false, format!("no usable CATEGORY (got {c:?})")),
    }
}

/// Region from a content ID's prefix: `EP0002-…` → Europe.
pub fn region_of_content_id(content_id: &str) -> String {
    let pre: String = content_id
        .split('-')
        .next()
        .unwrap_or("")
        .chars()
        .take(2)
        .collect::<String>()
        .to_ascii_uppercase();
    match pre.as_str() {
        "EP" => "Europe".into(),
        "UP" => "Americas".into(),
        "JP" => "Japan".into(),
        "HP" => "Asia".into(),
        _ => pre,
    }
}

/// Region from a PS3 title ID's prefix, for packages whose content ID says nothing.
pub fn region_of_ps3_title_id(title_id: &str) -> String {
    let pre = title_id.get(..4).unwrap_or("").to_ascii_uppercase();
    match pre.as_str() {
        "NPEB" | "BCES" => "Europe",
        "NPHB" | "BCAS" => "Asia",
        "NPJB" | "BCJS" => "Japan",
        "NPUB" | "BCUS" => "Americas",
        _ => "",
    }
    .into()
}

/// `UP9000-CUSA00900_00-BLOODBORNE000000` → `CUSA00900`.
pub fn title_id_of_content_id(content_id: &str) -> String {
    content_id
        .split('-')
        .nth(1)
        .and_then(|s| s.split('_').next())
        .unwrap_or("")
        .to_string()
}

/// `04.040.100` → `4.40.100`; keeps the conventional `1.06`. Never fails.
pub fn normalize_version(ver: &str) -> String {
    ver.trim()
        .split('.')
        .enumerate()
        .filter_map(|(i, s)| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else if s.chars().all(|c| c.is_ascii_digit()) && (i == 0 || s.len() > 2) {
                Some(s.trim_start_matches('0').to_string()).map(|t| {
                    if t.is_empty() {
                        "0".into()
                    } else {
                        t
                    }
                })
            } else {
                Some(s.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Orders versions numerically segment by segment; non-numeric segments sort last.
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |v: &str| -> Vec<(u8, u64)> {
        v.split('.')
            .map(|s| match s.trim().parse::<u64>() {
                Ok(n) => (0, n),
                Err(_) => (1, 0),
            })
            .collect()
    };
    key(a).cmp(&key(b))
}

/// The higher of two versions, ignoring blanks.
pub fn newer_version(a: &str, b: &str) -> String {
    if a.trim().is_empty() {
        return b.trim().to_string();
    }
    if b.trim().is_empty() {
        return a.trim().to_string();
    }
    if version_cmp(a, b) == std::cmp::Ordering::Less {
        b.trim().to_string()
    } else {
        a.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_decides_and_category_only_vouches() {
        let (k, sure, why) = classify(0x1A, 0x0A00_0000, "gd");
        assert_eq!((k, sure), (Kind::Base, true));
        assert!(why.contains("has no patch bit"));
        let (k, sure, why) = classify(0x1A, 0x6210_0000, "gp");
        assert_eq!((k, sure), (Kind::Patch, true));
        assert!(why.contains("0x40100000"), "{why}");
        assert_eq!(classify(0x1B, 0, "ac").0, Kind::Dlc);
        assert_eq!(classify(0x21, 0, "").0, Kind::Dlc);
        // A disagreement keeps the header's answer, marked unsure with both sides.
        let (k, sure, why) = classify(0x1A, 0, "gp");
        assert_eq!((k, sure), (Kind::Base, false));
        assert!(why.contains("CATEGORY=gp says patch"), "{why}");
        let (_, sure, why) = classify(0x99, 0, "");
        assert!(!sure && why.contains("unrecognized"));
    }

    #[test]
    fn categories_name_what_folders_and_images_are() {
        assert_eq!(classify_category("gd").0, Kind::Base);
        assert_eq!(classify_category("gp").0, Kind::Patch);
        assert_eq!(classify_category("AC").0, Kind::Dlc);
        assert_eq!(classify_category("AP").0, Kind::Patch);
        let (k, sure, _) = classify_category("");
        assert_eq!((k, sure), (Kind::Base, false));
    }

    #[test]
    fn versions_normalize_and_compare_numerically() {
        assert_eq!(normalize_version("04.040.100"), "4.40.100");
        assert_eq!(normalize_version("01.06"), "1.06");
        assert_eq!(normalize_version("1.000.000"), "1.0.0");
        assert_eq!(newer_version("01.00", "01.04"), "01.04");
        assert_eq!(newer_version("", "1.2"), "1.2");
        assert_eq!(version_cmp("1.10", "1.9"), std::cmp::Ordering::Greater);
        assert_eq!(version_cmp("1.32", "1.32"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn ids_and_regions_come_from_the_content_id() {
        assert_eq!(
            title_id_of_content_id("UP9000-CUSA00900_00-BLOODBORNE000000"),
            "CUSA00900"
        );
        assert_eq!(region_of_content_id("EP0002-PPSA00001_00-X"), "Europe");
        assert_eq!(region_of_content_id("UP9000-CUSA00900_00-X"), "Americas");
        assert_eq!(region_of_ps3_title_id("NPUB30001"), "Americas");
    }
}
