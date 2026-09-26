//! List and extract RAR archives.
//!
//! Vendored copy of upstream `unrar` 0.5.8 with one local fix — see
//! `README.md` in this directory for what changed and why.
//!
//! ps5upload local change: upstream had
//! `#![doc = include_str!("../README.md")]` here. That makes the crate
//! fail to compile whenever README.md is absent from the build context,
//! which is easy to do by accident — a `.dockerignore` excluding `*.md`
//! is enough. Documentation should never be a build dependency, so the
//! text lives in README.md and this is a plain doc comment.
#![warn(missing_docs)]

pub use archive::Archive;
use unrar_sys as native;
mod archive;
pub mod error;
mod pathed;
mod open_archive;
pub use error::UnrarResult;
pub use open_archive::{
    CursorBeforeFile, CursorBeforeHeader, FileHeader, List, ListSplit, OpenArchive, Process,
    StreamSink, VolumeInfo,
};
