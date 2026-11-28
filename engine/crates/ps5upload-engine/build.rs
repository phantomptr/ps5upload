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
    println!("cargo::rustc-check-cfg=cfg(have_bundled_installer)");
    println!("cargo::rustc-check-cfg=cfg(have_bundled_elfldr)");

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
        "PS5UPLOAD_BUNDLED_INSTALLER_ELF",
        "have_bundled_installer",
        &[
            manifest_dir.join("payload").join("ps5upload-installer.elf"),
            repo_root
                .join("payload")
                .join("installer")
                .join("ps5upload-installer.elf"),
        ],
    );
    // The patched elfldr, checked in under third_party/ (see its README), so
    // every build embeds it with or without the payload SDK.
    embed(
        "PS5UPLOAD_BUNDLED_ELFLDR_ELF",
        "have_bundled_elfldr",
        &[repo_root
            .join("third_party")
            .join("elfldr")
            .join("elfldr-ps5.elf")],
    );

    // PS5Upload Play's release zip (play_app_install.rs installs it on a PS5). Optional like the
    // ELFs: it is built in Docker by apps/play's Makefile, which a plain `cargo build` does not
    // run. Only a zip of THIS version is taken (named with 13.0.0 or 13.000.000), so a stale
    // build left in a folder is never shipped as the current app. Looked for in the Docker
    // staging folder (next to the ELFs), where publish.yml's artifact puts it (payload/play/),
    // and where `make play-app` writes it (apps/play/dist/).
    println!("cargo::rustc-check-cfg=cfg(have_bundled_play)");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut names = vec![format!("PS5Upload-Play-FAKE99261-{version}.zip")];
    if let Some(ps5) = ps5_content_version(&version) {
        names.push(format!("PS5Upload-Play-FAKE99261-{ps5}.zip"));
    }
    let dirs = [
        manifest_dir.join("payload"),
        repo_root.join("payload").join("play"),
        repo_root.join("apps").join("play").join("dist"),
    ];
    let candidates: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .collect();
    embed(
        "PS5UPLOAD_BUNDLED_PLAY_ZIP",
        "have_bundled_play",
        &candidates,
    );
}

/// `13.0.0` -> `13.000.000` (the PS5's contentVersion form; scripts/update-version.js).
fn ps5_content_version(v: &str) -> Option<String> {
    let mut it = v.split(['.', '-', '+']);
    let a: u32 = it.next()?.parse().ok()?;
    let b: u32 = it.next()?.parse().ok()?;
    let c: u32 = it.next()?.parse().ok()?;
    (a <= 99 && b <= 999 && c <= 999).then(|| format!("{a:02}.{b:03}.{c:03}"))
}

/// Point `env!(var)` at the first candidate that exists and turn `cfg` on.
/// Every candidate is watched so a later `make payload` (or a Docker staging
/// copy) re-runs this script instead of leaving a stale "not bundled" build
/// cached. A candidate that exists is watched as a file; a missing one through
/// its nearest existing folder, whose contents change when the file appears.
/// Registering the missing path itself made cargo treat it as changed on
/// every invocation, so the engine recompiled on every build and test run.
fn embed(var: &str, cfg: &str, candidates: &[PathBuf]) {
    let mut found: Option<&Path> = None;
    for path in candidates {
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
        } else if let Some(dir) = path.ancestors().skip(1).find(|d| d.is_dir()) {
            println!("cargo:rerun-if-changed={}", dir.display());
        }
        if found.is_none() && path.is_file() {
            found = Some(path.as_path());
        }
    }
    if let Some(path) = found {
        println!("cargo:rustc-env={var}={}", path.display());
        println!("cargo:rustc-cfg={cfg}");
    }
}
