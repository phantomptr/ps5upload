//! Serve a package on a saved server to the console's installer, in byte ranges.
//!
//! The console pulls a package over HTTP in ranges and the pkg-host answers each one from
//! wherever the bytes live. This is that source for any saved server — the SMB share streaming
//! of `smb_range` generalised to every protocol. Nothing is copied: each range becomes a few
//! positioned reads run concurrently, because one read at a time would be bound by round trips.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::pool::{Backoff, Pool};
use super::store::Store;
use super::{RemoteError, RemoteFile};

/// Size of each positioned read within one range.
const PIECE: u64 = 4 * 1024 * 1024;
/// How many run at once.
const PIECES_IN_FLIGHT: usize = 4;

pub struct RemoteRangeSource {
    handle: tokio::runtime::Handle,
    file: Arc<dyn RemoteFile>,
    size: u64,
    /// Server host, for logs. Never the path or credentials.
    host: String,
    origin_bytes: AtomicU64,
    origin_nanos: AtomicU64,
}

impl std::fmt::Debug for RemoteRangeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteRangeSource")
            .field("host", &self.host)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl RemoteRangeSource {
    /// Sign in (or reuse the session), open the file, and keep it for the install.
    pub async fn open(
        pool: Arc<Pool>,
        store: Arc<Store>,
        remote_path: &str,
        backoff: Backoff,
    ) -> Result<Self, RemoteError> {
        let p = super::path::parse(remote_path)?;
        let file = pool
            .with_fs(&store, &p.connection_id, |fs| {
                let path = p.path.clone();
                async move { fs.open(&path).await }
            })
            .await?;
        let host = store
            .get(&p.connection_id)
            .map(|(c, _)| c.host)
            .unwrap_or_default();
        let size = file.size();
        let file = super::pool::retrying(pool, store, p.connection_id, p.path, file, backoff);
        Ok(Self {
            handle: tokio::runtime::Handle::current(),
            file,
            size,
            host,
            origin_bytes: AtomicU64::new(0),
            origin_nanos: AtomicU64::new(0),
        })
    }

    pub fn total_size(&self) -> u64 {
        self.size
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// Bytes `[start, end]` inclusive. Called from the pkg-host's blocking producer thread,
    /// never from a runtime worker, so blocking on the runtime here is allowed.
    pub fn read_range(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        if end < start || start >= self.size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("range {start}-{end} outside a {}-byte file", self.size),
            ));
        }
        let end = end.min(self.size - 1);
        let began = std::time::Instant::now();
        let bytes = self
            .handle
            .block_on(read_pieces(Arc::clone(&self.file), start, end))?;
        self.origin_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.origin_nanos
            .fetch_add(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    /// Average read throughput from the server so far, for the install status.
    pub fn origin_rate_bps(&self) -> Option<u64> {
        let bytes = self.origin_bytes.load(Ordering::Relaxed);
        let nanos = self.origin_nanos.load(Ordering::Relaxed);
        if bytes == 0 || nanos == 0 {
            return None;
        }
        Some(((bytes as u128 * 1_000_000_000u128) / nanos as u128) as u64)
    }
}

/// Split `[start, end]` into `PIECE`-sized positioned reads, run up to `PIECES_IN_FLIGHT` at a
/// time, and reassemble them in order. A short read is an error: never hand the installer a hole.
async fn read_pieces(file: Arc<dyn RemoteFile>, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let mut pieces = Vec::new();
    let mut off = start;
    while off <= end {
        let len = PIECE.min(end - off + 1);
        pieces.push((off, len));
        off += len;
    }
    let mut out = Vec::with_capacity((end - start + 1) as usize);
    for batch in pieces.chunks(PIECES_IN_FLIGHT) {
        let mut tasks = Vec::with_capacity(batch.len());
        for &(off, len) in batch {
            let f = Arc::clone(&file);
            tasks.push(tokio::spawn(async move { f.read_at(off, len).await }));
        }
        for (t, &(off, len)) in tasks.into_iter().zip(batch) {
            let got = t
                .await
                .map_err(|e| std::io::Error::other(format!("remote read task: {e}")))?
                .map_err(|e| std::io::Error::other(format!("remote read at {off}: {e}")))?;
            if got.len() as u64 != len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("short remote read at {off}: {} of {len} bytes", got.len()),
                ));
            }
            out.extend_from_slice(&got);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::pool::testing::{remote_with, remote_with_shared};
    use crate::remote::store::{conn, Protocol, Secret};
    use crate::remote::MemFs;

    fn setup(fs: Arc<MemFs>) -> (Arc<Pool>, Arc<Store>, String) {
        let r = remote_with_shared(fs, None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        (Arc::clone(&r.pool), Arc::clone(&r.store), id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn serves_ranges_byte_for_byte() {
        let data: Vec<u8> = (0..(9 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
        let (pool, store, id) = setup(Arc::new(MemFs::new(&[("/g/a.pkg", &data)])));
        let src = RemoteRangeSource::open(
            pool,
            store,
            &format!("remote://{id}/g/a.pkg"),
            Backoff::instant(),
        )
        .await
        .unwrap();
        assert_eq!(src.total_size(), data.len() as u64);
        assert_eq!(src.host(), "10.0.0.5");
        let end = 5 * 1024 * 1024;
        let got = tokio::task::spawn_blocking(move || src.read_range(1000, end))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, data[1000..=end as usize]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_read_is_retried() {
        let mem = Arc::new(MemFs::new(&[("/a.pkg", &[7u8; 4096])]));
        let (pool, store, id) = setup(Arc::clone(&mem));
        let src = RemoteRangeSource::open(
            pool,
            store,
            &format!("remote://{id}/a.pkg"),
            Backoff::instant(),
        )
        .await
        .unwrap();
        mem.fail_next_reads(2);
        let got = tokio::task::spawn_blocking(move || src.read_range(0, 4095))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.len(), 4096);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_range_outside_the_file_is_refused() {
        let (pool, store, id) = setup(Arc::new(MemFs::new(&[("/a.pkg", &[1u8; 10])])));
        let src = RemoteRangeSource::open(
            pool,
            store,
            &format!("remote://{id}/a.pkg"),
            Backoff::instant(),
        )
        .await
        .unwrap();
        let r = tokio::task::spawn_blocking(move || src.read_range(20, 30))
            .await
            .unwrap();
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn a_deleted_connection_fails_before_anything_starts() {
        let r = remote_with(MemFs::new(&[]), None);
        let e = RemoteRangeSource::open(
            Arc::clone(&r.pool),
            Arc::clone(&r.store),
            "remote://gone-0000/a.pkg",
            Backoff::instant(),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("no longer exists"), "{e}");
    }
}
