//! The rule that keeps Share to homebrew: PS5Upload never shares a commercial game, in any format
//! (folder, dump, mount image, package), nor its updates or add-ons, which carry the game's own
//! Title ID. The engine applies it to every app it is asked to share (and to every app a sender
//! offers, on the receiving side), whatever the app's page sent: it is not a UI check.
//!
//! An app is refused when
//! * its Title ID starts with one of Sony's product-code prefixes ([`RETAIL_PREFIXES`]), in any
//!   case, with spaces, `-` or `_` in it (`ppsa 01234`, `PPSA-01234`), and whatever follows the
//!   prefix: a homebrew that borrows a retail-looking prefix is refused too, which is the safe
//!   side;
//! * its Content ID names such a Title ID (`UP0001-CUSA01234_00-…`), or carries a real
//!   publisher prefix ([`PUBLISHER_REGIONS`] followed by four digits, `UP0001-`) in front of a
//!   Title ID of the retail shape (four letters, five digits). Homebrew packages use `IV0000-`
//!   and the like, which no store uses;
//! * it says neither (no param.json or param.sfo could be read): what can't be identified can't
//!   be shown to be homebrew.
//!
//! Everything else is homebrew (`PSUP00001`, `LAPY10010`, `ITEM00001`, `BREW00042`, …).

/// Sony's product-code prefixes for games and their add-ons, by platform. PS5 and PS4 are what a
/// PS5 runs; the older platforms' codes are here because a dump or a classic repackaged for a PS4
/// can carry them, and refusing those is the safe side.
pub(crate) const RETAIL_PREFIXES: &[&str] = &[
    // PS5: the Title ID of every PS5 game, and the disc product codes.
    "PPSA", "PPSB", "ECAS", "ECJS", "ECJM", "ECKS", "ELAS", "ELJS", "ELJM", "ELKS", "ERJS", "ERJM",
    "ECSA",
    // PS4: the Title ID of every PS4 game (and add-on, theme, classic), and disc product codes.
    "CUSA", "CUSB", "PLAS", "PLJS", "PLJM", "PLKS", "PCAS", "PCJS", "PCKS", "PCSA", "PCSB", "PCSC",
    "PCSD", "PCSE", "PCSF", "PCSG", "PCSH", // PS Vita product codes.
    "PCHS", "PCLS", "VCAS", "VCJS", "VCKS", "VLAS", "VLJS", "VLJM", "VLKS",
    // PS3 discs and PSN.
    "BCAS", "BCES", "BCJS", "BCKS", "BCUS", "BLAS", "BLES", "BLJM", "BLJS", "BLKS", "BLUS", "NPEA",
    "NPEB", "NPEZ", "NPHA", "NPHB", "NPJA", "NPJB", "NPUA", "NPUB", "NPUZ",
    // PS2 and PS1 discs (classics carry them inside).
    "SCAJ", "SCED", "SCES", "SCKA", "SCPS", "SCUS", "SLAJ", "SLED", "SLES", "SLKA", "SLPM", "SLPS",
    "SLUS", // Sony's own system software.
    "NPXS",
];

/// The first two letters of a store's publisher code (`UP0001`, `EP9000`, …): America, Europe,
/// Japan, Asia (Hong Kong), Korea, and the Indian/other storefronts.
pub(crate) const PUBLISHER_REGIONS: &[&str] = &["UP", "EP", "JP", "HP", "KP", "IP"];

/// Why an app may not be shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A commercial game (or its update or add-on): the Title ID shown.
    Retail(String),
    /// Neither a Title ID nor a Content ID could be read.
    Unknown,
}

impl Refusal {
    /// What the person is told.
    pub(crate) fn message(&self) -> String {
        match self {
            Refusal::Retail(id) => {
                format!("This is a retail game ({id}). PS5Upload only shares homebrew.")
            }
            Refusal::Unknown => "PS5Upload can't tell which app this is, so it can't share it. \
                                 It only shares homebrew."
                .to_string(),
        }
    }

    /// The Title ID it names, for the API (`refused.title_id`).
    pub(crate) fn title_id(&self) -> Option<&str> {
        match self {
            Refusal::Retail(id) => Some(id),
            Refusal::Unknown => None,
        }
    }
}

/// Upper case with spaces, `-`, `_` and `.` taken out: how a Title ID is compared.
fn squeeze(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '_' | '.'))
        .flat_map(char::to_uppercase)
        .collect()
}

/// The retail prefix `title_id` starts with, if any.
fn retail_prefix(title_id: &str) -> Option<&'static str> {
    let t = squeeze(title_id);
    RETAIL_PREFIXES.iter().copied().find(|p| t.starts_with(p))
}

/// Four letters and five digits: how a store's Title ID looks.
fn retail_shaped(title_id: &str) -> bool {
    let t = squeeze(title_id);
    t.len() == 9
        && t[..4].bytes().all(|b| b.is_ascii_uppercase())
        && t[4..].bytes().all(|b| b.is_ascii_digit())
}

/// A Title ID as the refusal names it (`PPSA01234`), from what the app said.
fn shown(title_id: &str) -> String {
    let t = squeeze(title_id);
    if t.is_empty() {
        "unknown".into()
    } else {
        t.chars().take(16).collect()
    }
}

/// The parts of a Content ID (`UP0001-CUSA01234_00-LABEL`): the publisher code and the Title ID.
/// Spaces are ignored and case does not matter.
fn content_parts(content_id: &str) -> Option<(String, String)> {
    let c: String = content_id
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect();
    let (publisher, rest) = c.split_once('-')?;
    let title = rest.split(['_', '-']).next().unwrap_or("");
    Some((publisher.to_string(), title.to_string()))
}

/// Whether an app with this Title ID and Content ID (either may be missing or empty) may be
/// shared. See the module's notes for the rules.
pub(crate) fn check(title_id: Option<&str>, content_id: Option<&str>) -> Result<(), Refusal> {
    let title = title_id.map(str::trim).filter(|t| !t.is_empty());
    let content = content_id.map(str::trim).filter(|c| !c.is_empty());
    if title.is_none() && content.is_none() {
        return Err(Refusal::Unknown);
    }
    if let Some(t) = title {
        if retail_prefix(t).is_some() {
            return Err(Refusal::Retail(shown(t)));
        }
    }
    if let Some((publisher, ctitle)) = content.and_then(content_parts) {
        if retail_prefix(&ctitle).is_some() {
            return Err(Refusal::Retail(shown(&ctitle)));
        }
        let real_publisher = publisher.len() == 6
            && PUBLISHER_REGIONS.contains(&&publisher[..2])
            && publisher[2..].bytes().all(|b| b.is_ascii_digit());
        if real_publisher && (retail_shaped(&ctitle) || title.is_some_and(retail_shaped)) {
            let id = if retail_shaped(&ctitle) {
                ctitle
            } else {
                title.unwrap_or_default().to_string()
            };
            return Err(Refusal::Retail(shown(&id)));
        }
    } else if let Some(c) = content {
        // A Content ID that is not one (no `-`): read it as a Title ID, as the safe side.
        if retail_prefix(c).is_some() {
            return Err(Refusal::Retail(shown(c)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(t: Option<&str>, c: Option<&str>) -> String {
        match check(t, c) {
            Err(r) => r.message(),
            Ok(()) => panic!("{t:?} / {c:?} was allowed"),
        }
    }

    #[test]
    fn a_ps5_or_ps4_retail_title_is_refused_with_the_message() {
        assert_eq!(
            refused(Some("PPSA01234"), None),
            "This is a retail game (PPSA01234). PS5Upload only shares homebrew."
        );
        assert!(refused(Some("CUSA00001"), None).contains("(CUSA00001)"));
        for p in [
            "PCAS", "PCJS", "PCKS", "PCSE", "PCSA", "PCSB", "PCSF", "PCSG", "PCSH", "PLAS", "PLJS",
            "PLJM", "ELJM", "ELJS", "ECAS", "ELAS", "ERJM", "BLUS", "SLUS", "NPXS",
        ] {
            let id = format!("{p}12345");
            assert!(check(Some(&id), None).is_err(), "{id} allowed");
        }
    }

    #[test]
    fn case_spaces_and_separators_do_not_get_a_retail_title_through() {
        for id in [
            "ppsa01234",
            " PPSA01234 ",
            "PPSA 01234",
            "ppsa-01234",
            "Ppsa_01234",
            "P P S A 0 1 2 3 4",
            "\tcusa12345\n",
        ] {
            assert!(check(Some(id), None).is_err(), "{id:?} allowed");
        }
        assert!(refused(Some("ppsa-01234"), None).contains("(PPSA01234)"));
    }

    #[test]
    fn homebrew_that_reuses_a_retail_prefix_is_refused_the_safe_side() {
        // Not the retail shape, but the prefix is a store's: refused.
        for id in ["PPSA0", "PPSAHOMEBREW", "CUSA999999", "PPSA"] {
            assert!(check(Some(id), None).is_err(), "{id} allowed");
        }
    }

    #[test]
    fn homebrew_title_ids_are_allowed() {
        for id in [
            "PSUP00001",
            "LAPY10010",
            "ITEM00001",
            "BREW00042",
            "psup00001",
            "HBRW12345",
            "TEST00000",
            "NOTAGAME",
        ] {
            assert_eq!(check(Some(id), None), Ok(()), "{id} refused");
        }
    }

    #[test]
    fn nothing_to_go_by_is_refused() {
        assert_eq!(check(None, None), Err(Refusal::Unknown));
        assert_eq!(check(Some("  "), Some("")), Err(Refusal::Unknown));
        assert!(refused(None, None).contains("can't tell"));
    }

    #[test]
    fn the_content_id_is_checked_too() {
        // A retail title inside the content ID, whatever the Title ID says.
        assert!(check(
            Some("PSUP00001"),
            Some("UP0001-CUSA01234_00-GAMEGAMEGAMEGAME")
        )
        .is_err());
        assert!(check(None, Some("EP9000-PPSA01234_00-ABCDEFGHIJKLMNOP")).is_err());
        assert!(check(None, Some("iv0000-ppsa01234_00-abcdefghijklmnop")).is_err());
        // A real publisher prefix in front of a retail-shaped Title ID: refused.
        for p in ["UP", "EP", "JP", "HP", "KP", "IP"] {
            let c = format!("{p}1234-LAPY10010_00-ABCDEFGHIJKLMNOP");
            assert!(check(Some("LAPY10010"), Some(&c)).is_err(), "{c} allowed");
        }
        assert!(refused(Some("LAPY10010"), Some("UP0001-LAPY10010_00-X")).contains("LAPY10010"));
        // Homebrew's own prefixes: allowed.
        assert_eq!(
            check(
                Some("LAPY10010"),
                Some("IV0000-LAPY10010_00-HOMEBREW00000000")
            ),
            Ok(())
        );
        assert_eq!(
            check(None, Some("IV0001-PSUP00001_00-0000000000000000")),
            Ok(())
        );
        // A publisher-looking prefix before a Title ID of another shape: homebrew.
        assert_eq!(check(None, Some("UP0001-MYAPP_00-X")), Ok(()));
        // A content ID that is really a Title ID.
        assert!(check(None, Some("CUSA12345")).is_err());
    }

    #[test]
    fn updates_and_add_ons_carry_the_games_title_and_are_refused() {
        // A patch's and a DLC's identity: the base game's Title ID with their own label.
        assert!(check(
            Some("CUSA01234"),
            Some("UP0001-CUSA01234_00-PATCH00000000001")
        )
        .is_err());
        assert!(check(
            Some("PPSA01234"),
            Some("EP0002-PPSA01234_00-DLCPACK000000001")
        )
        .is_err());
        assert!(check(None, Some("JP0003-PPSA01234_00-SEASONPASS000000")).is_err());
    }

    #[test]
    fn the_refusal_names_the_title_id() {
        assert_eq!(
            check(Some("ppsa01234"), None).unwrap_err().title_id(),
            Some("PPSA01234")
        );
        assert_eq!(check(None, None).unwrap_err().title_id(), None);
    }
}
