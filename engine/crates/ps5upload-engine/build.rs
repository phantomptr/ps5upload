//! Optional build-time embedding of the two PS5 ELF images the engine may
//! need to hand to the console's loader on :9021.
//!
//! Why the engine carries these at all: the desktop client embeds them and
//! sends them itself, but a browser driving a self-hosted engine cannot —
//! it has no TCP socket and no copy of the bytes. Without them the install
//! cascade's DPI fallback (the only path that lands a game *patch*) is
//! unreachable from the web UI — the web UI half of #152.
//!
//! Both embeds are OPTIONAL, mirroring the desktop client's `have_dpi`
//! gate: the images are produced by `make payload` and need the PS5 payload
//! SDK, which plain `cargo build` and the PR Docker smoke-builds don't
//! have. Absent → the cfg is off, the engine reports the capability as
//! unavailable, and an operator can still point `PS5UPLOAD_PAYLOAD_DIR` at
//! the files at runtime.
//!
//! Two search locations, in order:
//!   1. `crates/ps5upload-engine/payload/` — the staging directory the
//!      Docker build populates, exactly like `webui/` for the React bundle.
//!      This keeps the images inside the `COPY engine/` layer so no
//!      Dockerfile needs a second context path.
//!   2. `<repo root>/payload/` — where `make payload` writes them, so a
//!      normal source build picks them up with no extra step.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(have_bundled_payload)");
    println!("cargo::rustc-check-cfg=cfg(have_bundled_dpi)");

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    // manifest_dir is engine/crates/ps5upload-engine; three levels up is the
    // repo root.
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("repo-root resolution");

    embed(
        "PS5UPLOAD_BUNDLED_PAYLOAD_ELF",
        "have_bundled_payload",
        &[
            manifest_dir.join("payload").join("ps5upload.elf"),
            repo_root.join("payload").join("ps5upload.elf"),
        ],
    );
    embed(
        "PS5UPLOAD_BUNDLED_DPI_ELF",
        "have_bundled_dpi",
        &[
            manifest_dir.join("payload").join("ezremote-dpi.elf"),
            repo_root
                .join("payload")
                .join("dpi")
                .join("ezremote-dpi.elf"),
        ],
    );
}

/// Point `env!(var)` at the first candidate that exists and turn `cfg` on.
/// Every candidate is registered with `rerun-if-changed` — including the
/// ones that are missing — so a later `make payload` re-runs this script
/// instead of leaving a stale "not bundled" build cached.
fn embed(var: &str, cfg: &str, candidates: &[PathBuf]) {
    let mut found: Option<&Path> = None;
    for path in candidates {
        println!("cargo:rerun-if-changed={}", path.display());
        if found.is_none() && path.is_file() {
            found = Some(path.as_path());
        }
    }
    if let Some(path) = found {
        println!("cargo:rustc-env={var}={}", path.display());
        println!("cargo:rustc-cfg={cfg}");
    }
}
