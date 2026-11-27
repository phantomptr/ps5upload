//! Whether a PS5 can open a fake package of a given size, and how big its layout index is.
//!
//! The console checks a fake package's size when it mounts it, and each firmware line has its
//! own ceiling (from a firmware analysis the owner shared, 2026-11-27; not measured here):
//!
//! * 5.50 to 6.50: about 242.55 GiB at most (the largest of three thresholds: 151.354, 182.426,
//!   242.551 GiB). 5.10 has no such check.
//! * 7.00 to 10.60: 256 GiB at most.
//! * 11.00 to 12.xx: the game's own `appSizeInGib` when it is 256 to 320, else 256 GiB.
//! * 13.00 and later (13.00 to 13.60 confirmed): the package plus the add-on space the game
//!   reserves (`acSizeInGib`, counted even with no add-on installed) at most 450 GiB.
//!
//! A package past its ceiling installs and then never starts, so a build refuses it before any
//! work instead of writing hundreds of gigabytes the console can't open.
//!
//! The layout index (`naps_pkg_layout.dat`) is what the header calls `napsMetadataSize`: its
//! length sits at header `0xA8`, and the kernel allocates it whole before mounting, failing with
//! error 28 when it can't (`allocate_pkg_g6metadata_memory`). Its length follows the format,
//! not a choice of ours: 8 bytes per 64 KiB of the stored image, 9 per 256 KiB block (every
//! file opens one) and 6 per file. Ours matched packages built with Sony's tools for the same
//! game to within 1% (see [`LARGEST_MEASURED_INDEX`]); no kernel limit for it is known, so a
//! larger one is only a warning.

use crate::kraken_image;
use crate::plan::Plan;

const GIB: u64 = 1 << 30;

/// The largest layout index measured on a package made with Sony's tools: 50,380,176 bytes
/// (48.0 MiB) for a 201.6 GiB package of a 540.7 GiB game (2,362,470 block records). Others
/// measured: 13.2 MiB for a 34.9 GiB package of 166,720 files, 8.3 MiB for 39.6 GiB of 182
/// files, 0.66 MiB for 0.76 GiB of 35,254 files. Whether a console on 13.x mounts an index this
/// size is not known; past it a package is in territory nobody has measured.
pub const LARGEST_MEASURED_INDEX: u64 = 50_380_176;

/// A firmware version, `13.60` as `(0x13, 0x60)` read as decimal digits `(13, 60)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Firmware {
    pub major: u8,
    pub minor: u8,
}

impl Firmware {
    /// `13.60`, `5.1`, `05.10` or a `requiredSystemSoftwareVersion` word (`0x1360000000000000`).
    pub fn parse(text: &str) -> Option<Self> {
        let word = crate::source::firmware_word(text)?;
        let value = u64::from_str_radix(&word[2..], 16).ok()?;
        let digit = |b: u8| -> Option<u8> {
            let (hi, lo) = (b >> 4, b & 0xF);
            (hi < 10 && lo < 10).then_some(hi * 10 + lo)
        };
        let (major, minor) = (digit((value >> 56) as u8)?, digit((value >> 48) as u8)?);
        (major > 0).then_some(Self { major, minor })
    }

    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }
}

impl std::fmt::Display for Firmware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:02}", self.major, self.minor)
    }
}

/// What a game's `param.json` reserves: `acSizeInGib` (add-on space) and `appSizeInGib` (the
/// application's size profile). Found wherever they are in the file (`appSizeInGib` sits under
/// `kernel` in the games measured).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ParamSizes {
    pub ac_gib: Option<u64>,
    pub app_gib: Option<u64>,
}

/// The sizes a `param.json` declares; both absent when it declares neither or doesn't parse.
pub fn param_sizes(param_json: &[u8]) -> ParamSizes {
    fn find(v: &serde_json::Value, key: &str) -> Option<u64> {
        match v {
            serde_json::Value::Object(map) => map
                .get(key)
                .and_then(|x| x.as_u64().or_else(|| x.as_str()?.trim().parse().ok()))
                .or_else(|| map.values().find_map(|x| find(x, key))),
            serde_json::Value::Array(items) => items.iter().find_map(|x| find(x, key)),
            _ => None,
        }
    }
    match crate::source::parse_param_json(param_json) {
        Some(json) => ParamSizes {
            ac_gib: find(&json, "acSizeInGib"),
            app_gib: find(&json, "appSizeInGib"),
        },
        None => ParamSizes::default(),
    }
}

/// The firmware lines with a size check, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// 5.50 to 6.xx.
    Fw550,
    /// 7.00 to 10.xx.
    Fw700,
    /// 11.00 to 12.xx.
    Fw1100,
    /// 13.00 and later.
    Fw1300,
}

impl Band {
    /// The firmware line in words, as the messages name it.
    pub fn describe(self) -> &'static str {
        match self {
            Band::Fw550 => "5.50 to 6.xx",
            Band::Fw700 => "7.xx to 10.xx",
            Band::Fw1100 => "11.xx and 12.xx",
            Band::Fw1300 => "13.x",
        }
    }
}

/// The largest fake package one firmware line opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Limit {
    pub band: Band,
    /// The ceiling the firmware checks, in bytes: 450 GiB on 13.x.
    pub cap: u64,
    /// The add-on space counted against it (13.x only), in bytes.
    pub reserved: u64,
    /// The largest package that fits: `cap - reserved`.
    pub max_package: u64,
}

/// 242.551 GiB, the largest 5.50 to 6.50 threshold (the value `sdk_rules` declares as the
/// level-2 size class).
const FW550_CAP: u64 = 260_436_918_272;

/// The ceiling for a console on `fw`, `None` below 5.50 (no size check is known there).
pub fn limit_for(fw: Firmware, sizes: &ParamSizes) -> Option<Limit> {
    let at = |band: Band, cap: u64, reserved: u64| Limit {
        band,
        cap,
        reserved,
        max_package: cap.saturating_sub(reserved),
    };
    Some(match (fw.major, fw.minor) {
        (0..=4, _) | (5, 0..=49) => return None,
        (5..=6, _) => at(Band::Fw550, FW550_CAP, 0),
        (7..=10, _) => at(Band::Fw700, 256 * GIB, 0),
        (11..=12, _) => {
            let profile = sizes.app_gib.filter(|g| (256..=320).contains(g));
            at(Band::Fw1100, profile.unwrap_or(256) * GIB, 0)
        }
        _ => at(
            Band::Fw1300,
            450 * GIB,
            sizes.ac_gib.unwrap_or(0).saturating_mul(GIB),
        ),
    })
}

/// Which console a check is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The firmware the user asked the package for.
    Target,
    /// The console the app is connected to.
    Console,
    /// The game itself needs 13.00 or later, so only that line's ceiling applies.
    Game,
    /// None known: 13.x's ceiling, with a note for older firmware.
    Assumed,
}

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Fit {
    pub package: u64,
    /// The firmware the ceiling is for, and why that one.
    pub firmware: Option<Firmware>,
    pub basis: Basis,
    /// `None` when that firmware checks no size.
    pub limit: Option<Limit>,
    /// The refusal, when the package is past the ceiling.
    pub refusal: Option<String>,
    /// A firmware it would not open on although it fits the one checked.
    pub note: Option<String>,
}

impl Fit {
    pub fn fits(&self) -> bool {
        self.refusal.is_none()
    }
}

/// `n` bytes in GiB as the messages show it: `580`, `216.4`.
pub fn gib(n: u64) -> String {
    let v = n as f64 / GIB as f64;
    let text = format!("{v:.1}");
    text.strip_suffix(".0").map(str::to_string).unwrap_or(text)
}

/// Check a `package` of that many bytes (`estimated`: from a sample, not yet written). The
/// firmware it is for: the user's `target`, else the connected `console`'s, else the game's own
/// when that is 13.00 or later, else 13.x's ceiling with a note for older firmware.
pub fn check(
    package: u64,
    estimated: bool,
    target: Option<Firmware>,
    console: Option<Firmware>,
    required: Option<Firmware>,
    sizes: &ParamSizes,
) -> Fit {
    let fw13 = Firmware::new(13, 0);
    let (firmware, basis) = match (target, console) {
        (Some(t), _) => (t, Basis::Target),
        (None, Some(c)) => (c, Basis::Console),
        (None, None) => match required.filter(|r| *r >= fw13) {
            Some(r) => (r, Basis::Game),
            None => (fw13, Basis::Assumed),
        },
    };
    let limit = limit_for(firmware, sizes);
    let about = if estimated { "about " } else { "" };
    let refusal = limit.filter(|l| package > l.max_package).map(|l| {
        let reserved = if l.reserved > 0 {
            format!(
                " including the add-on space this game reserves ({} GiB)",
                gib(l.reserved)
            )
        } else {
            String::new()
        };
        format!(
            "This package would be {about}{} GiB. PS5 firmware {} can't open a fake package \
             larger than {} GiB{reserved}: it would install but never start. Keep it as a game \
             image instead, or use the original package.",
            gib(package),
            l.band.describe(),
            gib(l.cap),
        )
    });
    // No console named: 13.x's ceiling is the check, and older lines are lower.
    let note = (basis == Basis::Assumed && refusal.is_none() && package > 256 * GIB).then(|| {
        let older = match sizes.app_gib.filter(|g| (257..=320).contains(g)) {
            Some(app) if package <= app * GIB => "7.xx to 10.xx (256 GiB at most)".to_string(),
            Some(app) => format!(
                "7.xx to 12.xx (256 GiB at most; {app} GiB for this game on 11.xx and 12.xx)"
            ),
            None => "7.xx to 12.xx (256 GiB at most)".to_string(),
        };
        format!(
            "At {about}{} GiB, this package opens on PS5 firmware 13.x but not on {older} or \
             5.50 to 6.xx.",
            gib(package)
        )
    });
    Fit {
        package,
        firmware: Some(firmware),
        basis,
        limit,
        refusal,
        note,
    }
}

/// What the container adds per 64 KiB block of the outer image: its digest tables and the
/// PlayGo checksums. Measured: two builds of one game, 16,347 blocks apart, differed by 68 bytes
/// a block. The container's own files (artwork, trophies) come on top, tens of megabytes at most.
pub fn container_allowance(outer_blocks: u64) -> u64 {
    72 * outer_blocks
}

/// The firmware a build checks its package against (see [`check`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SizeTarget {
    /// The firmware the user asked the package for.
    pub target: Option<Firmware>,
    /// The connected console's.
    pub console: Option<Firmware>,
}

/// The layout index's length for `plan` stored in an image of `image_len` bytes: exact for a
/// block (Kraken) build once its image length is known, and an estimate before.
pub fn layout_index_bytes(plan: &Plan, image_len: u64) -> u64 {
    kraken_image::layout_len(
        plan.afid_order.len() as u64,
        plan.ndblock * crate::BLOCK,
        image_len,
        kraken_image::block_count(plan),
    )
}

/// A warning for a layout index past [`LARGEST_MEASURED_INDEX`], `None` below it.
pub fn index_note(bytes: u64) -> Option<String> {
    (bytes > LARGEST_MEASURED_INDEX).then(|| {
        format!(
            "this package's layout index is {:.1} MiB, more than any package measured so far \
             ({:.1} MiB). The PS5 loads it whole before it starts the game; if the game doesn't \
             start, keep it as a game image instead.",
            bytes as f64 / (1u64 << 20) as f64,
            LARGEST_MEASURED_INDEX as f64 / (1u64 << 20) as f64,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn fw(major: u8, minor: u8) -> Firmware {
        Firmware::new(major, minor)
    }

    fn sizes(ac: Option<u64>, app: Option<u64>) -> ParamSizes {
        ParamSizes {
            ac_gib: ac,
            app_gib: app,
        }
    }

    #[test]
    fn firmware_parses_every_way_it_is_written() {
        assert_eq!(Firmware::parse("13.60"), Some(fw(13, 60)));
        assert_eq!(Firmware::parse("05.10"), Some(fw(5, 10)));
        assert_eq!(Firmware::parse("9.6"), Some(fw(9, 60)));
        assert_eq!(Firmware::parse("0x1200000000000000"), Some(fw(12, 0)));
        assert_eq!(Firmware::parse("nonsense"), None);
        assert_eq!(fw(5, 10).to_string(), "5.10");
        assert!(fw(10, 60) < fw(11, 0));
    }

    #[test]
    fn param_sizes_are_found_wherever_they_sit() {
        let json = br#"{"kernel":{"appSizeInGib":320,"flexibleMemorySize":1},"addcont":{"acSizeInGib":64}}"#;
        assert_eq!(param_sizes(json), sizes(Some(64), Some(320)));
        assert_eq!(
            param_sizes(b"{\"titleId\":\"PPSA00001\"}"),
            sizes(None, None)
        );
        assert_eq!(param_sizes(b"not json"), sizes(None, None));
        assert_eq!(
            param_sizes(b"\xef\xbb\xbf{\"acSizeInGib\":\"16\"}"),
            sizes(Some(16), None)
        );
    }

    #[test]
    fn each_firmware_line_has_its_own_ceiling() {
        let none = sizes(None, None);
        assert_eq!(limit_for(fw(5, 10), &none), None, "5.10 checks no size");
        assert_eq!(limit_for(fw(4, 51), &none), None);
        for f in [fw(5, 50), fw(6, 50)] {
            let l = limit_for(f, &none).unwrap();
            assert_eq!((l.band, l.max_package), (Band::Fw550, 260_436_918_272));
        }
        for f in [fw(7, 0), fw(9, 60), fw(10, 60)] {
            let l = limit_for(f, &none).unwrap();
            assert_eq!((l.band, l.max_package), (Band::Fw700, 256 * GIB));
        }
        // 11.xx and 12.xx: the game's own profile from 256 to 320, else 256.
        for (app, cap) in [
            (None, 256),
            (Some(320), 320),
            (Some(300), 300),
            (Some(400), 256),
            (Some(100), 256),
        ] {
            let l = limit_for(fw(12, 0), &sizes(None, app)).unwrap();
            assert_eq!(
                (l.band, l.max_package),
                (Band::Fw1100, cap * GIB),
                "{app:?}"
            );
        }
        // 13.x: 450 GiB less the add-on space, which an older line doesn't count.
        let l = limit_for(fw(13, 60), &sizes(Some(64), Some(320))).unwrap();
        assert_eq!(
            (l.band, l.cap, l.reserved, l.max_package),
            (Band::Fw1300, 450 * GIB, 64 * GIB, 386 * GIB)
        );
        assert_eq!(limit_for(fw(13, 0), &none).unwrap().max_package, 450 * GIB);
        assert_eq!(
            limit_for(fw(9, 0), &sizes(Some(64), None))
                .unwrap()
                .reserved,
            0
        );
    }

    #[test]
    fn a_package_past_the_ceiling_is_refused_with_the_numbers() {
        let s = sizes(Some(64), None);
        let fit = check(580 * GIB, false, Some(fw(13, 60)), None, None, &s);
        assert!(!fit.fits());
        assert_eq!(
            fit.refusal.as_deref(),
            Some(
                "This package would be 580 GiB. PS5 firmware 13.x can't open a fake package \
                 larger than 450 GiB including the add-on space this game reserves (64 GiB): it \
                 would install but never start. Keep it as a game image instead, or use the \
                 original package."
            )
        );
        // The add-on space is what pushes it over: 400 GiB alone fits 450, not 386.
        assert!(!check(400 * GIB, false, Some(fw(13, 0)), None, None, &s).fits());
        assert!(check(386 * GIB, false, Some(fw(13, 0)), None, None, &s).fits());
        assert!(check(
            400 * GIB,
            false,
            Some(fw(13, 0)),
            None,
            None,
            &sizes(None, None)
        )
        .fits());
        // A sampled size says so.
        let est = check(300 * GIB, true, Some(fw(9, 60)), None, None, &s);
        assert!(est
            .refusal
            .unwrap()
            .starts_with("This package would be about 300 GiB. PS5 firmware 7.xx to 10.xx"));
    }

    #[test]
    fn the_firmware_checked_follows_target_then_console_then_game() {
        let s = sizes(None, None);
        let size = 300 * GIB;
        // The user's target wins over the connected console.
        let fit = check(size, false, Some(fw(9, 0)), Some(fw(13, 60)), None, &s);
        assert_eq!((fit.basis, fit.firmware), (Basis::Target, Some(fw(9, 0))));
        assert!(!fit.fits());
        let fit = check(size, false, None, Some(fw(13, 60)), None, &s);
        assert_eq!(fit.basis, Basis::Console);
        assert!(fit.fits() && fit.note.is_none());
        let fit = check(size, false, None, Some(fw(5, 10)), None, &s);
        assert!(fit.fits() && fit.limit.is_none(), "5.10 checks no size");
        let fit = check(size, false, None, None, Some(fw(13, 20)), &s);
        assert_eq!(fit.basis, Basis::Game);
        assert!(fit.note.is_none());
        // Nothing known: 13.x, with a note for the older lines it would not open on.
        let fit = check(size, false, None, None, Some(fw(7, 0)), &s);
        assert_eq!((fit.basis, fit.firmware), (Basis::Assumed, Some(fw(13, 0))));
        assert!(fit.fits());
        let note = fit.note.unwrap();
        assert!(
            note.contains("not on 7.xx to 12.xx (256 GiB at most)"),
            "{note}"
        );
        assert!(check(200 * GIB, false, None, None, None, &s).note.is_none());
        // A game whose own profile covers it on 11.xx and 12.xx.
        let note = check(size, false, None, None, None, &sizes(None, Some(320)))
            .note
            .unwrap();
        assert!(note.contains("not on 7.xx to 10.xx"), "{note}");
    }

    #[test]
    fn sizes_read_as_the_messages_show_them() {
        assert_eq!(gib(580 * GIB), "580");
        assert_eq!(gib(216 * GIB + 400 * (1 << 20)), "216.4");
        assert_eq!(gib(FW550_CAP), "242.6");
    }

    #[test]
    fn a_layout_index_past_the_largest_measured_one_is_noted() {
        assert!(index_note(LARGEST_MEASURED_INDEX).is_none());
        let note = index_note(80 << 20).unwrap();
        assert!(
            note.contains("80.0 MiB") && note.contains("48.0 MiB"),
            "{note}"
        );
    }

    /// The layout index for a game shaped like the one in a user's report (133,000 files,
    /// 48 GiB): the plan and the format's arithmetic, no data. It stays a few megabytes, in line
    /// with Sony-built packages of the same shape (13.2 MiB for 166,720 files in 34.9 GiB).
    #[test]
    fn a_large_many_file_game_has_a_sane_layout_index() {
        let mut files = Vec::new();
        let total = 48 * GIB;
        let n = 133_000u64;
        // Mostly small files, a few large ones holding the bulk.
        let small = 64 * 1024;
        let big = (total - small * (n - 40)) / 40;
        for i in 0..n {
            files.push(crate::source::SourceFile {
                path: format!("data/d{:03}/f{i}.bin", i % 700),
                size: if i < 40 { big } else { small },
            });
        }
        files.push(crate::source::SourceFile {
            path: "eboot.bin".into(),
            size: 1 << 20,
        });
        let plan = crate::plan::build_tree(&files, &[], true).unwrap();
        // Stored (the largest image) and compressed to half.
        let stored = layout_index_bytes(&plan, plan.ndblock * crate::BLOCK);
        let half = layout_index_bytes(&plan, plan.ndblock * crate::BLOCK / 2);
        assert!(half < stored);
        assert!(stored < 12 << 20, "{stored}");
        assert!(index_note(stored).is_none());
        // Format arithmetic: 8 B per stored 64 KiB, at least 9 per 256 KiB block and per file.
        let floor = 8 * (48 * GIB / crate::BLOCK) + 9 * (48 * GIB / (256 * 1024)) + 15 * n;
        assert!(
            stored >= floor && stored < floor + floor / 5,
            "{stored} vs {floor}"
        );
    }
}
