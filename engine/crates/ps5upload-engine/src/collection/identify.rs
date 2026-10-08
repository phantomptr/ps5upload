//! What one item says about itself. Never the filename when the item can say: a package, an
//! image and a game folder each carry their own identity.

use std::io::Read;
use std::path::Path;

use ps5upload_pkg::kind::{
    classify, classify_category, newer_version, normalize_version, region_of_content_id,
};

use super::PkgInfo;
use crate::inspect::GameInspection;

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

fn package(path: &Path) -> PkgInfo {
    let mut magic = [0u8; 4];
    let Some((mut f, size)) = open_any(path) else {
        return failed("could not be read");
    };
    if f.read_exact(&mut magic).is_err() {
        return failed("could not be read");
    }
    if magic == ps5upload_pkg::ps3::PS3_MAGIC {
        return match ps5upload_pkg::ps3::parse_ps3(&mut f, size) {
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
                error: None,
            },
            Err(e) => failed(e),
        };
    }
    if std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0)).is_err() {
        return failed("could not be read");
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
        // the reader fills one in from the same header bits, which would check nothing.
        let category = if head.content_type == 0x1A || head.content_type == 0x1B {
            head.category.as_str()
        } else {
            ""
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
    let mut tree = match ps5_dump_forge_pfs::PfsSource::open(path) {
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

fn identify_raw(path: &Path, kind: &str) -> Option<PkgInfo> {
    match kind {
        "pkg" => Some(package(path)),
        "mount.ffpfs" => Some(pfs_image(path)),
        "folder" | "mount.exfat" | "mount.ffpkg" | "mount.ffpfsc" => {
            Some(match crate::inspect::inspect_local(path) {
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
