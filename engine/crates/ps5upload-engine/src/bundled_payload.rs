//! The PS5 ELF images this engine can hand to the console's loader.
//!
//! The desktop client embeds `ps5upload.elf` and `ezremote-dpi.elf` and
//! streams them to :9021 itself. A browser talking to a self-hosted engine
//! can do neither — no raw socket, no copy of the bytes — so before this
//! module the web UI could not bring up the DPI install daemon, and the
//! install cascade's DPI fallback was dead. That fallback is the only path
//! that lands a game *patch*, which is why base games installed from the
//! web UI and updates did not (issue #295).
//!
//! Two sources, in this order:
//!
//!   1. `PS5UPLOAD_PAYLOAD_DIR` — a directory of ELF images. An operator
//!      setting this has made an explicit choice (a locally built payload,
//!      a pinned older one), so it wins over whatever was compiled in.
//!   2. The build-time embed (`build.rs`), present in release builds and
//!      the published Docker images.
//!
//! Neither is guaranteed: `make payload` needs the PS5 payload SDK, so a
//! plain `cargo build` produces an engine with no images. That is not an
//! error — callers surface it as an unavailable capability with an
//! actionable message, exactly as the desktop client does when its own
//! optional DPI embed is missing.

use std::borrow::Cow;
use std::path::PathBuf;

#[cfg(have_bundled_payload)]
const EMBEDDED_PAYLOAD: Option<&[u8]> =
    Some(include_bytes!(env!("PS5UPLOAD_BUNDLED_PAYLOAD_ELF")) as &[u8]);
#[cfg(not(have_bundled_payload))]
const EMBEDDED_PAYLOAD: Option<&[u8]> = None;

#[cfg(have_bundled_dpi)]
const EMBEDDED_DPI: Option<&[u8]> =
    Some(include_bytes!(env!("PS5UPLOAD_BUNDLED_DPI_ELF")) as &[u8]);
#[cfg(not(have_bundled_dpi))]
const EMBEDDED_DPI: Option<&[u8]> = None;

/// Which ELF image a caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// The ps5upload helper itself — what "restore the payload" re-sends
    /// after the DPI daemon has displaced it on a single-payload loader.
    Payload,
    /// The standalone DPI install daemon (`payload/dpi/`).
    Dpi,
}

impl Image {
    /// File name to look for under `PS5UPLOAD_PAYLOAD_DIR`. Deliberately
    /// the same basename `make payload` produces, so an operator can point
    /// the variable straight at a build tree's `payload/` directory (the
    /// DPI image is looked up by basename in that directory too, not under
    /// its `dpi/` subdirectory — see `dir_candidates`).
    fn file_name(self) -> &'static str {
        match self {
            Image::Payload => "ps5upload.elf",
            Image::Dpi => "ezremote-dpi.elf",
        }
    }

    fn embedded(self) -> Option<&'static [u8]> {
        match self {
            Image::Payload => EMBEDDED_PAYLOAD,
            Image::Dpi => EMBEDDED_DPI,
        }
    }

    /// Human name used in the "we don't have this image" error, which is
    /// shown to the person trying to install something.
    fn label(self) -> &'static str {
        match self {
            Image::Payload => "ps5upload payload",
            Image::Dpi => "DPI install daemon",
        }
    }
}

/// Paths to try inside the override directory. `make payload` writes the
/// DPI image to `payload/dpi/`, so accept both that layout and a flat
/// directory of ELFs.
fn dir_candidates(dir: &str, image: Image) -> Vec<PathBuf> {
    let root = PathBuf::from(dir);
    let mut out = vec![root.join(image.file_name())];
    if image == Image::Dpi {
        out.push(root.join("dpi").join(image.file_name()));
    }
    out
}

/// The operator's override directory, if set and non-empty.
fn override_dir() -> Option<String> {
    std::env::var("PS5UPLOAD_PAYLOAD_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Resolve the bytes for `image`, or an actionable message saying why we
/// have none. Reads are done per call rather than cached: an override
/// directory exists so someone can swap the file, and these calls happen
/// once per install, not per frame.
pub fn image_bytes(image: Image) -> Result<Cow<'static, [u8]>, String> {
    if let Some(dir) = override_dir() {
        let candidates = dir_candidates(&dir, image);
        for path in &candidates {
            match std::fs::read(path) {
                Ok(bytes) if !bytes.is_empty() => return Ok(Cow::Owned(bytes)),
                Ok(_) => return Err(format!("{} at {} is empty", image.label(), path.display())),
                Err(_) => continue,
            }
        }
        // An explicitly configured directory that doesn't hold the image is
        // a misconfiguration worth naming, not a silent fall-through to a
        // build-time embed the operator was trying to override.
        return Err(format!(
            "PS5UPLOAD_PAYLOAD_DIR is set to {dir} but has no {} ({})",
            image.file_name(),
            image.label()
        ));
    }
    match image.embedded() {
        Some(bytes) if !bytes.is_empty() => Ok(Cow::Borrowed(bytes)),
        _ => Err(missing_image_message(image)),
    }
}

/// What to tell someone whose engine has no copy of `image`. Kept separate
/// from the lookup so it can be asserted on regardless of whether the build
/// running the tests happens to have the image embedded.
fn missing_image_message(image: Image) -> String {
    format!(
        "this engine build has no {} bundled — install the released engine \
         (or the ps5upload-engine Docker image), or point PS5UPLOAD_PAYLOAD_DIR \
         at a directory containing {}",
        image.label(),
        image.file_name()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_is_found_in_a_flat_dir_or_a_make_payload_tree() {
        let flat = dir_candidates("/opt/elf", Image::Dpi);
        assert_eq!(flat[0], PathBuf::from("/opt/elf/ezremote-dpi.elf"));
        // `make payload` writes it one level down; pointing the variable at
        // a build tree must work without the operator reshuffling files.
        assert_eq!(flat[1], PathBuf::from("/opt/elf/dpi/ezremote-dpi.elf"));
    }

    #[test]
    fn the_payload_image_is_only_looked_for_at_the_top_level() {
        let c = dir_candidates("/opt/elf", Image::Payload);
        assert_eq!(c, vec![PathBuf::from("/opt/elf/ps5upload.elf")]);
    }

    /// The message a user sees when the engine has no image must say what
    /// to do about it. A bare "not found" sent people to the wrong place in
    /// every previous install-path report on this cascade — and this one is
    /// reached by anyone who built the engine from source without the PS5
    /// payload SDK, which is a normal thing to have done.
    #[test]
    fn missing_image_error_names_the_remedy() {
        let msg = missing_image_message(Image::Dpi);
        assert!(msg.contains("DPI install daemon"), "unhelpful: {msg}");
        assert!(msg.contains("Docker image"), "unhelpful: {msg}");
        assert!(msg.contains("PS5UPLOAD_PAYLOAD_DIR"), "unhelpful: {msg}");
        assert!(msg.contains("ezremote-dpi.elf"), "unhelpful: {msg}");

        let msg = missing_image_message(Image::Payload);
        assert!(msg.contains("ps5upload.elf"), "unhelpful: {msg}");
    }
}
