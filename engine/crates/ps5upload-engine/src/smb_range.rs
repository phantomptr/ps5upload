//! Serve an install straight from a file on an SMB share, in byte ranges.
//!
//! The console's installer pulls a package over HTTP in ranges; the pkg-host
//! answers each range from wherever the bytes live. For a link that is
//! [`crate::remote_pkg::RemoteSource`]; this is the same thing for a file on a
//! Samba / Windows share, so an SMB install needs no copy at all.
//!
//! Before this, installing from SMB meant copying the whole package to this
//! computer, uploading that copy to the console, and installing the upload —
//! the package on disk twice more before the install could even start. itsPLK's
//! PKG Manager streams SMB into the installer instead (reading the share on the
//! console with libsmb2, ~110 MB/s on a LAN). This does the same from this
//! computer: nothing staged anywhere, and the install starts at once.
//!
//! One SMB connection and one open handle per install. Each range is split into
//! positioned reads that run concurrently, because a single `read_at` issues
//! its wire READs one after another and would be bound by round trips rather
//! than by the link.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Size of each concurrent positioned read within one range.
const PIECE: u64 = 4 * 1024 * 1024;
/// How many of those run at once. The console asks for 16 MiB ranges on two
/// connections, so this keeps ~8 reads in flight against the share.
const PIECES_IN_FLIGHT: usize = 4;

pub struct SmbRangeSource {
    handle: tokio::runtime::Handle,
    reader: Arc<smb2::FileReader>,
    size: u64,
    /// Share host, for logs. Never the path or credentials.
    host: String,
    origin_bytes: AtomicU64,
    origin_nanos: AtomicU64,
}

impl std::fmt::Debug for SmbRangeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmbRangeSource")
            .field("host", &self.host)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl SmbRangeSource {
    /// Connect, open `path` on `share`, and keep the handle for the install.
    pub async fn open(
        server: &str,
        share: &str,
        user: &str,
        password: &str,
        path: &str,
    ) -> anyhow::Result<Self> {
        // Same normalisation as the SMB browser, so whatever the user typed
        // there (smb://nas/share, a bare host, host:port) works here too.
        let server = crate::smb::normalize_smb_server(server)?;
        let mut client = smb2::connect(&server, user, password)
            .await
            .map_err(|e| anyhow::anyhow!("connect to {server}: {e}"))?;
        let tree = client
            .connect_share(share)
            .await
            .map_err(|e| anyhow::anyhow!("open share {share}: {e}"))?;
        let reader = client
            .open_file_reader(&tree, path)
            .await
            .map_err(|e| anyhow::anyhow!("open {path}: {e}"))?;
        let size = reader.size();
        Ok(Self {
            handle: tokio::runtime::Handle::current(),
            reader: Arc::new(reader),
            size,
            host: server,
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

    /// Bytes `[start, end]` inclusive. Called from the pkg-host's blocking
    /// producer thread, never from the async reactor, so blocking on the
    /// runtime here is allowed (`Handle::block_on` panics only on a runtime
    /// worker).
    pub fn read_range(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        if end < start || start >= self.size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("range {start}-{end} outside a {}-byte file", self.size),
            ));
        }
        let end = end.min(self.size - 1);
        let began = Instant::now();
        let reader = Arc::clone(&self.reader);
        let bytes = self.handle.block_on(read_pieces(reader, start, end))?;
        self.origin_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.origin_nanos
            .fetch_add(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
        Ok(bytes)
    }

    /// Average read throughput from the share so far, for the install status.
    pub fn origin_rate_bps(&self) -> Option<u64> {
        let bytes = self.origin_bytes.load(Ordering::Relaxed);
        let nanos = self.origin_nanos.load(Ordering::Relaxed);
        if bytes == 0 || nanos == 0 {
            return None;
        }
        Some(((bytes as u128 * 1_000_000_000u128) / nanos as u128) as u64)
    }
}

/// Split `[start, end]` into `PIECE`-sized positioned reads, run up to
/// `PIECES_IN_FLIGHT` at a time, and reassemble them in order.
async fn read_pieces(
    reader: Arc<smb2::FileReader>,
    start: u64,
    end: u64,
) -> std::io::Result<Vec<u8>> {
    let ranges = piece_ranges(start, end, PIECE);
    let mut out = Vec::with_capacity((end - start + 1) as usize);
    for batch in ranges.chunks(PIECES_IN_FLIGHT) {
        let mut tasks = Vec::with_capacity(batch.len());
        for &(off, len) in batch {
            let r = Arc::clone(&reader);
            tasks.push(tokio::spawn(async move { r.read_at(off, len).await }));
        }
        for (t, &(off, len)) in tasks.into_iter().zip(batch) {
            let got = t
                .await
                .map_err(|e| std::io::Error::other(format!("smb read task: {e}")))?
                .map_err(|e| std::io::Error::other(format!("smb read at {off}: {e}")))?;
            if got.len() as u64 != len {
                // read_at only comes back short at end of file, and the range
                // was clamped to the file, so a short read here is the share
                // misbehaving — never hand the installer a hole.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("short smb read at {off}: {} of {len} bytes", got.len()),
                ));
            }
            out.extend_from_slice(&got);
        }
    }
    Ok(out)
}

/// `(offset, len)` pieces covering `[start, end]` inclusive.
fn piece_ranges(start: u64, end: u64, piece: u64) -> Vec<(u64, u64)> {
    let mut v = Vec::new();
    let mut off = start;
    while off <= end {
        let len = piece.min(end - off + 1);
        v.push((off, len));
        off += len;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::piece_ranges;

    #[test]
    fn pieces_cover_the_range_exactly_and_in_order() {
        let p = piece_ranges(10, 10 + 9 * 1024 * 1024 - 1, 4 * 1024 * 1024);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], (10, 4 * 1024 * 1024));
        assert_eq!(p[1].0, 10 + 4 * 1024 * 1024);
        let total: u64 = p.iter().map(|x| x.1).sum();
        assert_eq!(total, 9 * 1024 * 1024);
        assert_eq!(p[2].1, 1024 * 1024);
    }

    #[test]
    fn a_one_byte_range_is_one_piece() {
        assert_eq!(piece_ranges(5, 5, 4096), vec![(5, 1)]);
    }
}
