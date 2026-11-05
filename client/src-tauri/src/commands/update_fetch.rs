//! Streams an update file to disk: the one path the desktop download (`updates.rs`) and the
//! Android APK download (`update_android.rs`) share.
//!
//! A slow link must be able to finish. There is no limit on the whole transfer, only on how long
//! nothing arrives (`IDLE_TIMEOUT`): a total limit made a 194 MB Linux zip impossible below about
//! 2.6 Mbit/s, every try ending at the same limit and starting again from byte 0. What arrived
//! stays in the `.part` file when the connection drops, and the next try asks for the rest with a
//! `Range` request (GitHub's CDN answers them). The checksum is taken over the file on disk once
//! it is whole, so a resumed file is checked like any other, and one that does not match after a
//! resume is downloaded once more from the start.

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use tokio::sync::Notify;

use super::shell_error::{code_only, coded};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// The connection counts as lost when no byte arrives for this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often progress is reported.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

/// The client for an update file: an idle limit and a connect limit, no limit on the whole.
pub(crate) fn client() -> Result<reqwest::Client, String> {
    client_going_quiet_after(IDLE_TIMEOUT)
}

fn client_going_quiet_after(idle: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(idle)
        .build()
        .map_err(|e| {
            coded(
                "update_network",
                serde_json::json!({ "detail": e.to_string() }),
            )
        })
}

/// The download running now: at most one (two would write the same `.part`), and what cancels it.
static ACTIVE: Mutex<Option<Arc<Notify>>> = Mutex::new(None);

fn active_slot() -> std::sync::MutexGuard<'static, Option<Arc<Notify>>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Holds the one download slot; gives it back when dropped.
pub(crate) struct ActiveDownload(Arc<Notify>);

impl ActiveDownload {
    /// Take the slot, or say a download is already running.
    pub(crate) fn begin() -> Result<Self, String> {
        let mut slot = active_slot();
        if slot.is_some() {
            return Err(code_only("update_busy"));
        }
        let cancel = Arc::new(Notify::new());
        *slot = Some(cancel.clone());
        Ok(Self(cancel))
    }

    pub(crate) fn cancel_signal(&self) -> &Notify {
        &self.0
    }
}

impl Drop for ActiveDownload {
    fn drop(&mut self) {
        *active_slot() = None;
    }
}

/// Stop the running update download and delete what it saved so far. False when none is running.
#[tauri::command]
pub async fn update_download_cancel() -> bool {
    match active_slot().as_ref() {
        Some(cancel) => {
            // Kept as a permit if the download is between two awaits: it cannot be missed.
            cancel.notify_one();
            true
        }
        None => false,
    }
}

/// Why a transfer stopped.
#[derive(Debug, PartialEq, Eq)]
enum FetchError {
    /// The person cancelled it.
    Cancelled,
    /// The server answered with this status.
    Http(u16),
    /// Bigger than the file can be.
    TooLarge,
    /// The connection failed or went quiet; what arrived is kept for the next try.
    Network(String),
    /// The file could not be written.
    Disk(String),
}

impl FetchError {
    fn into_message(self) -> String {
        match self {
            Self::Cancelled => code_only("update_cancelled"),
            Self::Http(status) => coded("update_http", serde_json::json!({ "status": status })),
            Self::TooLarge => code_only("update_too_large"),
            Self::Network(detail) => coded(
                "update_interrupted",
                serde_json::json!({ "detail": detail }),
            ),
            Self::Disk(detail) => coded("update_disk", serde_json::json!({ "detail": detail })),
        }
    }

    /// What arrived so far is worth keeping for the next try: only a transfer that was cut off
    /// (a server error says nothing about it either). A person's cancel, a file too large and a
    /// disk that failed leave nothing behind.
    fn keeps_part(&self) -> bool {
        matches!(self, Self::Http(_) | Self::Network(_))
    }
}

fn network(e: &reqwest::Error) -> FetchError {
    FetchError::Network(crate::engine_http::error_chain(e))
}

/// Where an answer's body starts in the file.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// The rest of the file, as asked for (`total` 0 when not said).
    Rest { total: u64 },
    /// The whole file: a server that ignores `Range`, or no `Range` was sent.
    Whole,
    /// A range that is not what was asked for: ask again without one.
    Unusable,
}

/// `bytes <start>-<end>/<total>` (`total` may be `*`), as (start, total).
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (start, _end) = span.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()))
}

/// What a success answer means, for a request that already had `have` bytes.
fn read_answer(status: StatusCode, content_range: Option<&str>, have: u64) -> Answer {
    if status != StatusCode::PARTIAL_CONTENT {
        return Answer::Whole;
    }
    match content_range.and_then(parse_content_range) {
        Some((start, total)) if start == have && have > 0 => Answer::Rest {
            total: total.unwrap_or(0),
        },
        _ => Answer::Unusable,
    }
}

/// `bytes */<total>`: the size a 416 answer gives.
fn range_refused_total(value: Option<&str>) -> Option<u64> {
    value?.trim().strip_prefix("bytes */")?.trim().parse().ok()
}

fn part_len(part: &Path) -> u64 {
    std::fs::metadata(part).map(|m| m.len()).unwrap_or(0)
}

/// A blocking file step off the async workers (their other tasks must keep running, F28).
async fn on_disk<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, FetchError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| FetchError::Disk(e.to_string()))?
        .map_err(|e| FetchError::Disk(e.to_string()))
}

/// What a finished transfer left in the `.part` file.
#[derive(Debug, PartialEq, Eq)]
struct Fetched {
    bytes: u64,
    /// It carried on from an earlier try's bytes.
    resumed: bool,
}

/// Bring `part` up to the whole file at `url`, resuming what is already there.
async fn fetch(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    max_bytes: u64,
    cancel: &Notify,
    on_progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<Fetched, FetchError> {
    use futures_util::StreamExt;

    let mut have = {
        let part = part.to_path_buf();
        on_disk(move || Ok(part_len(&part))).await?
    };
    if have > max_bytes {
        have = 0;
    }
    // A second round only when a resume was refused: the first asked with a Range, the next
    // does not, so this ends.
    let (resp, offset, total) = loop {
        let mut request = client.get(url);
        if have > 0 {
            request = request.header(RANGE, format!("bytes={have}-"));
        }
        let resp = tokio::select! {
            biased;
            _ = cancel.notified() => return Err(FetchError::Cancelled),
            r = request.send() => r.map_err(|e| network(&e))?,
        };
        let status = resp.status();
        let content_range = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if status == StatusCode::RANGE_NOT_SATISFIABLE && have > 0 {
            // All of it arrived last time (what failed then came after the transfer).
            if range_refused_total(content_range.as_deref()) == Some(have) {
                return Ok(Fetched {
                    bytes: have,
                    resumed: true,
                });
            }
            have = 0;
            continue;
        }
        if !status.is_success() {
            return Err(FetchError::Http(status.as_u16()));
        }
        match read_answer(status, content_range.as_deref(), have) {
            Answer::Rest { total } => {
                // A range answer's Content-Length is the rest only.
                let total = if total > 0 {
                    total
                } else {
                    resp.content_length().map_or(0, |len| have + len)
                };
                break (resp, have, total);
            }
            Answer::Whole => {
                let total = resp.content_length().unwrap_or(0);
                break (resp, 0, total);
            }
            Answer::Unusable if have > 0 => {
                have = 0;
                continue;
            }
            Answer::Unusable => {
                return Err(FetchError::Network("unexpected partial answer".into()));
            }
        }
    };
    if total > max_bytes {
        return Err(FetchError::TooLarge);
    }

    let mut file = {
        let part = part.to_path_buf();
        on_disk(move || {
            if offset > 0 {
                // Written at the measured length, not appended: bytes past it (a file that grew
                // since) would put the rest in the wrong place.
                let mut f = std::fs::OpenOptions::new().write(true).open(&part)?;
                f.set_len(offset)?;
                f.seek(SeekFrom::Start(offset))?;
                Ok(f)
            } else {
                std::fs::File::create(&part)
            }
        })
        .await?
    };

    let mut received = offset;
    let mut stream = resp.bytes_stream();
    let mut last_report = Instant::now();
    on_progress(received, total);
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.notified() => return Err(FetchError::Cancelled),
            next = stream.next() => next,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| network(&e))?;
        received = received.saturating_add(chunk.len() as u64);
        if received > max_bytes {
            return Err(FetchError::TooLarge);
        }
        file = on_disk(move || {
            file.write_all(&chunk)?;
            Ok(file)
        })
        .await?;
        if last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            on_progress(received, total);
        }
    }
    on_progress(received, total);
    on_disk(move || file.sync_all()).await?;
    if total > 0 && received != total {
        return Err(FetchError::Network(format!(
            "the connection ended at {received} of {total} bytes"
        )));
    }
    Ok(Fetched {
        bytes: received,
        resumed: offset > 0,
    })
}

/// Lowercase hex SHA-256 of a file on disk.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The update file downloaded to `part` and checked: its size and SHA-256.
pub(crate) struct Verified {
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

/// Download `url` to `part` (carrying on from what is there) and check it against `expected`.
/// A failed transfer keeps the bytes for the next try; a file that does not match is deleted, and
/// when it was a resumed one it is downloaded once more from the start, since the bytes from
/// before may be what is wrong. With no checksum published (a release before 10.2) nothing could
/// tell, so such a download always starts from the start.
pub(crate) async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    max_bytes: u64,
    expected: Option<&str>,
    cancel: &Notify,
    on_progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<Verified, String> {
    if expected.is_none() {
        remove_quietly(part).await;
    }
    let mut started_over = false;
    loop {
        let fetched = match fetch(client, url, part, max_bytes, cancel, on_progress).await {
            Ok(f) => f,
            Err(e) => {
                if !e.keeps_part() {
                    remove_quietly(part).await;
                }
                return Err(e.into_message());
            }
        };
        let actual = {
            let part = part.to_path_buf();
            on_disk(move || sha256_file(&part))
                .await
                .map_err(FetchError::into_message)?
        };
        if !expected.is_some_and(|want| !want.eq_ignore_ascii_case(&actual)) {
            return Ok(Verified {
                bytes: fetched.bytes,
                sha256: actual,
            });
        }
        remove_quietly(part).await;
        if !fetched.resumed || started_over {
            return Err(coded(
                "update_checksum",
                serde_json::json!({ "expected": expected, "actual": actual }),
            ));
        }
        started_over = true;
    }
}

async fn remove_quietly(path: &Path) {
    let path = path.to_path_buf();
    let _ = on_disk(move || std::fs::remove_file(&path)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ps5upload-update-fetch-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("file.part")
    }

    /// One answer of the test server: what it sends for a request.
    type Reply = Box<dyn Fn(Option<u64>, &[u8]) -> Vec<u8> + Send + Sync>;

    /// A loopback HTTP server serving `file`, each connection answered by `reply` (given the
    /// start of the request's `Range`). Returns the URL and the requests it saw.
    fn serve(file: &'static [u8], reply: Reply) -> (String, Arc<Mutex<Vec<Option<u64>>>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let start = text
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim().trim_end_matches('-').parse::<u64>().ok());
                log.lock().unwrap().push(start);
                let _ = stream.write_all(&reply(start, file));
            }
        });
        (url, seen)
    }

    /// A server that honours Range.
    fn honest(start: Option<u64>, file: &[u8]) -> Vec<u8> {
        match start {
            Some(s) if s as usize >= file.len() => format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                file.len()
            )
            .into_bytes(),
            Some(s) => {
                let rest = &file[s as usize..];
                let mut out = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    file.len() - 1,
                    file.len(),
                    rest.len()
                )
                .into_bytes();
                out.extend_from_slice(rest);
                out
            }
            None => whole(file),
        }
    }

    fn whole(file: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            file.len()
        )
        .into_bytes();
        out.extend_from_slice(file);
        out
    }

    async fn run(url: &str, part: &Path, expected: Option<&str>) -> Result<Verified, String> {
        let cancel = Notify::new();
        download_verified(
            &client().unwrap(),
            url,
            part,
            1 << 20,
            expected,
            &cancel,
            &mut |_, _| {},
        )
        .await
    }

    #[tokio::test]
    async fn a_download_cut_off_keeps_its_bytes_and_the_next_try_asks_for_the_rest() {
        // F1: every try on a slow link started again from byte 0.
        static FILE: &[u8] = b"0123456789abcdefghij";
        let attempts = AtomicUsize::new(0);
        let (url, seen) = serve(
            FILE,
            Box::new(move |start, file| {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    // Promises the whole file, then the connection ends after 8 bytes.
                    let mut out = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        file.len()
                    )
                    .into_bytes();
                    out.extend_from_slice(&file[..8]);
                    out
                } else {
                    honest(start, file)
                }
            }),
        );
        let part = scratch("resume");
        let first = run(&url, &part, None).await;
        assert!(first.is_err(), "the transfer was cut off");
        assert!(first.err().unwrap().contains("update_interrupted"));
        assert_eq!(std::fs::read(&part).unwrap(), &FILE[..8], "kept");

        let done = run(&url, &part, Some(&sha_of(FILE))).await.unwrap();
        assert_eq!(done.bytes, FILE.len() as u64);
        assert_eq!(std::fs::read(&part).unwrap(), FILE);
        assert_eq!(*seen.lock().unwrap(), vec![None, Some(8)]);
    }

    fn sha_of(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[tokio::test]
    async fn a_server_that_ignores_range_starts_the_file_over() {
        static FILE: &[u8] = b"the whole file again";
        let (url, seen) = serve(FILE, Box::new(|_, file| whole(file)));
        let part = scratch("ignored");
        std::fs::write(&part, b"stale bytes").unwrap();
        let done = run(&url, &part, Some(&sha_of(FILE))).await.unwrap();
        assert_eq!(done.bytes, FILE.len() as u64);
        assert_eq!(std::fs::read(&part).unwrap(), FILE, "not appended");
        assert_eq!(*seen.lock().unwrap(), vec![Some(11)]);
    }

    #[tokio::test]
    async fn a_range_the_server_refuses_starts_over_and_a_whole_part_needs_nothing() {
        static FILE: &[u8] = b"abc";
        let (url, seen) = serve(FILE, Box::new(honest));
        // Longer than the file: thrown away.
        let part = scratch("refused");
        std::fs::write(&part, b"longer than the file").unwrap();
        run(&url, &part, Some(&sha_of(FILE))).await.unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), FILE);
        assert_eq!(*seen.lock().unwrap(), vec![Some(20), None]);

        // Exactly the file (what failed last time came after the transfer): only checked.
        seen.lock().unwrap().clear();
        run(&url, &part, Some(&sha_of(FILE))).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![Some(3)]);
    }

    #[tokio::test]
    async fn a_resumed_file_that_does_not_match_is_downloaded_once_more_from_the_start() {
        static FILE: &[u8] = b"0123456789";
        let (url, seen) = serve(FILE, Box::new(honest));
        let part = scratch("mismatch");
        // Bytes from another build under the same name.
        std::fs::write(&part, b"WRONG").unwrap();
        let done = run(&url, &part, Some(&sha_of(FILE))).await.unwrap();
        assert_eq!(done.sha256, sha_of(FILE));
        assert_eq!(std::fs::read(&part).unwrap(), FILE);
        assert_eq!(*seen.lock().unwrap(), vec![Some(5), None]);
    }

    #[tokio::test]
    async fn a_fresh_file_that_does_not_match_is_deleted_and_reported() {
        static FILE: &[u8] = b"0123456789";
        let (url, seen) = serve(FILE, Box::new(honest));
        let part = scratch("bad");
        let want = "0".repeat(64);
        let err = run(&url, &part, Some(&want)).await.err().unwrap();
        assert!(err.contains("update_checksum"), "{err}");
        assert!(!part.exists());
        assert_eq!(seen.lock().unwrap().len(), 1, "not tried again");
    }

    #[tokio::test]
    async fn no_published_checksum_still_downloads_and_reports_the_hash() {
        static FILE: &[u8] = b"abc";
        let (url, _) = serve(FILE, Box::new(honest));
        let done = run(&url, &scratch("nochecksum"), None).await.unwrap();
        assert_eq!(
            done.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[tokio::test]
    async fn with_no_checksum_to_catch_a_bad_mix_nothing_is_resumed() {
        static FILE: &[u8] = b"abc";
        let (url, seen) = serve(FILE, Box::new(honest));
        let part = scratch("noresume");
        std::fs::write(&part, b"a").unwrap();
        run(&url, &part, None).await.unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), FILE);
        assert_eq!(*seen.lock().unwrap(), vec![None]);
    }

    #[tokio::test]
    async fn a_server_error_is_reported_with_its_status_and_keeps_the_bytes() {
        static FILE: &[u8] = b"abc";
        let (url, _) = serve(
            FILE,
            Box::new(|_, _| {
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec()
            }),
        );
        let part = scratch("http");
        std::fs::write(&part, b"ab").unwrap();
        let err = run(&url, &part, Some(&sha_of(FILE))).await.err().unwrap();
        let v: serde_json::Value = serde_json::from_str(&err).unwrap();
        assert_eq!(v["code"], "update_http");
        assert_eq!(v["status"], 503);
        assert_eq!(std::fs::read(&part).unwrap(), b"ab");
    }

    #[tokio::test]
    async fn a_file_over_the_limit_is_refused_and_leaves_nothing() {
        static FILE: &[u8] = b"0123456789";
        let (url, _) = serve(FILE, Box::new(|_, file| whole(file)));
        let part = scratch("large");
        let cancel = Notify::new();
        let err = download_verified(
            &client().unwrap(),
            &url,
            &part,
            4,
            None,
            &cancel,
            &mut |_, _| {},
        )
        .await
        .err()
        .unwrap();
        assert!(err.contains("update_too_large"), "{err}");
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn a_cancel_stops_at_once_and_deletes_the_part() {
        // F8: a download started by mistake could not be stopped. The server sends headers and
        // a first chunk, then nothing more.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nfirst",
                );
                let _ = stream.flush();
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        let part = scratch("cancel");
        let cancel = Arc::new(Notify::new());
        let later = cancel.clone();
        let started = Instant::now();
        let task = tokio::spawn({
            let part = part.clone();
            async move {
                download_verified(
                    &client().unwrap(),
                    &url,
                    &part,
                    1 << 20,
                    None,
                    &cancel,
                    &mut |_, total| assert_eq!(total, 1000),
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        later.notify_one();
        let err = task.await.unwrap().err().unwrap();
        assert!(err.contains("update_cancelled"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(4), "at once");
        assert!(!part.exists(), "nothing left behind");
    }

    #[tokio::test]
    async fn a_connection_that_goes_quiet_counts_as_lost_and_keeps_what_arrived() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nfirst",
                );
                let _ = stream.flush();
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        let part = scratch("quiet");
        let cancel = Notify::new();
        let started = Instant::now();
        let err = download_verified(
            &client_going_quiet_after(Duration::from_millis(200)).unwrap(),
            &url,
            &part,
            1 << 20,
            None,
            &cancel,
            &mut |_, _| {},
        )
        .await
        .err()
        .unwrap();
        assert!(err.contains("update_interrupted"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "not the whole transfer's limit"
        );
        assert_eq!(std::fs::read(&part).unwrap(), b"first");
    }

    #[tokio::test]
    async fn a_slow_transfer_longer_than_the_idle_limit_still_finishes() {
        // F1: the old limit was on the whole transfer, so below about 2.6 Mbit/s no try ever ended.
        static FILE: &[u8] = b"0123456789";
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        FILE.len()
                    )
                    .as_bytes(),
                );
                for byte in FILE {
                    let _ = stream.write_all(&[*byte]);
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let part = scratch("slow");
        let cancel = Notify::new();
        let done = download_verified(
            &client_going_quiet_after(Duration::from_millis(600)).unwrap(),
            &url,
            &part,
            1 << 20,
            Some(&sha_of(FILE)),
            &cancel,
            &mut |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(done.bytes, FILE.len() as u64);
    }

    #[tokio::test]
    async fn progress_counts_from_the_bytes_already_there() {
        static FILE: &[u8] = b"0123456789";
        let (url, _) = serve(FILE, Box::new(honest));
        let part = scratch("progress");
        std::fs::write(&part, b"01234").unwrap();
        let cancel = Notify::new();
        let mut last = (0, 0);
        let mut first = None;
        download_verified(
            &client().unwrap(),
            &url,
            &part,
            1 << 20,
            Some(&sha_of(FILE)),
            &cancel,
            &mut |r, t| {
                first.get_or_insert((r, t));
                last = (r, t);
            },
        )
        .await
        .unwrap();
        assert_eq!(first, Some((5, 10)));
        assert_eq!(last, (10, 10));
    }

    #[test]
    fn only_one_download_runs_at_a_time() {
        let first = ActiveDownload::begin().unwrap();
        assert!(ActiveDownload::begin()
            .err()
            .unwrap()
            .contains("update_busy"));
        drop(first);
        drop(ActiveDownload::begin().unwrap());
    }

    #[test]
    fn answers_are_read_by_where_their_bytes_start() {
        assert_eq!(read_answer(StatusCode::OK, None, 0), Answer::Whole);
        assert_eq!(read_answer(StatusCode::OK, None, 5), Answer::Whole);
        assert_eq!(
            read_answer(StatusCode::PARTIAL_CONTENT, Some("bytes 5-9/10"), 5),
            Answer::Rest { total: 10 }
        );
        assert_eq!(
            read_answer(StatusCode::PARTIAL_CONTENT, Some("bytes 5-9/*"), 5),
            Answer::Rest { total: 0 }
        );
        // Another offset, or nothing to say where it starts: not usable.
        assert_eq!(
            read_answer(StatusCode::PARTIAL_CONTENT, Some("bytes 0-9/10"), 5),
            Answer::Unusable
        );
        assert_eq!(
            read_answer(StatusCode::PARTIAL_CONTENT, None, 5),
            Answer::Unusable
        );
        assert_eq!(
            range_refused_total(Some("bytes */10")),
            Some(10),
            "a 416 says the size"
        );
        assert_eq!(range_refused_total(None), None);
    }
}
