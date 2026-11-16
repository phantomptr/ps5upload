//! Why a source can't become what was asked: a PS4 update or add-on, a PS5 patch package.

use std::path::Path;

/// Why a PS4 source can't become what was asked, or `None`: an update alone holds only the
/// files it changed, so it goes into its game ("Merge into the game"); an add-on becomes a
/// folder, not an image. A licence-only add-on never opens (`ps4_pkg::LICENCE_ONLY`).
pub fn ps4_refusal(
    source: &Path,
    tree: &mut dyn ps5upload_fpkg::source::SourceTree,
    image: bool,
) -> Option<String> {
    use ps5upload_fpkg::ps4_pkg::{self, Ps4Kind};
    let (kind, title, version) = match ps4_pkg::peek_path(source).ok().flatten() {
        Some(info) => (
            Some(info.kind),
            info.title_id(),
            info.text("APP_VER").unwrap_or_default(),
        ),
        None => {
            let path = tree
                .files()
                .iter()
                .find(|f| f.path.eq_ignore_ascii_case("sce_sys/param.sfo"))
                .map(|f| f.path.clone())?;
            // Capped as every param file is: a tree that claims a huge one is not read whole.
            let bytes = ps5upload_fpkg::source::read_small(
                tree,
                &path,
                ps5upload_fpkg::source::MAX_PARAM_BYTES,
            )
            .ok()?;
            let params = ps5upload_pkg::sfo_params(&bytes).ok()?;
            let text = |k: &str| ps4_pkg::sfo_text(&params, k).unwrap_or_default();
            (
                Ps4Kind::of_category(&text("CATEGORY")),
                text("TITLE_ID"),
                text("APP_VER"),
            )
        }
    };
    match kind? {
        Ps4Kind::Patch => Some(format!(
            "{} is a PS4 update ({version}) for {title}. It holds only the files the update \
             changed, so on its own it isn't a game: merge it into the game instead (Convert's \
             \"Merge into the game\": the game as the source, this as its update)",
            source.display()
        )),
        Ps4Kind::Addon if image => Some(format!(
            "{} is a PS4 add-on: its files become a folder, not a game image",
            source.display()
        )),
        _ => None,
    }
}

/// Why a PS5 patch package can't become a game image or a package, or `None` for anything
/// else. As PS5 Dump Forge refuses one (`PATCH_SOURCE` in its `preflight.rs`): it holds only what
/// its update changed. Told by the file's first four bytes, before the open that reads all of it.
pub fn patch_refusal(source: &Path) -> Option<String> {
    let is_pkg = source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pkg"));
    (is_pkg && ps5upload_fpkg::lih::is_patch(source)).then(|| {
        format!(
            "{} is a patch package: it holds only the files its update changed, so on its own \
             it isn't a game and can't become an image or a package. Its files can still be \
             saved: unpack it to a folder, or open it in the image browser",
            source.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_fpkg::source::{SourceFile, SourceTree};

    /// A tree that lists a param.sfo far larger than any real one and fails the test if it is
    /// read whole.
    struct HugeSfo(Vec<SourceFile>);

    impl SourceTree for HugeSfo {
        fn files(&self) -> &[SourceFile] {
            &self.0
        }
        fn read(&mut self, path: &str) -> ps5upload_fpkg::Result<Vec<u8>> {
            panic!("{path} was read whole")
        }
        fn describe(&self) -> String {
            "huge".into()
        }
    }

    #[test]
    fn a_huge_param_sfo_is_not_read_whole() {
        let mut tree = HugeSfo(vec![SourceFile {
            path: "sce_sys/param.sfo".into(),
            size: 1 << 40,
        }]);
        assert_eq!(
            ps4_refusal(Path::new("/no/such/game"), &mut tree, true),
            None
        );
    }
}
