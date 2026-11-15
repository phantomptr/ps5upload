//! POST /api/image/browse (#479): what is inside a game image or debug package on this
//! computer, one folder at a time, for the Local image screen's browser.
//!
//! Every format the converter reads is browsed through the same reader it uses
//! (`ps5upload_fpkg::browse`). Saving a file or folder of the image runs as a folder build
//! (`/api/folder/build` with `entry`); an edited copy as an image build (`/api/exfat/build`
//! with `edits`). This route only reads.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use ps5upload_fpkg::browse::{self, Filesystem, Index};
use serde::{Deserialize, Serialize};

use crate::json_err;

#[derive(Deserialize)]
pub(crate) struct BrowseReq {
    /// The image or package, on this computer.
    path: String,
    /// The folder inside it to list; the root when absent.
    #[serde(default)]
    dir: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct BrowseEntry {
    name: String,
    is_dir: bool,
    size: u64,
    files: u64,
}

#[derive(Serialize, Debug)]
pub(crate) struct BrowseResp {
    /// `exfat`, `ufs2`, `pfs` or `pkg`: the filesystem inside (a `.ffpfsc`'s inner one).
    filesystem: &'static str,
    /// A `.ffpfsc`: the image sits in a compressed container.
    compressed: bool,
    /// An edited copy can be written (every image kind but a package).
    editable: bool,
    /// Changes can also go into the image itself (`/api/image/edit/check` says whether a given
    /// set fits): an uncompressed exFAT, UFS2 or PFS image.
    in_place: bool,
    /// The folder listed, normalized ("" at the root).
    dir: String,
    entries: Vec<BrowseEntry>,
    total_files: u64,
    total_bytes: u64,
    /// The reader's one-line description of the image.
    label: String,
}

/// One image's listing, kept while the file is unchanged: opening a large image walks its
/// whole directory tree, and a user clicking through folders should not pay that each time.
struct Cached {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    filesystem: Filesystem,
    compressed: bool,
    label: String,
    index: Arc<Index>,
}

fn cache() -> &'static Mutex<Option<Cached>> {
    static CACHE: std::sync::OnceLock<Mutex<Option<Cached>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The listing of the image at `path` (absolute), from the cache when the file is unchanged.
fn indexed(path: &Path) -> Result<(Arc<Index>, Filesystem, bool, String), String> {
    // Not while an in-place edit writes it; a journal an interrupted one left is settled first.
    crate::image_edit_in_place::before_open(path)?;
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let (len, modified) = (meta.len(), meta.modified().ok());
    {
        let g = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = g.as_ref() {
            if c.path == path && c.len == len && c.modified == modified {
                return Ok((c.index.clone(), c.filesystem, c.compressed, c.label.clone()));
            }
        }
    }
    let opened = browse::open(path).map_err(|e| e.to_string())?;
    let index = Arc::new(Index::new(&*opened.tree));
    let label = opened.tree.describe();
    *cache().lock().unwrap_or_else(|e| e.into_inner()) = Some(Cached {
        path: path.to_path_buf(),
        len,
        modified,
        filesystem: opened.filesystem,
        compressed: opened.compressed,
        label: label.clone(),
        index: index.clone(),
    });
    Ok((index, opened.filesystem, opened.compressed, label))
}

/// Drops the cached listing of `path` (an in-place edit changed it).
pub(crate) fn forget(path: &Path) {
    let mut g = cache().lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|c| c.path == path) {
        *g = None;
    }
}

/// Lists folder `dir` of the image at `path`.
fn browse_blocking(path: &Path, dir: Option<&str>) -> Result<BrowseResp, (StatusCode, String)> {
    let dir = match browse::normalize(dir.unwrap_or("")) {
        Some(d) => d,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{:?} is not a folder path", dir.unwrap_or("")),
            ))
        }
    };
    let (index, filesystem, compressed, label) =
        indexed(path).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let Some(entries) = index.list(&dir) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("{dir} is not a folder in {}", path.display()),
        ));
    };
    Ok(BrowseResp {
        filesystem: filesystem.name(),
        compressed,
        editable: filesystem.editable(),
        in_place: !compressed
            && matches!(
                filesystem,
                Filesystem::Exfat | Filesystem::Ufs2 | Filesystem::Pfs
            ),
        dir,
        entries: entries
            .into_iter()
            .map(|e| BrowseEntry {
                name: e.name,
                is_dir: e.is_dir,
                size: e.size,
                files: e.files,
            })
            .collect(),
        total_files: index.file_count(),
        total_bytes: index.total_bytes(),
        label,
    })
}

/// POST /api/image/browse  { "path": "/games/Game.ffpfsc", "dir": "sce_sys" }
pub(crate) async fn image_browse_handler(Json(req): Json<BrowseReq>) -> impl IntoResponse {
    let path = crate::fpkg_api::resolve_engine_path(&req.path);
    if ps5upload_fpkg::remote_source::is_remote(&path) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "only an image on this computer can be browsed here",
        )
        .into_response();
    }
    let dir = req.dir;
    match tokio::task::spawn_blocking(move || browse_blocking(&path, dir.as_deref())).await {
        Ok(Ok(resp)) => (StatusCode::OK, Json(resp)).into_response(),
        Ok(Err((code, e))) => json_err(code, e).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_build::{self, ImageFormat};
    use ps5upload_fpkg::sample::VerifyMode;
    use ps5upload_fpkg::source::FolderSource;
    use std::sync::atomic::AtomicBool;

    fn scratch(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ps5upload-browse-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn game(root: &Path) -> PathBuf {
        let game = root.join("PPSA99999");
        std::fs::create_dir_all(game.join("sce_sys")).unwrap();
        std::fs::create_dir_all(game.join("data/sub")).unwrap();
        std::fs::create_dir_all(game.join("data/empty")).unwrap();
        std::fs::write(game.join("eboot.bin"), vec![7u8; 70_000]).unwrap();
        std::fs::write(
            game.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99999\"}",
        )
        .unwrap();
        std::fs::write(game.join("data/sub/a.bin"), b"hello").unwrap();
        game
    }

    fn names(r: &BrowseResp) -> Vec<String> {
        r.entries
            .iter()
            .map(|e| format!("{}{} {}", e.name, if e.is_dir { "/" } else { "" }, e.size))
            .collect()
    }

    /// Each image Convert writes lists the same way, says what it is, and refuses a folder
    /// that is not there or is no folder path.
    #[test]
    fn every_image_kind_browses() {
        let root = scratch("kinds");
        let src = game(&root);
        let cases = [
            (ImageFormat::Exfat, false, "exfat"),
            (ImageFormat::Ffpkg, false, "ufs2"),
            (ImageFormat::Ffpfs, false, "pfs"),
            (ImageFormat::Exfat, true, "exfat"),
            (ImageFormat::Ffpkg, true, "ufs2"),
        ];
        for (n, (format, compress, fs)) in cases.into_iter().enumerate() {
            let (name, inner) = image_build::output_names(&src, format, compress);
            let image = root.join(format!("{n}-{name}"));
            let mut tree = FolderSource::open(&src).unwrap();
            image_build::build(
                format,
                inner.as_deref(),
                &mut tree,
                &image,
                VerifyMode::Full,
                8 << 20,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .unwrap();
            let r = browse_blocking(&image, None).unwrap();
            assert_eq!(
                (r.filesystem, r.compressed, r.editable),
                (fs, compress, true)
            );
            assert_eq!(
                names(&r),
                ["data/ 5", "sce_sys/ 23", "eboot.bin 70000"],
                "{name}"
            );
            assert_eq!((r.total_files, r.total_bytes), (3, 70_028));
            let r = browse_blocking(&image, Some("/data/")).unwrap();
            assert_eq!(r.dir, "data");
            assert_eq!(names(&r), ["empty/ 0", "sub/ 5"], "{name}");
            let (code, _) = browse_blocking(&image, Some("nope")).unwrap_err();
            assert_eq!(code, StatusCode::NOT_FOUND);
            let (code, _) = browse_blocking(&image, Some("../x")).unwrap_err();
            assert_eq!(code, StatusCode::BAD_REQUEST);
        }
        let (code, _) = browse_blocking(&root.join("missing.exfat"), None).unwrap_err();
        assert_eq!(code, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn build_image(src: &Path, image: &Path, format: ImageFormat, compress: bool) {
        let (_, inner) = image_build::output_names(src, format, compress);
        let mut tree = FolderSource::open(src).unwrap();
        image_build::build(
            format,
            inner.as_deref(),
            &mut tree,
            image,
            VerifyMode::Full,
            8 << 20,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
    }

    /// The browser's Save: one folder of an image becomes a folder with just its contents,
    /// one file a file, each byte-identical and checked against the image read afresh.
    #[test]
    fn a_folder_or_file_of_an_image_saves_to_the_computer() {
        use crate::folder_build;
        use ps5upload_fpkg::browse::Subtree;
        let root = scratch("save");
        let src = game(&root);
        let image = root.join("game.ffpfsc");
        build_image(&src, &image, ImageFormat::Ffpkg, true);
        let reopen_path = image.clone();
        let reopen = move || {
            ps5upload_fpkg::source::open(&reopen_path)
                .map(|t| -> Box<dyn ps5upload_fpkg::source::SourceTree> {
                    Box::new(Subtree::new(t, "data").unwrap())
                })
                .map_err(|e| e.to_string())
        };
        let mut sub = Subtree::new(ps5upload_fpkg::source::open(&image).unwrap(), "data").unwrap();
        let target = root.join("out/data");
        std::fs::create_dir_all(root.join("out")).unwrap();
        let built = folder_build::build(
            &mut sub,
            &reopen,
            &target,
            VerifyMode::Full,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(built.files, 1);
        assert_eq!(std::fs::read(target.join("sub/a.bin")).unwrap(), b"hello");
        assert!(target.join("empty").is_dir());
        assert!(!target.join("eboot.bin").exists());

        let reopen_path = image.clone();
        let reopen = move || ps5upload_fpkg::source::open(&reopen_path).map_err(|e| e.to_string());
        let mut whole = ps5upload_fpkg::source::open(&image).unwrap();
        let file = root.join("out/eboot.bin");
        let mut stages = Vec::new();
        folder_build::extract_file(
            &mut *whole,
            "eboot.bin",
            70_000,
            &reopen,
            &file,
            &AtomicBool::new(false),
            &mut |s, _, _| {
                if stages.last() != Some(&s.to_string()) {
                    stages.push(s.to_string());
                }
            },
        )
        .unwrap();
        assert_eq!(stages, ["write", "verify"]);
        assert_eq!(std::fs::read(&file).unwrap(), vec![7u8; 70_000]);
        assert!(!folder_build::partial_of(&file).exists());
        // An existing file is refused and left as it is.
        let err = folder_build::extract_file(
            &mut *whole,
            "eboot.bin",
            70_000,
            &reopen,
            &file,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An edited copy of each editable image kind: the changes are in the copy, everything
    /// else is as it was, the copy is the same kind of image, and the original is untouched.
    #[test]
    fn an_edited_copy_has_the_changes_and_the_original_is_untouched() {
        use crate::fpkg_api::{open_edited, ImageEditReq};
        let root = scratch("edit");
        let src = game(&root);
        let patch = root.join("patch.bin");
        std::fs::write(&patch, b"patched eboot").unwrap();
        let added = root.join("added");
        std::fs::create_dir_all(added.join("deep")).unwrap();
        std::fs::write(added.join("deep/new.txt"), b"new file").unwrap();
        for (n, (format, compress)) in [
            (ImageFormat::Exfat, false),
            (ImageFormat::Ffpkg, false),
            (ImageFormat::Ffpfs, false),
            (ImageFormat::Exfat, true),
        ]
        .into_iter()
        .enumerate()
        {
            let (name, _) = image_build::output_names(&src, format, compress);
            let image = root.join(format!("{n}-{name}"));
            build_image(&src, &image, format, compress);
            let before = std::fs::read(&image).unwrap();
            let edits = [
                ImageEditReq::Put {
                    path: "eboot.bin".into(),
                    from: patch.display().to_string(),
                },
                ImageEditReq::Put {
                    path: "data/added".into(),
                    from: added.display().to_string(),
                },
                ImageEditReq::Rename {
                    from: "data/sub/a.bin".into(),
                    to: "data/moved/hello.bin".into(),
                },
                ImageEditReq::Mkdir {
                    path: "data/made".into(),
                },
                ImageEditReq::Delete {
                    path: "data/empty".into(),
                },
            ];
            let mut edited = open_edited(&image, &edits).unwrap();
            assert_eq!(
                (edited.format, edited.compress),
                (format, compress),
                "{name}"
            );
            let (out_name, inner) = image_build::output_names(
                Path::new(&format!("{n}-copy")),
                edited.format,
                edited.compress,
            );
            let copy = root.join(out_name);
            image_build::build(
                edited.format,
                inner.as_deref(),
                &mut *edited.tree,
                &copy,
                VerifyMode::Full,
                8 << 20,
                &AtomicBool::new(false),
                &mut |_, _, _| {},
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                std::fs::read(&image).unwrap(),
                before,
                "{name}: original changed"
            );

            let r = browse_blocking(&copy, Some("data")).unwrap();
            // The renamed file took its bytes along and left its folder behind, empty; the new
            // folder is there; the deleted one is not.
            assert_eq!(
                names(&r),
                ["added/ 8", "made/ 0", "moved/ 5", "sub/ 0"],
                "{name}"
            );
            let mut tree = ps5upload_fpkg::source::open(&copy).unwrap();
            assert_eq!(tree.read("data/moved/hello.bin").unwrap(), b"hello");
            assert_eq!(tree.read("eboot.bin").unwrap(), b"patched eboot");
            assert_eq!(tree.read("data/added/deep/new.txt").unwrap(), b"new file");
            assert_eq!(
                tree.read("sce_sys/param.json").unwrap(),
                b"{\"titleId\":\"PPSA99999\"}"
            );
        }
        // A bad change is refused before anything is written.
        let image = root.join("0-PPSA99999.exfat");
        let bad = [ImageEditReq::Delete {
            path: "missing".into(),
        }];
        let err = open_edited(&image, &bad).err().unwrap();
        assert!(err.contains("not in the image"), "{err}");
        // So is a new name the image's own filesystem cannot hold: exFAT has no ':', UFS2 does.
        let colon = || {
            [ImageEditReq::Rename {
                from: "eboot.bin".into(),
                to: "e:boot.bin".into(),
            }]
        };
        let err = open_edited(&image, &colon()).err().unwrap();
        assert!(err.contains("exFAT does not allow"), "{err}");
        assert!(open_edited(&root.join("1-PPSA99999.ffpkg"), &colon()).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }
}
