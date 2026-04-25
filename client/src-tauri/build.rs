//! Tauri build-script hook + sidecar-binary embedding.
//!
//! Beyond the standard Tauri codegen, this script wires absolute
//! paths to the two sidecar binaries we embed in the desktop exe
//! (`ps5upload-engine[.exe]` and `ps5upload.elf.gz`) into env vars
//! that `include_bytes!(env!("…"))` can consume at compile time.
//!
//! Why embed at compile time: Windows ships with `--no-bundle`, which
//! means there's no Tauri Resources dir next to the portable .exe. We
//! previously packed the sidecars into the zip alongside the .exe,
//! but that left users with a two-thing folder instead of a single
//! portable file. Embedding gives a genuinely standalone exe: on
//! first use the main code extracts both to the user's local-app-data
//! dir and execs from there.
//!
//! Fails the build with an actionable message if either file is
//! missing — in CI the release workflow builds the engine first, in
//! dev you need `make payload` plus a release `ps5upload-engine`
//! built for the same target as the Tauri bundle.

use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    // manifest_dir is client/src-tauri; go up twice to the repo root.
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("repo-root resolution");

    let target = std::env::var("TARGET").expect("TARGET not set");
    let host = std::env::var("HOST").expect("HOST not set");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS not set");

    let engine_name = if target_os == "windows" {
        "ps5upload-engine.exe"
    } else {
        "ps5upload-engine"
    };
    let engine_target_path = repo_root
        .join("engine")
        .join("target")
        .join(&target)
        .join("release")
        .join(engine_name);
    let engine_host_path = repo_root
        .join("engine")
        .join("target")
        .join("release")
        .join(engine_name);
    let engine_path = if engine_target_path.is_file() {
        engine_target_path
    } else if target == host && engine_host_path.is_file() {
        engine_host_path
    } else {
        engine_target_path
    };
    let payload_gz = repo_root.join("payload").join("ps5upload.elf.gz");

    let engine_build_hint = format!("cargo build --release -p ps5upload-engine --target {target}");
    require_file(&engine_path, "engine binary", &engine_build_hint);
    require_file(&payload_gz, "payload gzip", "make -C payload all");

    // Absolute paths emitted as env vars so `include_bytes!(env!("…"))`
    // in src/ can pull them in at compile time. Also emit
    // `rerun-if-changed` so touching either file re-triggers the build.
    println!(
        "cargo:rustc-env=PS5UPLOAD_ENGINE_BYTES={}",
        engine_path.display()
    );
    println!("cargo:rerun-if-changed={}", engine_path.display());
    println!(
        "cargo:rustc-env=PS5UPLOAD_PAYLOAD_GZ_BYTES={}",
        payload_gz.display()
    );
    println!("cargo:rerun-if-changed={}", payload_gz.display());
}

fn require_file(path: &Path, label: &str, how_to_build: &str) {
    if !path.is_file() {
        eprintln!(
            "\n\x1b[1;31merror\x1b[0m: missing {label} at {}",
            path.display()
        );
        eprintln!("  The desktop shell embeds this file via include_bytes! so the");
        eprintln!("  compiled exe is self-contained.");
        eprintln!("  Build it first:  \x1b[1m{how_to_build}\x1b[0m\n");
        std::process::exit(1);
    }
}
