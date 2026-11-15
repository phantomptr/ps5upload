//! Which runtime a zstd-packed game carries, and whether it is one that starts it.
//!
//! A game in zstd asset packs (`ampr_zstd.index` at its root) is read through quer3q's
//! ampr-emu-ng in its `fakelib2/` or `fakelib/` (`libSceAmpr.sprx`). Releases before 0.3.4 hang
//! some games at startup (ASTRO BOT, quer3q/ampr-emu-ng#17: the runtime's read threads took the
//! room the game's own threads needed); drakmor's ampr_emu, which serves LZ4 packs, does not
//! read zstd packs at all. ampr-emu-ng releases are recognised by the SHA-256 GitHub publishes
//! for each release's `libSceAmpr.sprx`.

use serde::Serialize;

use crate::source::SourceTree;

/// The zstd pack index at a packed game's root (ps5-dump-forge's convention).
pub const ZSTD_INDEX: &str = "ampr_zstd.index";

/// ampr-emu-ng releases: version and the SHA-256 GitHub publishes for its `libSceAmpr.sprx`.
pub const NG_RELEASES: [(&str, &str); 7] = [
    (
        "0.4.0",
        "8320eb65c16565f42031b9d88b15a447039f0fd7e5694faf5f1cb84cafdba218",
    ),
    (
        "0.3.5",
        "998f2029b250d954725f864b57f2eee72aefbfb88054a3f1c3df550d4d6c8261",
    ),
    (
        "0.3.4",
        "7d1bb260f954b755ed687733e52ccfa38c31440a9b88a6fbba5b3b75e08bfa26",
    ),
    (
        "0.3.3",
        "1c1051f91bc7cba19b15362b2b832114b53a4c3d6193f50c90fdf1ae5d78d268",
    ),
    (
        "0.3.2",
        "9a8f553a68d4e6e6be11bd9f8d5519b5faa68560761881966abff1dd65bf19f7",
    ),
    (
        "0.3.1",
        "bdd93ce2fbcc2d0986b16d77604cc0770d994ccc124b9051ba9f53aeac39f56f",
    ),
    (
        "0.3.0",
        "5f39ca8ebe169aa15d8a8ba8ec2f9cd4195f509d55daaf4b4edbfa80b145035c",
    ),
];

/// The first ampr-emu-ng release with the thread fixes that made the games which hung at
/// startup (ASTRO BOT) start.
pub const NG_FIXED: &str = "0.3.4";

/// What runs a zstd-packed game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZstdRuntime {
    /// The emulator's path in the game (`fakelib2/…` or `fakelib/…`); `None` when it has none.
    pub module: Option<String>,
    /// `ampr-emu-ng`; `ampr_emu` (drakmor's, which doesn't read zstd packs); `unknown` (a
    /// build this app doesn't know, a test build or one newer than it); `missing`.
    pub kind: &'static str,
    /// The release, when known.
    pub version: Option<String>,
    /// The game won't start or may hang with it: missing, drakmor's, or ampr-emu-ng older than
    /// [`NG_FIXED`]. A newer emulator fixes it.
    pub outdated: bool,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The ampr-emu-ng release `module` is, by its digest.
pub fn ng_version(module: &[u8]) -> Option<&'static str> {
    let sha = sha256_hex(module);
    NG_RELEASES.iter().find(|(_, s)| *s == sha).map(|(v, _)| *v)
}

/// `a` < `b`, compared as dotted numbers.
pub fn older(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Vec<u64> { s.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a) < parts(b)
}

/// What a `libSceAmpr.sprx` is, for a game in zstd packs.
pub fn classify(module: &[u8]) -> (&'static str, Option<String>, bool) {
    if let Some(v) = ng_version(module) {
        return ("ampr-emu-ng", Some(v.to_string()), older(v, NG_FIXED));
    }
    let drakmor = super::runtime_support(module);
    if drakmor.version.is_some() {
        return ("ampr_emu", drakmor.version, true);
    }
    ("unknown", None, false)
}

/// Whether `tree` is a game in zstd packs.
pub fn is_zstd_packed(tree: &dyn SourceTree) -> bool {
    tree.files()
        .iter()
        .any(|f| f.path.eq_ignore_ascii_case(ZSTD_INDEX))
}

/// The emulator a game loads: `fakelib2/` wins over `fakelib/` (ShadowMount+ mounts `fakelib2`
/// on its own).
pub fn module_path(tree: &dyn SourceTree) -> Option<String> {
    for dir in ["fakelib2/", "fakelib/"] {
        if let Some(f) = tree.files().iter().find(|f| {
            f.path.len() == dir.len() + "libSceAmpr.sprx".len()
                && f.path[..dir.len()].eq_ignore_ascii_case(dir)
                && f.path[dir.len()..].eq_ignore_ascii_case("libSceAmpr.sprx")
        }) {
            return Some(f.path.clone());
        }
    }
    None
}

/// For a game in zstd packs, the runtime it carries; `None` for any other game.
pub fn zstd_runtime(tree: &mut dyn SourceTree) -> Option<ZstdRuntime> {
    if !is_zstd_packed(tree) {
        return None;
    }
    let Some(module) = module_path(tree) else {
        return Some(ZstdRuntime {
            module: None,
            kind: "missing",
            version: None,
            outdated: true,
        });
    };
    let (kind, version, outdated) = match crate::source::read_small(tree, &module, 64 << 20) {
        Ok(bytes) => classify(&bytes),
        Err(_) => ("unknown", None, false),
    };
    Some(ZstdRuntime {
        module: Some(module),
        kind,
        version,
        outdated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(older("0.3.2", "0.3.4"));
        assert!(older("0.3.10", "0.4.0"));
        assert!(!older("0.3.4", "0.3.4"));
        assert!(!older("0.4.0", "0.3.4"));
        assert!(older("0.3", "0.3.4"));
    }

    #[test]
    fn every_release_before_the_thread_fixes_is_outdated() {
        for (v, _) in NG_RELEASES {
            assert_eq!(
                older(v, NG_FIXED),
                ["0.3.3", "0.3.2", "0.3.1", "0.3.0"].contains(&v),
                "{v}"
            );
        }
    }

    #[test]
    fn drakmors_emulator_does_not_run_zstd_packs() {
        let mut m = b"\0\0ampr_emu 0.4.2.1 (c) Drakmor\0".to_vec();
        m.extend_from_slice(&[0u8; 32]);
        let (kind, version, outdated) = classify(&m);
        assert_eq!(
            (kind, version.as_deref(), outdated),
            ("ampr_emu", Some("0.4.2.1"), true)
        );
    }

    #[test]
    fn an_unknown_build_is_left_alone() {
        assert_eq!(classify(b"something else"), ("unknown", None, false));
    }
}
