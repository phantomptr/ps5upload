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

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS not set");

    // ── PS5 payload (ps5upload.elf.gz): embedded on EVERY platform ──
    // It's just bytes streamed to the PS5 over the network, so the host
    // OS is irrelevant — probes.rs include_bytes! it via this env var on
    // desktop AND mobile, so "send payload/helper" works everywhere. We
    // embed the gzip (not the raw ELF) so linuxdeploy doesn't choke on
    // the payload's PS5 sprx deps when bundling the Linux AppImage.
    let payload_gz = repo_root.join("payload").join("ps5upload.elf.gz");
    require_file(&payload_gz, "payload gzip", "make -C payload all");
    println!(
        "cargo:rustc-env=PS5UPLOAD_PAYLOAD_GZ_BYTES={}",
        payload_gz.display()
    );
    println!("cargo:rerun-if-changed={}", payload_gz.display());

    // ── DPI install daemon (ezremote-dpi.elf.gz): OPTIONAL embed ──
    // Unlike the main payload we do NOT require this — CI's build-
    // verification jobs stub only the main payload, so the DPI gz won't
    // exist there. We gate the embed behind `have_dpi`: present →
    // embed + flag on; absent → flag off and probes.rs ships an empty
    // bytes/None so the shell still compiles (DPI just unavailable).
    // Real payload builds (`make payload` depends on `dpi`) produce it,
    // and publish.yml carries it into the client build for releases.
    // `rustc-check-cfg` is emitted unconditionally so the cfg is a known
    // name and the unexpected_cfgs lint stays quiet under `-D warnings`.
    println!("cargo::rustc-check-cfg=cfg(have_dpi)");
    let dpi_gz = repo_root
        .join("payload")
        .join("dpi")
        .join("ezremote-dpi.elf.gz");
    println!("cargo:rerun-if-changed={}", dpi_gz.display());
    if dpi_gz.is_file() {
        println!(
            "cargo:rustc-env=PS5UPLOAD_DPI_GZ_BYTES={}",
            dpi_gz.display()
        );
        println!("cargo:rustc-cfg=have_dpi");
    }

    // ── Engine binary: DESKTOP ONLY ──
    // On mobile the engine is linked as a library and runs in-process
    // (engine_mobile.rs); there's no sidecar binary to embed, and
    // PS5UPLOAD_ENGINE_BYTES is only consumed by the desktop engine
    // module. Bail out here on Android/iOS — the payload above is the
    // only thing those targets need from this script.
    if target_os == "android" || target_os == "ios" {
        return;
    }

    let target = std::env::var("TARGET").expect("TARGET not set");
    let host = std::env::var("HOST").expect("HOST not set");

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
    // Engine binary selection. Two paths exist:
    //   engine/target/<TARGET>/release/ps5upload-engine     (target-specific)
    //   engine/target/release/ps5upload-engine               (host default)
    //
    // Cross-compile builds (TARGET != HOST) MUST use the target-specific
    // path — the host binary is the wrong architecture.
    //
    // Native builds (TARGET == HOST) commonly write to the host default
    // path. The previous logic preferred the target-specific path
    // unconditionally, which was a foot-gun: if the user ran an explicit
    // `cargo build --target ...` once and then later ran plain `cargo
    // build`, the target-specific path would be stale relative to the
    // host path, and the Tauri shell would embed the stale engine —
    // missing newly-added routes (/api/transfer/download was a real
    // example of this) and surfacing as 404 to the user.
    //
    // For native builds, pick whichever file is newer so a recent
    // `cargo build` is always preferred over a stale explicit-target
    // build, and vice-versa.
    let engine_path = if target != host {
        engine_target_path
    } else {
        match (engine_target_path.is_file(), engine_host_path.is_file()) {
            (true, true) => {
                let mtime = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
                if mtime(&engine_target_path) >= mtime(&engine_host_path) {
                    engine_target_path
                } else {
                    engine_host_path
                }
            }
            (true, false) => engine_target_path,
            (false, true) => engine_host_path,
            // Neither exists — let require_file below produce the
            // actionable error pointing at the canonical target path.
            (false, false) => engine_target_path,
        }
    };
    let engine_build_hint = format!("cargo build --release -p ps5upload-engine --target {target}");
    require_file(&engine_path, "engine binary", &engine_build_hint);

    // Absolute path emitted as an env var so `include_bytes!(env!("…"))`
    // in engine.rs can pull the engine binary in at compile time. Also
    // emit `rerun-if-changed` so rebuilding the engine re-triggers this.
    println!(
        "cargo:rustc-env=PS5UPLOAD_ENGINE_BYTES={}",
        engine_path.display()
    );
    println!("cargo:rerun-if-changed={}", engine_path.display());
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
