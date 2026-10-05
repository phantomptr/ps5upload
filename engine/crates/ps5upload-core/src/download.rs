//! What a download request names.
//!
//! The download itself runs over AVA1 (`ps5upload_ava1::download`). Only the request's shape
//! lives here, because the engine's routes and the AVA1 adapters both take it.

/// What the user picked to download. Folder = walk; file = single entry. The shape is decided
/// by the caller (Library/FileSystem already knows whether the row is a file or a directory) so
/// there is no redundant remote stat round-trip just to classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadKind {
    File,
    Folder,
}
