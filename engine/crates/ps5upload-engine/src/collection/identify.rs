//! What one item says about itself. Never the filename when the item can say: a package, an
//! image and a game folder each carry their own identity.

use std::io::Read;
use std::path::Path;

use ps5upload_pkg::kind::{
    classify, classify_category, newer_version, normalize_version, region_of_content_id,
};

use super::PkgInfo;
use crate::inspect::GameInspection;

/// Why an item has no identity when it could not be opened or read at all: a dropped connection
/// says nothing about the file, so this answer is not kept.
pub const READ_FAILED: &str = "could not be read";

/// What the index records for a later part of a split set: not an item, and not a package.
pub const SPLIT_PART: &str = "part of a split package";

fn failed(why: impl Into<String>) -> PkgInfo {
    PkgInfo {
        error: Some(why.into()),
        complete: true,
        ..PkgInfo::default()
    }
}

/// The version a source delivers. A PS4 PARAM.SFO raises APP_VER in a patch but leaves
/// VERSION at the release, while some base games do the opposite, so the higher of the two;
/// a PS5 param.json says `contentVersion`.
fn version_of(g: &GameInspection) -> String {
    let v = if g.identity.platform.eq_ignore_ascii_case("ps4") {
        newer_version(
            &g.specs.app_ver,
            g.specs.master_ver.as_deref().unwrap_or(""),
        )
    } else {
        g.specs.app_ver.clone()
    };
    normalize_version(&v)
}

fn from_inspection(g: &GameInspection) -> PkgInfo {
    let (kind, confident, reason) = classify_category(&g.identity.category);
    PkgInfo {
        platform: g.identity.platform.to_ascii_uppercase(),
        title: g.identity.title.clone(),
        title_id: g.identity.title_id.to_ascii_uppercase(),
        content_id: g.identity.content_id.clone(),
        version: version_of(g),
        region: g.identity.region.clone().unwrap_or_default(),
        kind: kind.as_str().to_string(),
        kind_confident: confident,
        kind_reason: reason,
        complete: true,
        min_fw: g.specs.min_fw.clone().unwrap_or_default(),
        error: None,
    }
}

/// A `.pkg`: PS3 from its own header; PS4/PS5 from the container header (kind) and its PARAM
/// (title, version).
/// The file's bytes and size: this computer's disk, or a saved server (`remote://`) through
/// the same opener Convert and the package viewer use.
fn open_any(path: &Path) -> Option<(Box<dyn ps5upload_fpkg::ReadSeek>, u64)> {
    if ps5upload_fpkg::remote_source::is_remote(path) {
        return ps5upload_fpkg::remote_source::open_file(path).ok();
    }
    let f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    Some((Box::new(f), size))
}

fn package(path: &Path, set_size: Option<u64>) -> PkgInfo {
    let mut magic = [0u8; 4];
    let Some((mut f, size)) = open_any(path) else {
        return failed(READ_FAILED);
    };
    if f.read_exact(&mut magic).is_err() {
        return failed(READ_FAILED);
    }
    if magic == ps5upload_pkg::ps3::PS3_MAGIC {
        // "Complete" is the whole set's length against what the header declares.
        return match ps5upload_pkg::ps3::parse_ps3(&mut f, set_size.unwrap_or(size)) {
            Ok(p) => PkgInfo {
                platform: "PS3".into(),
                title: p.title,
                title_id: p.title_id,
                content_id: p.content_id,
                version: p.version,
                region: p.region,
                kind: p.kind.as_str().into(),
                kind_confident: p.kind_confident,
                kind_reason: p.kind_reason,
                complete: p.complete,
                min_fw: String::new(),
                error: None,
            },
            Err(e) => failed(e),
        };
    }
    if std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0)).is_err() {
        return failed(READ_FAILED);
    }
    let head = match ps5upload_pkg::parse_pkg_from(&mut f, size, path) {
        Ok(h) => h,
        Err(e) => return failed(e.to_string()),
    };
    let mut info = match crate::inspect::inspect_local(path) {
        Ok(g) => from_inspection(&g),
        Err(_) => PkgInfo {
            title: head.title.clone(),
            title_id: head.title_id.clone(),
            content_id: head.content_id.clone(),
            version: normalize_version(&head.app_ver),
            region: region_of_content_id(&head.content_id),
            complete: true,
            ..PkgInfo::default()
        },
    };
    if head.content_type != 0 {
        // Only a PS4 package carries a PARAM.SFO CATEGORY to cross-check; for a PS5 package
        // the reader fills one in from the same header bits, which would check nothing. Every
        // PS4 type gets it, a DLC-only licence (0x1C) and a type not known yet included: the
        // CATEGORY is what tells an unknown type's DLC from its game (#638).
        let category = if head.platform.eq_ignore_ascii_case("ps5")
            || ps5upload_pkg::kind::content_type_platform(head.content_type) == Some("PS5")
        {
            ""
        } else {
            head.category.as_str()
        };
        let (kind, confident, reason) = classify(head.content_type, head.content_flags, category);
        info.kind = kind.as_str().into();
        info.kind_confident = confident;
        info.kind_reason = reason;
        if let Some(p) = ps5upload_pkg::kind::content_type_platform(head.content_type) {
            info.platform = p.into();
        }
    }
    if info.platform.is_empty() {
        info.platform = head.platform.to_ascii_uppercase();
    }
    if info.content_id.is_empty() {
        info.content_id = head.content_id.clone();
    }
    if info.title_id.is_empty() {
        info.title_id = if head.title_id.is_empty() {
            ps5upload_pkg::kind::title_id_of_content_id(&head.content_id)
        } else {
            head.title_id.clone()
        };
    }
    // A package shorter than its header declares is still being copied, or was cut short.
    if head
        .warnings
        .iter()
        .any(|w| w.contains("truncat") || w.contains("shorter"))
    {
        info.complete = false;
    }
    info
}

/// A `.ffpfs` image: PS5 games keep `sce_sys/param.json`.
fn pfs_image(path: &Path) -> PkgInfo {
    use ps5upload_fpkg::source::SourceTree;
    let Some((reader, _)) = open_any(path) else {
        return failed(READ_FAILED);
    };
    let mut tree =
        match ps5_dump_forge_pfs::PfsSource::from_reader(reader, path.display().to_string()) {
            Ok(t) => t,
            Err(e) => return failed(e.to_string()),
        };
    let Ok(bytes) = tree.read("sce_sys/param.json") else {
        return failed("no sce_sys/param.json in this image");
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return failed("sce_sys/param.json is not JSON");
    };
    let mut g = GameInspection::default();
    crate::inspect::apply_param_json(&mut g, &v);
    crate::inspect::finish(&mut g);
    from_inspection(&g)
}

/// What the item at `path` (of discovery type `kind`) says about itself. `None` for an archive,
/// which is identified by the Game ID in its name.
pub fn identify(path: &Path, kind: &str) -> Option<PkgInfo> {
    let mut info = identify_raw(path, kind)?;
    // The source app's region: the content ID's prefix by name ("Americas", "Europe", …), or
    // the prefix itself ("IV") when it names no region. Exports and consumers expect it.
    if !info.content_id.is_empty() {
        info.region = region_of_content_id(&info.content_id);
    }
    Some(info)
}

/// What a split package says about itself: its first part names it; whether it is whole is the
/// set's (`total` bytes, and `gap` when a part is missing from the run).
pub fn identify_set(lead: &Path, total: u64, gap: bool) -> PkgInfo {
    let mut info = package(lead, Some(total));
    if !info.content_id.is_empty() {
        info.region = region_of_content_id(&info.content_id);
    }
    if gap {
        info.complete = false;
        info.kind_reason = "a part of this split package is missing".into();
    }
    info
}

fn identify_raw(path: &Path, kind: &str) -> Option<PkgInfo> {
    match kind {
        "pkg" => Some(package(path, None)),
        "mount.ffpfs" => Some(pfs_image(path)),
        "folder" | "mount.exfat" | "mount.ffpkg" | "mount.ffpfsc" => {
            Some(match crate::inspect::inspect_local(path) {
                Ok(g) if g.partial && g.identity.title_id.is_empty() => {
                    failed("no readable sce_sys/param.json or param.sfo")
                }
                Ok(g) => from_inspection(&g),
                Err(e) => failed(format!("{e:#}")),
            })
        }
        _ => None,
    }
}

/// The cover (`icon0.png`) of an item, when it has one.
pub fn cover(path: &Path, kind: &str) -> Option<Vec<u8>> {
    match kind {
        "pkg" => crate::inspect::read_image(path, "pkg", "icon0.png").ok(),
        "mount.ffpfs" => {
            use ps5upload_fpkg::source::SourceTree;
            let mut t = ps5_dump_forge_pfs::PfsSource::open(path).ok()?;
            t.read("sce_sys/icon0.png").ok()
        }
        "folder" => crate::inspect::read_image(path, "folder", "icon0.png").ok(),
        k if k.starts_with("mount.") => {
            crate::inspect::read_image(path, k.trim_start_matches("mount."), "icon0.png").ok()
        }
        _ => None,
    }
    .filter(|b| !b.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::collection::group::{finish, place, summarize};
    use crate::collection::Location;
    use crate::inspect::test_fixtures::{scratch, sfo};

    const GAME: &str = "EP0700-CUSA12345_00-GAME000000000000";

    /// A PS4 `\x7FCNT` package: its header's content ID, `content_type` and `content_flags`,
    /// and a PARAM.SFO with `category` unless that is `None`.
    fn ps4_pkg(
        dir: &Path,
        name: &str,
        cid: &str,
        content_type: u32,
        flags: u32,
        category: Option<&str>,
    ) -> PathBuf {
        let table = 0x100u32;
        let entries: u32 = if category.is_some() { 1 } else { 0 };
        // A package with no PARAM.SFO still has an entry (here an empty one at id 0x0001).
        let data_off = table + 0x20;
        let mut pkg = vec![0u8; data_off as usize];
        pkg[0..4].copy_from_slice(&ps5upload_pkg::PKG_MAGIC.to_be_bytes());
        pkg[0x10..0x14].copy_from_slice(&1u32.to_be_bytes());
        pkg[0x18..0x1C].copy_from_slice(&table.to_be_bytes());
        pkg[0x40..0x40 + cid.len()].copy_from_slice(cid.as_bytes());
        pkg[0x74..0x78].copy_from_slice(&content_type.to_be_bytes());
        pkg[0x78..0x7C].copy_from_slice(&flags.to_be_bytes());
        let e = table as usize;
        let body = match category {
            Some(c) => sfo(&[("CATEGORY", c), ("CONTENT_ID", cid), ("TITLE", "Some Game")]),
            None => vec![0u8; 0x10],
        };
        let id: u32 = if entries == 1 { 0x1000 } else { 0x0001 };
        pkg[e..e + 4].copy_from_slice(&id.to_be_bytes());
        pkg[e + 0x10..e + 0x14].copy_from_slice(&data_off.to_be_bytes());
        pkg[e + 0x14..e + 0x18].copy_from_slice(&(body.len() as u32).to_be_bytes());
        pkg.extend_from_slice(&body);
        pkg.resize(pkg.len().max(0x200), 0);
        let p = dir.join(name);
        std::fs::write(&p, &pkg).unwrap();
        p
    }

    fn kind_of(p: &Path) -> PkgInfo {
        identify(p, "pkg").unwrap()
    }

    #[test]
    fn every_ps4_package_shape_is_named_by_what_it_is() {
        let d = scratch();
        let game = kind_of(&ps4_pkg(&d, "g.pkg", GAME, 0x1A, 0x0A00_0000, Some("gd")));
        assert_eq!((game.kind.as_str(), game.kind_confident), ("base", true));
        assert_eq!(
            (game.title_id.as_str(), game.platform.as_str()),
            ("CUSA12345", "PS4")
        );
        let patch = kind_of(&ps4_pkg(&d, "u.pkg", GAME, 0x1A, 0x6210_0000, Some("gp")));
        assert_eq!((patch.kind.as_str(), patch.kind_confident), ("patch", true));
        let ac = "EP0700-CUSA12345_00-SEASONPASS000001";
        let dlc = kind_of(&ps4_pkg(&d, "d.pkg", ac, 0x1B, 0, Some("ac")));
        assert_eq!((dlc.kind.as_str(), dlc.kind_confident), ("dlc", true));
        // #638: a DLC that is only a licence (AL, 0x1C), with and without a PARAM.SFO.
        let al = "EP0700-CUSA12345_00-UNLOCKKEY0000002";
        let unlock = kind_of(&ps4_pkg(&d, "k.pkg", al, 0x1C, 0, Some("ac")));
        assert_eq!((unlock.kind.as_str(), unlock.kind_confident), ("dlc", true));
        assert_eq!(unlock.platform, "PS4");
        let bare = kind_of(&ps4_pkg(&d, "b.pkg", al, 0x1C, 0, None));
        assert_eq!(
            (bare.kind.as_str(), bare.title_id.as_str()),
            ("dlc", "CUSA12345")
        );
        let bare_ac = kind_of(&ps4_pkg(&d, "b2.pkg", ac, 0x1B, 0, None));
        assert_eq!(bare_ac.kind, "dlc");
        // A header that says "game" while its PARAM.SFO says add-on is an add-on.
        let odd = kind_of(&ps4_pkg(&d, "o.pkg", ac, 0x1A, 0, Some("ac")));
        assert_eq!((odd.kind.as_str(), odd.kind_confident), ("dlc", false));
    }

    #[test]
    fn a_game_with_all_its_dlc_is_one_copy_and_the_game_twice_is_a_duplicate() {
        let d = scratch();
        let shapes = [
            ("game.pkg", GAME, 0x1A, 0x0A00_0000, Some("gd")),
            ("update.pkg", GAME, 0x1A, 0x6210_0000, Some("gp")),
            (
                "dlc1.pkg",
                "EP0700-CUSA12345_00-SEASONPASS000001",
                0x1B,
                0,
                Some("ac"),
            ),
            (
                "dlc2.pkg",
                "EP0700-CUSA12345_00-COSTUMEPACK00002",
                0x1B,
                0,
                None,
            ),
            (
                "dlc3.pkg",
                "EP0700-CUSA12345_00-UNLOCKKEY0000003",
                0x1C,
                0,
                Some("ac"),
            ),
            (
                "dlc4.pkg",
                "EP0700-CUSA12345_00-UNLOCKKEY0000004",
                0x1C,
                0,
                None,
            ),
        ];
        let mut games = BTreeMap::new();
        let add = |games: &mut BTreeMap<_, _>, p: PathBuf| {
            let info = kind_of(&p);
            let loc = Location {
                path: p.display().to_string(),
                kind: "pkg".into(),
                size_bytes: std::fs::metadata(&p).unwrap().len(),
                pkg: Some(info.clone()),
                ..Location::default()
            };
            place(games, &info.title_id, loc);
        };
        for (name, cid, ct, flags, cat) in shapes {
            add(&mut games, ps4_pkg(&d, name, cid, ct, flags, cat));
        }
        finish(games.get_mut("CUSA12345").unwrap());
        let g = &games["CUSA12345"];
        assert_eq!(g.locations.len(), 6);
        assert_eq!((g.copies, g.is_duplicate), (1, false));
        assert_eq!(summarize(&games).duplicates_count, 0);

        // The same game stored a second time is a duplicate.
        add(
            &mut games,
            ps4_pkg(&d, "game-again.pkg", GAME, 0x1A, 0x0A00_0000, Some("gd")),
        );
        finish(games.get_mut("CUSA12345").unwrap());
        let g = &games["CUSA12345"];
        assert_eq!((g.copies, g.is_duplicate), (2, true));
        assert_eq!(summarize(&games).duplicates_count, 1);
    }
}
