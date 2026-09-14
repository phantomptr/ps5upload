//! The real game mounts: reading them is what the converter's image sources are for.
//!
//! Gated like the other sample tests — without the mount folder these skip with a note
//! instead of failing. `PS5UPLOAD_SAMPLE_MOUNTS` overrides the default location.

use std::path::{Path, PathBuf};

use ps5upload_fpkg::source;

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn mount_dir() -> Option<PathBuf> {
    let dir = std::env::var("PS5UPLOAD_SAMPLE_MOUNTS")
        .unwrap_or_else(|_| "/Volumes/Storage/PS5/games/game_mounts".to_string());
    let path = PathBuf::from(dir);
    path.is_dir().then_some(path)
}

fn images(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        // `._NAME.exfat` is macOS resource fork, not an image.
        .filter(|p| {
            !p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .starts_with("._")
        })
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        })
        .collect();
    out.sort();
    out
}

/// Every real `.exfat` mount opens, walks, and gives back the bytes of its files.
#[test]
fn every_real_exfat_mount_walks_and_reads() {
    let Some(dir) = mount_dir() else {
        eprintln!("skip: no mount folder (set PS5UPLOAD_SAMPLE_MOUNTS)");
        return;
    };
    let images = images(&dir, "exfat");
    if images.is_empty() {
        eprintln!("skip: no .exfat images in {}", dir.display());
        return;
    }
    for image in &images {
        let mut tree = source::open(image).expect("the image opens as a source");
        let files = tree.files().to_vec();
        let name = image.file_name().unwrap().to_string_lossy().into_owned();

        // Real counts run from 33 (a 90 GB game in one blob) to several hundred.
        assert!(
            files.len() >= 20,
            "{name}: only {} files walked",
            files.len()
        );
        for want in ["eboot.bin", "sce_sys/param.json", "sce_sys/icon0.png"] {
            assert!(
                files.iter().any(|f| f.path == want),
                "{name}: {want} is missing"
            );
        }
        assert!(
            files.windows(2).all(|w| w[0].path < w[1].path),
            "{name}: the walk is not sorted and unique"
        );

        let param = tree.read("sce_sys/param.json").expect("param.json reads");
        let id = source::content_id(&param).expect("param.json has a content id");
        assert_eq!(id.len(), 36, "{name}: content id {id:?}");

        // Real bytes at real offsets: a module magic at 0 and a PNG header in sce_sys.
        let magic = tree.read_range("eboot.bin", 0, 4).expect("eboot head");
        assert!(
            [
                source::magic::RAW_ELF.as_slice(),
                source::magic::SELF_PS5.as_slice(),
                source::magic::SELF_PS4.as_slice(),
                source::magic::SIGNED_SELF.as_slice()
            ]
            .contains(&magic.as_slice()),
            "{name}: eboot.bin starts with {magic:02x?}"
        );
        let png = tree
            .read_range("sce_sys/icon0.png", 0, 8)
            .expect("icon head");
        assert_eq!(png, PNG_MAGIC, "{name}: icon0.png is not a PNG");

        // The walk accounts for the volume: its files fill most of the image.
        let total: u64 = files.iter().map(|f| f.size).sum();
        let image_size = std::fs::metadata(image).unwrap().len();
        assert!(
            total <= image_size,
            "{name}: {total} bytes of files in a {image_size} byte image"
        );
        assert!(
            total * 100 >= image_size * 90,
            "{name}: only {total} of {image_size} bytes are walked files"
        );

        let dds = tree
            .read_range("sce_sys/icon0.dds", 0, 4)
            .expect("dds head");
        assert_eq!(dds, b"DDS ", "{name}: icon0.dds is not a DDS");

        // A ranged read takes different offset arithmetic than the whole-file read; the
        // tail of a file both times must agree.
        let full = tree.read("sce_sys/param.json").expect("param.json reads");
        let tail = tree
            .read_range("sce_sys/param.json", full.len() as u64 - 16, 16)
            .expect("tail read");
        assert_eq!(tail, full[full.len() - 16..], "{name}: tail mismatch");
        // And past the end it stops, rather than failing or reading a cluster.
        let past = tree
            .read_range("sce_sys/param.json", full.len() as u64 - 4, 64)
            .expect("past-the-end read");
        assert_eq!(past, full[full.len() - 4..], "{name}: past-the-end read");
    }
    eprintln!("read {} exfat mounts", images.len());
}
