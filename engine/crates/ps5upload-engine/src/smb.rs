//! SMB Browser: browse, download, and stage-to-PS5 from SMB2/3 shares.
//! Engine-only — no payload component needed. Uses the pure-Rust
//! `smb2` crate for zero native dependencies.
//!
//! Credentials are sent in the POST body (not query params) for security.
//!
//! ## PS5 upload path
//!
//! `stage_smb_path` streams a remote file or directory tree into a local
//! temp directory using chunked SMB reads (no full-file RAM buffer). The
//! engine transfer handler then runs the normal FTX2 pipeline against that
//! staged path and deletes the temp tree when done. See
//! `docs/smb-ps5-upload-design.md`.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::json_err;

/// Hard cap on files staged from one SMB tree — same order of magnitude as
/// a real game dump, stops runaway recursion on misconfigured shares.
pub const SMB_STAGE_MAX_FILES: u64 = 200_000;
pub const SMB_STAGE_MAX_DIRS: u64 = 200_000;

/// Normalize a user-entered server address into the `host:port` form
/// `smb2::connect` expects.
///
/// The crate takes a socket address, not a URL — so the `smb://host/`
/// form that every file manager shows (and that our own placeholder
/// suggested) was passed through verbatim and failed DNS resolution on
/// the literal string `smb://host`. Users typing the most natural thing
/// got "failed to lookup address information" and no hint as to why.
pub(crate) fn normalize_smb_server(input: &str) -> anyhow::Result<String> {
    let mut s = input.trim();
    if s.is_empty() {
        anyhow::bail!("server is required");
    }

    // Schemes are case-insensitive in practice (SMB://NAS from Windows).
    for scheme in ["smb://", "cifs://", "//"] {
        if s.len() >= scheme.len() && s[..scheme.len()].eq_ignore_ascii_case(scheme) {
            s = &s[scheme.len()..];
            break;
        }
    }

    // Drop any share or path pasted along with the host.
    if let Some(slash) = s.find('/') {
        s = &s[..slash];
    }
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("server is required");
    }

    // Bracketed IPv6 is colon-dense, so the brackets are what separate
    // "an address" from "an address and a port".
    if let Some(close) = s.rfind(']') {
        return Ok(if s[close..].starts_with("]:") {
            s.to_string()
        } else {
            format!("{s}:445")
        });
    }
    if s.contains(':') {
        return Ok(s.to_string());
    }
    Ok(format!("{s}:445"))
}

/// Classify an SMB error into an appropriate HTTP status code + message.
fn smb_err(e: anyhow::Error) -> (StatusCode, String) {
    let msg = format!("{e:#}");
    let lower = msg.to_ascii_lowercase();
    if lower.contains("auth")
        || lower.contains("logon")
        || lower.contains("credential")
        || lower.contains("denied")
        || lower.contains("nt_status_logon_failure")
    {
        (
            StatusCode::UNAUTHORIZED,
            format!("SMB authentication failed: {msg}"),
        )
    } else if lower.contains("refused")
        || lower.contains("timeout")
        || lower.contains("unreachable")
        || lower.contains("resolve")
        || lower.contains("timed out")
    {
        (
            StatusCode::BAD_GATEWAY,
            format!("SMB connection failed: {msg}"),
        )
    } else if lower.contains("not found")
        || lower.contains("no such")
        || lower.contains("bad_netpath")
    {
        (StatusCode::NOT_FOUND, format!("SMB path not found: {msg}"))
    } else {
        (StatusCode::BAD_GATEWAY, msg)
    }
}

#[derive(Debug, Deserialize)]
pub struct SmbListSharesRequest {
    pub server: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct SmbShare {
    pub name: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub share_type: String,
}

#[derive(Debug, Serialize)]
pub struct SmbListSharesResponse {
    pub shares: Vec<SmbShare>,
}

pub async fn smb_list_shares(Json(q): Json<SmbListSharesRequest>) -> impl IntoResponse {
    if q.server.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "server is required").into_response();
    }
    let r = async {
        let server = normalize_smb_server(&q.server)?;
        let mut client = smb2::connect(&server, &q.user, &q.password)
            .await
            .map_err(|e| anyhow::anyhow!("connect: {e}"))?;
        let shares = client
            .list_shares()
            .await
            .map_err(|e| anyhow::anyhow!("list_shares: {e}"))?;
        Ok::<Vec<SmbShare>, anyhow::Error>(
            shares
                .into_iter()
                .map(|s| SmbShare {
                    name: s.name.clone(),
                    comment: s.comment.clone(),
                    share_type: format!("{:?}", s.share_type),
                })
                .collect(),
        )
    }
    .await;

    match r {
        Ok(shares) => (StatusCode::OK, Json(SmbListSharesResponse { shares })).into_response(),
        Err(e) => {
            let (code, msg) = smb_err(e);
            json_err(code, msg).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SmbListDirRequest {
    pub server: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
    pub share: String,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct SmbDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    #[serde(default)]
    pub modified: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SmbListDirResponse {
    pub entries: Vec<SmbDirEntry>,
}

pub async fn smb_list_dir(Json(q): Json<SmbListDirRequest>) -> impl IntoResponse {
    if q.server.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "server is required").into_response();
    }
    if q.share.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "share is required").into_response();
    }
    let r = async {
        let server = normalize_smb_server(&q.server)?;
        let mut client = smb2::connect(&server, &q.user, &q.password)
            .await
            .map_err(|e| anyhow::anyhow!("connect: {e}"))?;
        let mut tree = client
            .connect_share(&q.share)
            .await
            .map_err(|e| anyhow::anyhow!("connect_share: {e}"))?;
        let entries = client
            .list_directory(&mut tree, &q.path)
            .await
            .map_err(|e| anyhow::anyhow!("list_directory: {e}"))?;
        Ok::<Vec<SmbDirEntry>, anyhow::Error>(
            entries
                .into_iter()
                .map(|e| SmbDirEntry {
                    name: e.name.clone(),
                    is_dir: e.is_directory,
                    size: e.size,
                    modified: if e.modified.0 == 0 {
                        None
                    } else {
                        Some(e.modified.0.to_string())
                    },
                })
                .collect(),
        )
    }
    .await;

    match r {
        Ok(entries) => (StatusCode::OK, Json(SmbListDirResponse { entries })).into_response(),
        Err(e) => {
            let (code, msg) = smb_err(e);
            json_err(code, msg).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SmbDownloadRequest {
    pub server: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
    pub share: String,
    pub path: String,
}

pub async fn smb_download_file(Json(q): Json<SmbDownloadRequest>) -> impl IntoResponse {
    if q.server.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "server is required").into_response();
    }
    if q.share.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "share is required").into_response();
    }
    if q.path.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "path is required").into_response();
    }
    let r = async {
        let server = normalize_smb_server(&q.server)?;
        let mut client = smb2::connect(&server, &q.user, &q.password)
            .await
            .map_err(|e| anyhow::anyhow!("connect: {e}"))?;
        let mut tree = client
            .connect_share(&q.share)
            .await
            .map_err(|e| anyhow::anyhow!("connect_share: {e}"))?;

        let info = client
            .stat(&mut tree, &q.path)
            .await
            .map_err(|e| anyhow::anyhow!("stat: {e}"))?;
        const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024 * 1024;
        if info.size > MAX_DOWNLOAD {
            anyhow::bail!(
                "file is {} bytes; the in-memory SMB downloader supports up to 2 GiB",
                info.size
            );
        }

        let data = client
            .read_file_pipelined(&mut tree, &q.path)
            .await
            .map_err(|e| anyhow::anyhow!("read_file: {e}"))?;
        Ok::<Vec<u8>, anyhow::Error>(data)
    }
    .await;

    match r {
        Ok(data) => {
            let filename = q
                .path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("download")
                .to_string();
            let mut headers = HeaderMap::new();
            headers.insert(
                HeaderName::from_static("content-type"),
                HeaderValue::from_static("application/octet-stream"),
            );
            let cd = format!("attachment; filename=\"{}\"", filename);
            if let Ok(hv) = HeaderValue::from_str(&cd) {
                headers.insert(HeaderName::from_static("content-disposition"), hv);
            }
            (StatusCode::OK, headers, Body::from(data)).into_response()
        }
        Err(e) => {
            let (code, msg) = smb_err(e);
            json_err(code, msg).into_response()
        }
    }
}

// ── Stage-and-forward (SMB → host temp → FTX2) ───────────────────────────

/// Credentials + share for a staging session.
#[derive(Debug, Clone)]
pub struct SmbSource {
    pub server: String,
    pub user: String,
    pub password: String,
    pub share: String,
    /// Path inside the share. Empty = share root.
    pub path: String,
}

/// Result of streaming an SMB path into a local staging directory.
#[derive(Debug)]
pub struct StagedSmb {
    /// Local file path, or directory whose *contents* mirror the remote tree
    /// when the remote was a directory (the basename is already the last
    /// component of this path).
    pub local_path: PathBuf,
    pub is_dir: bool,
    pub total_bytes: u64,
    pub file_count: u64,
    pub basename: String,
}

/// Join SMB path components with `/` (the crate normalizes separators).
pub fn join_smb_path(base: &str, name: &str) -> String {
    let base = base.trim().trim_matches(|c| c == '/' || c == '\\');
    let name = name.trim().trim_matches(|c| c == '/' || c == '\\');
    if base.is_empty() {
        name.to_string()
    } else if name.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{name}")
    }
}

/// Last component of an SMB path.
pub fn smb_basename(path: &str) -> String {
    let t = path.trim().trim_end_matches(['/', '\\']);
    if t.is_empty() {
        return "share".to_string();
    }
    t.rsplit(['/', '\\']).next().unwrap_or(t).to_string()
}

/// Resolve PS5 destination the same way Upload does: always suffix the
/// source basename under `dest_root`.
pub fn resolve_ps5_dest(dest_root: &str, source_basename: &str) -> String {
    let root = dest_root.trim().trim_end_matches('/');
    let root = if root.is_empty() { "/data" } else { root };
    let name = source_basename.trim().trim_matches(['/', '\\']);
    if name.is_empty() {
        root.to_string()
    } else {
        format!("{root}/{name}")
    }
}

fn reject_dotdot(path: &str) -> anyhow::Result<()> {
    for part in path.split(['/', '\\']) {
        if part == ".." {
            anyhow::bail!("path must not contain '..'");
        }
    }
    Ok(())
}

/// Validate one server-provided filename before joining it to the host staging
/// tree. A malicious or broken SMB server must not be able to return an
/// absolute name, separator, or parent component that escapes the job temp
/// directory. `Component` also catches Windows drive/UNC prefixes.
fn validate_stage_component(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        anyhow::bail!("unsafe SMB filename returned by server: {name:?}");
    }
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        anyhow::bail!("unsafe SMB filename returned by server: {name:?}");
    }
    Ok(())
}

/// Stream one remote file to `local` via chunked SMB reads.
async fn stream_file_to_disk(
    client: &mut smb2::SmbClient,
    tree: &smb2::Tree,
    remote: &str,
    local: &Path,
    progress: Option<&Arc<AtomicU64>>,
    cancel: Option<&Arc<AtomicBool>>,
) -> anyhow::Result<u64> {
    if let Some(parent) = local.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("mkdir {}: {e}", parent.display()))?;
    }
    let mut download = client
        .download(tree, remote)
        .await
        .map_err(|e| anyhow::anyhow!("smb download open {remote}: {e}"))?;
    // Sync disk I/O: tokio `fs` is not enabled in this crate; chunk writes
    // are small and the bottleneck is SMB, not local disk.
    let mut file = std::fs::File::create(local)
        .map_err(|e| anyhow::anyhow!("create {}: {e}", local.display()))?;
    let mut written = 0u64;
    while let Some(chunk) = download.next_chunk().await {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            anyhow::bail!("cancelled");
        }
        let bytes = chunk.map_err(|e| anyhow::anyhow!("smb read {remote}: {e}"))?;
        file.write_all(&bytes)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", local.display()))?;
        written += bytes.len() as u64;
        if let Some(p) = progress {
            p.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
    }
    file.flush()
        .map_err(|e| anyhow::anyhow!("flush {}: {e}", local.display()))?;
    Ok(written)
}

/// Recursively stage `remote` (file or directory) under `stage_root`.
///
/// For a remote **file**, writes `stage_root/<basename>`.
/// For a remote **directory**, writes `stage_root/<basename>/...` tree.
pub async fn stage_smb_path(
    source: &SmbSource,
    stage_root: &Path,
    progress: Option<Arc<AtomicU64>>,
    cancel: Option<Arc<AtomicBool>>,
) -> anyhow::Result<StagedSmb> {
    reject_dotdot(&source.path)?;
    let server = normalize_smb_server(&source.server)?;
    let mut client = smb2::connect(&server, &source.user, &source.password)
        .await
        .map_err(|e| anyhow::anyhow!("connect: {e}"))?;
    let mut tree = client
        .connect_share(&source.share)
        .await
        .map_err(|e| anyhow::anyhow!("connect_share: {e}"))?;

    let remote = source.path.trim().trim_matches(|c| c == '/' || c == '\\');
    let basename = if remote.is_empty() {
        // Staging the share root — use the share name as the folder.
        source.share.clone()
    } else {
        smb_basename(remote)
    };
    validate_stage_component(&basename)?;

    // Stat to decide file vs dir. Share root may not stat; treat as dir.
    let is_dir = if remote.is_empty() {
        true
    } else {
        let info = client
            .stat(&mut tree, remote)
            .await
            .map_err(|e| anyhow::anyhow!("stat {remote}: {e}"))?;
        info.is_directory
    };

    std::fs::create_dir_all(stage_root)
        .map_err(|e| anyhow::anyhow!("mkdir stage {}: {e}", stage_root.display()))?;

    if !is_dir {
        let local = stage_root.join(&basename);
        let n = stream_file_to_disk(
            &mut client,
            &tree,
            remote,
            &local,
            progress.as_ref(),
            cancel.as_ref(),
        )
        .await?;
        return Ok(StagedSmb {
            local_path: local,
            is_dir: false,
            total_bytes: n,
            file_count: 1,
            basename,
        });
    }

    // Directory: stage into stage_root/basename/
    let dir_local = stage_root.join(&basename);
    std::fs::create_dir_all(&dir_local)
        .map_err(|e| anyhow::anyhow!("mkdir {}: {e}", dir_local.display()))?;

    // BFS: (remote_rel, local_dir)
    let mut queue: Vec<(String, PathBuf)> = vec![(remote.to_string(), dir_local.clone())];
    let mut total_bytes = 0u64;
    let mut file_count = 0u64;
    let mut dir_count = 1u64;

    while let Some((remote_dir, local_dir)) = queue.pop() {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            anyhow::bail!("cancelled");
        }
        let entries = client
            .list_directory(&mut tree, &remote_dir)
            .await
            .map_err(|e| anyhow::anyhow!("list_directory {remote_dir}: {e}"))?;
        for e in entries {
            let name = e.name.clone();
            if name == "." || name == ".." {
                continue;
            }
            validate_stage_component(&name)?;
            let child_remote = join_smb_path(&remote_dir, &name);
            let child_local = local_dir.join(&name);
            if e.is_directory {
                dir_count += 1;
                if dir_count > SMB_STAGE_MAX_DIRS {
                    anyhow::bail!(
                        "SMB tree has more than {SMB_STAGE_MAX_DIRS} directories; refusing to stage"
                    );
                }
                std::fs::create_dir_all(&child_local)
                    .map_err(|e| anyhow::anyhow!("mkdir {}: {e}", child_local.display()))?;
                queue.push((child_remote, child_local));
            } else {
                file_count += 1;
                if file_count > SMB_STAGE_MAX_FILES {
                    anyhow::bail!(
                        "SMB tree has more than {SMB_STAGE_MAX_FILES} files; refusing to stage"
                    );
                }
                let n = stream_file_to_disk(
                    &mut client,
                    &tree,
                    &child_remote,
                    &child_local,
                    progress.as_ref(),
                    cancel.as_ref(),
                )
                .await?;
                total_bytes += n;
            }
        }
    }

    Ok(StagedSmb {
        local_path: dir_local,
        is_dir: true,
        total_bytes,
        file_count,
        basename,
    })
}

/// Best-effort recursive delete of a staging directory.
pub fn cleanup_stage(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_smb_server;
    use super::{join_smb_path, resolve_ps5_dest, smb_basename, validate_stage_component};

    /// The form our own placeholder suggested, and the form every file
    /// manager displays. Passing it through unchanged made `connect`
    /// DNS-resolve the literal string and fail instantly.
    #[test]
    fn strips_the_smb_scheme_users_actually_type() {
        assert_eq!(
            normalize_smb_server("smb://192.168.86.50:445").unwrap(),
            "192.168.86.50:445"
        );
        assert_eq!(
            normalize_smb_server("SMB://192.168.86.50:445").unwrap(),
            "192.168.86.50:445"
        );
        assert_eq!(normalize_smb_server("cifs://nas:445").unwrap(), "nas:445");
        assert_eq!(normalize_smb_server("//nas:445").unwrap(), "nas:445");
    }

    /// Omitting the port is the common case; 445 is the only port SMB2
    /// speaks in practice.
    #[test]
    fn supplies_the_default_port_when_missing() {
        assert_eq!(
            normalize_smb_server("192.168.86.50").unwrap(),
            "192.168.86.50:445"
        );
        assert_eq!(normalize_smb_server("smb://nas").unwrap(), "nas:445");
        assert_eq!(normalize_smb_server("nas.local").unwrap(), "nas.local:445");
    }

    /// An explicit port must survive untouched.
    #[test]
    fn keeps_an_explicit_port() {
        assert_eq!(normalize_smb_server("nas:1445").unwrap(), "nas:1445");
    }

    /// Users paste the whole path from a file manager's address bar.
    #[test]
    fn drops_a_pasted_share_path() {
        assert_eq!(normalize_smb_server("smb://nas/games").unwrap(), "nas:445");
        assert_eq!(
            normalize_smb_server("smb://nas:445/games/ps5").unwrap(),
            "nas:445"
        );
        assert_eq!(normalize_smb_server("smb://nas/").unwrap(), "nas:445");
    }

    /// IPv6 literals are colon-dense; the brackets are what disambiguate
    /// an address from an address-plus-port.
    #[test]
    fn handles_bracketed_ipv6() {
        assert_eq!(normalize_smb_server("[fe80::1]").unwrap(), "[fe80::1]:445");
        assert_eq!(
            normalize_smb_server("[fe80::1]:445").unwrap(),
            "[fe80::1]:445"
        );
        assert_eq!(
            normalize_smb_server("smb://[fe80::1]").unwrap(),
            "[fe80::1]:445"
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(normalize_smb_server("  smb://nas  ").unwrap(), "nas:445");
    }

    #[test]
    fn rejects_input_with_no_host() {
        assert!(normalize_smb_server("").is_err());
        assert!(normalize_smb_server("   ").is_err());
        assert!(normalize_smb_server("smb://").is_err());
    }

    #[test]
    fn join_smb_path_handles_empty_and_slashes() {
        assert_eq!(join_smb_path("", "foo"), "foo");
        assert_eq!(join_smb_path("a/b", "c"), "a/b/c");
        assert_eq!(join_smb_path("a/b/", "/c"), "a/b/c");
        assert_eq!(join_smb_path("a", ""), "a");
    }

    #[test]
    fn smb_basename_strips_trailing_slash() {
        assert_eq!(smb_basename("games/MyGame/"), "MyGame");
        assert_eq!(smb_basename("foo.pkg"), "foo.pkg");
        assert_eq!(smb_basename(""), "share");
    }

    #[test]
    fn resolve_ps5_dest_suffixes_basename() {
        assert_eq!(
            resolve_ps5_dest("/data/homebrew", "MyGame"),
            "/data/homebrew/MyGame"
        );
        assert_eq!(
            resolve_ps5_dest("/data/homebrew/", "foo.pkg"),
            "/data/homebrew/foo.pkg"
        );
        assert_eq!(resolve_ps5_dest("", "x"), "/data/x");
    }

    #[test]
    fn rejects_server_filenames_that_can_escape_the_stage_root() {
        for bad in [
            "",
            ".",
            "..",
            "../outside",
            "/absolute",
            "a/b",
            "a\\b",
            "bad\nname",
        ] {
            assert!(validate_stage_component(bad).is_err(), "accepted {bad:?}");
        }
        assert!(validate_stage_component("eboot.bin").is_ok());
        assert!(validate_stage_component("Game Folder").is_ok());
    }
}
