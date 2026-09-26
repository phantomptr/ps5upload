//! Build script for ps5upload-core.
//!
//! The only job here is a Windows link fix for the `unrar` dependency. `unrar`
//! compiles the bundled UnRAR C++ source (via `unrar_sys` + cc), which calls
//! Windows registry APIs (`RegOpenKeyExW`, `RegQueryValueExW`, `RegCloseKey`)
//! and crypto APIs (`CryptAcquireContextW`, `CryptReleaseContext`,
//! `CryptGenRandom`). Those live in `advapi32.lib`, but `unrar_sys` doesn't
//! emit the link directive, so on the MSVC target the final binary fails to
//! link with `LNK2019: unresolved external symbol __imp_RegCloseKey` (and the
//! five siblings) — which broke the Windows release build.
//!
//! Emitting the link directive here means every binary that links
//! ps5upload-core (the engine sidecar, the lab CLI, the test harness, and the
//! Tauri-embedded engine) picks up `advapi32` on Windows. No-op on every other
//! OS (and `unrar` is excluded from the Android build entirely, so this is
//! never reached there).
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-lib=dylib=advapi32");
    }
}
