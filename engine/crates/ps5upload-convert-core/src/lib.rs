//! PS5Upload's converter build code: a game folder or image into a `.exfat`, `.ffpkg` or
//! `.ffpfs` image (optionally inside a `.ffpfsc`), or into a folder, optionally with its files in
//! LZ4 or zstd asset packs, each read back before it gets its name.
//!
//! Shared by the engine on a computer (`ps5upload-engine`, which adds the HTTP API, jobs, the
//! streams to a console or server and the downloads) and by the converter that runs on the PS5
//! itself (`ps5upload-convert`). Its rules: std only, no async, no network. Progress is a
//! `stage(id, done, total)` callback; a job is stopped through its cancel flag, which every loop
//! checks through `ps5upload_fpkg::workshop::checkpoint`; log lines go to the sink the caller
//! installs ([`log`]).

pub mod log;

/// Compression threads on the PS5: three of a payload's five CPUs, the other two left to the
/// console's own UI and a suspended game.
pub const CONSOLE_WORKERS: usize = 3;
/// The worker threads a build's compression starts: [`CONSOLE_WORKERS`] on the PS5, every core
/// elsewhere (0).
pub const WORKERS: usize = if safe_write::ON_CONSOLE {
    CONSOLE_WORKERS
} else {
    0
};

pub mod archive_tree;
pub mod folder_build;
pub mod image_build;
pub mod lz4_packs;
pub mod refusal;
pub mod safe_write;
pub mod writing;
pub mod zstd_packs;
