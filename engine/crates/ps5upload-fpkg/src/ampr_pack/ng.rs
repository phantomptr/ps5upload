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
pub const NG_RELEASES: [(&str, &str); 10] = [
    (
        "0.5.1",
        "7ca61afc444f0b44d2f005506dde16cb43e74dae4b1e045323b291b8db1c3d5d",
    ),
    (
        "0.5.0",
        "7379e49953d8f14f26b45ff06a863ecea56aa90a60ba3d5a375f61c52618a174",
    ),
    (
        "0.4.1",
        "9c7c41b3216bc4f877d67e3c2e5c9c8f9ffabdd5065f4f3fe0dd98fa7228386f",
    ),
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

/// The one ampr-emu-ng release that refuses very large games: it holds its lookup tables to
/// 48 MiB and leaves the packs unserved past that ("index tables need 64MiB, over the 48 MiB
/// beside the page cache; packed files not served", `rt/index_memory.rs` `INDEX_LOOKUP_MAX` in
/// v0.4.0). 0.4.1 has a new file index with no fixed cap.
pub const NG_CAPPED: &str = "0.4.0";

/// Whether an emulator of `kind` and `version` is the ampr-emu-ng release with the 48 MiB
/// index cap ([`NG_CAPPED`]).
pub fn capped(kind: &str, version: Option<&str>) -> bool {
    kind == "ampr-emu-ng" && version == Some(NG_CAPPED)
}

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
    /// It is ampr-emu-ng [`NG_CAPPED`], which starts most games and refuses very large ones:
    /// a newer one is worth offering, though the game may well run as it is.
    pub capped: bool,
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
        let want = format!("{dir}libSceAmpr.sprx");
        if let Some(f) = tree
            .files()
            .iter()
            .find(|f| f.path.eq_ignore_ascii_case(&want))
        {
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
            capped: false,
        });
    };
    let (kind, version, outdated) = match crate::source::read_small(tree, &module, 64 << 20) {
        Ok(bytes) => classify(&bytes),
        Err(_) => ("unknown", None, false),
    };
    Some(ZstdRuntime {
        module: Some(module),
        kind,
        capped: capped(kind, version.as_deref()),
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

    /// Newest first, each digest a SHA-256 in lower-case hex, no release or digest twice.
    #[test]
    fn the_release_table_is_newest_first_and_unique() {
        for pair in NG_RELEASES.windows(2) {
            assert!(
                older(pair[1].0, pair[0].0),
                "{} before {}",
                pair[0].0,
                pair[1].0
            );
        }
        for (v, sha) in NG_RELEASES {
            assert_eq!(sha.len(), 64, "{v}");
            assert!(sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            assert_eq!(
                NG_RELEASES.iter().filter(|(_, s)| *s == sha).count(),
                1,
                "{v}"
            );
        }
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

    /// Only 0.4.0 has the index cap: older releases hang instead, newer ones have no cap.
    #[test]
    fn only_the_capped_release_is_capped() {
        for (v, _) in NG_RELEASES {
            assert_eq!(capped("ampr-emu-ng", Some(v)), v == "0.4.0", "{v}");
        }
        assert!(!capped("ampr_emu", Some("0.4.0")));
        assert!(!capped("unknown", None));
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

    struct Names(Vec<crate::source::SourceFile>);
    impl SourceTree for Names {
        fn files(&self) -> &[crate::source::SourceFile] {
            &self.0
        }
        fn read(&mut self, path: &str) -> crate::Result<Vec<u8>> {
            Err(crate::Error::Format(format!("no {path}")))
        }
        fn describe(&self) -> String {
            "names".into()
        }
    }

    /// A name as long as the module's path with a character across the folder's length: found
    /// by comparing names, never by cutting one.
    #[test]
    fn a_non_ascii_name_of_the_same_length_is_not_the_module() {
        let file = |path: &str| crate::source::SourceFile {
            path: path.into(),
            size: 1,
        };
        let odd = "データ/abcdefghijklm";
        assert_eq!(odd.len(), "fakelib/libSceAmpr.sprx".len());
        let tree = Names(vec![file(odd), file("FakeLib/libsceampr.SPRX")]);
        assert_eq!(
            module_path(&tree).as_deref(),
            Some("FakeLib/libsceampr.SPRX")
        );
        assert_eq!(module_path(&Names(vec![file(odd)])), None);
    }

    #[test]
    fn an_unknown_build_is_left_alone() {
        assert_eq!(classify(b"something else"), ("unknown", None, false));
    }
}
