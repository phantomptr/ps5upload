use anyhow::Result;
use lz4_flex::block::compress_prepend_size;
use lzma_rs::lzma_compress;
use std::fs::File;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use walkdir::WalkDir;
use zstd::bulk::compress as zstd_compress;

// Optimized for 10 worker processes on payload side
const PACK_BUFFER_SIZE: usize = 16 * 1024 * 1024; // 16MB Packs
const MAGIC_FTX1: u32 = 0x31585446;
const SEND_CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4MB write chunks for better throughput
const PIPELINE_DEPTH: usize = 5; // Reduced to 5 to match server queue depth and save RAM (5 * 16MB = 80MB)
const ARCHIVE_READ_BUFFER_SIZE: usize = 1024 * 1024; // 1MB streaming buffer for archives
const WRITE_IDLE_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub size: u64,
    pub mtime: Option<i64>,
}

#[derive(Debug)]
enum FrameType {
    Pack = 4,
    PackLz4 = 8,
    PackZstd = 9,
    PackLzma = 10,
    Finish = 6,
}

// Sent from Packer to Sender
struct ReadyPack {
    buffer: Vec<u8>,
    bytes_in_pack: u64,
    files_in_pack: i32,
}

#[derive(Debug, Clone, Copy)]
pub enum CompressionMode {
    None,
    Lz4,
    Zstd,
    Lzma,
}

struct RateLimiter {
    limit_bps: Option<u64>,
    start: std::time::Instant,
    sent: u64,
}

impl RateLimiter {
    fn new(limit_bps: Option<u64>) -> Self {
        Self {
            limit_bps,
            start: std::time::Instant::now(),
            sent: 0,
        }
    }

    fn throttle(&mut self, bytes: u64) {
        let Some(limit) = self.limit_bps else {
            return;
        };
        if limit == 0 {
            return;
        }
        self.sent += bytes;
        let elapsed = self.start.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }
        let expected = self.sent as f64 / limit as f64;
        if expected > elapsed {
            let sleep_secs = expected - elapsed;
            std::thread::sleep(std::time::Duration::from_secs_f64(sleep_secs.min(0.5)));
        }
    }
}

struct SendContext<'a, F>
where
    F: FnMut(u64, i32, Option<String>),
{
    stream: &'a mut std::net::TcpStream,
    cancel: &'a Arc<AtomicBool>,
    limiter: &'a mut RateLimiter,
    progress: &'a mut F,
    total_sent_bytes: &'a mut u64,
    total_sent_files: &'a mut i32,
    last_progress_sent: &'a mut u64,
}

impl<'a, F> SendContext<'a, F>
where
    F: FnMut(u64, i32, Option<String>),
{
    fn send_pack_inline(&mut self, pack: &PackBuffer, current_file: Option<String>) -> Result<()> {
        send_frame_header(
            self.stream,
            FrameType::Pack,
            pack.buffer.len() as u64,
            self.cancel,
        )?;
        let mut sent_payload = 0usize;
        let pack_len = pack.buffer.len();
        while sent_payload < pack_len {
            if self.cancel.load(Ordering::Relaxed) {
                let _ = self.stream.shutdown(std::net::Shutdown::Both);
                return Err(anyhow::anyhow!("Upload cancelled"));
            }
            let end = std::cmp::min(sent_payload + SEND_CHUNK_SIZE, pack_len);
            write_all_retry(self.stream, &pack.buffer[sent_payload..end], self.cancel)?;
            self.limiter.throttle((end - sent_payload) as u64);
            sent_payload = end;
            let approx =
                (pack.bytes_added as u128 * sent_payload as u128 / pack_len as u128) as u64;
            let approx_total = *self.total_sent_bytes + approx;
            if approx_total != *self.last_progress_sent {
                (self.progress)(approx_total, *self.total_sent_files, current_file.clone());
                *self.last_progress_sent = approx_total;
            }
        }
        Ok(())
    }
}

struct PackBuffer {
    buffer: Vec<u8>,
    bytes_added: u64,
    files_added: i32,
}

impl PackBuffer {
    fn new() -> Self {
        let mut buffer = Vec::with_capacity(PACK_BUFFER_SIZE);
        buffer.extend_from_slice(&[0u8; 4]);
        Self {
            buffer,
            bytes_added: 0,
            files_added: 0,
        }
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.buffer.extend_from_slice(&[0u8; 4]);
        self.bytes_added = 0;
        self.files_added = 0;
    }

    fn take_ready_pack(&mut self) -> ReadyPack {
        let mut buffer = Vec::new();
        std::mem::swap(&mut buffer, &mut self.buffer);
        let ready = ReadyPack {
            buffer,
            bytes_in_pack: self.bytes_added,
            files_in_pack: self.files_added,
        };
        self.buffer = Vec::with_capacity(PACK_BUFFER_SIZE);
        self.buffer.extend_from_slice(&[0u8; 4]);
        self.bytes_added = 0;
        self.files_added = 0;
        ready
    }

    fn record_count(&self) -> u32 {
        u32::from_le_bytes(self.buffer[0..4].try_into().unwrap())
    }

    fn set_record_count(&mut self, count: u32) {
        self.buffer[0..4].copy_from_slice(&count.to_le_bytes());
    }

    fn can_fit(&self, path_len: usize, data_len: usize) -> bool {
        let overhead = 2 + path_len + 8;
        self.buffer.len() + overhead + data_len <= PACK_BUFFER_SIZE
    }

    fn add_record(&mut self, rel_path: &str, data: &[u8]) {
        let path_bytes = rel_path.as_bytes();
        let path_len = path_bytes.len() as u16;
        let data_len = data.len() as u64;

        self.buffer.extend_from_slice(&path_len.to_le_bytes());
        self.buffer.extend_from_slice(path_bytes);
        self.buffer.extend_from_slice(&data_len.to_le_bytes());
        self.buffer.extend_from_slice(data);

        let count = self.record_count();
        self.set_record_count(count + 1);
        self.bytes_added += data_len;
    }
}

fn pack_reader_into_stream<F, R>(
    reader: &mut R,
    rel_path: &str,
    pack: &mut PackBuffer,
    ctx: &mut SendContext<F>,
) -> Result<()>
where
    F: FnMut(u64, i32, Option<String>),
    R: Read + ?Sized,
{
    let mut buf = vec![0u8; ARCHIVE_READ_BUFFER_SIZE];
    let mut saw_data = false;
    let rel_path_string = rel_path.to_string();

    loop {
        if ctx.cancel.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Upload cancelled by user"));
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        saw_data = true;
        let mut offset = 0usize;
        while offset < n {
            if ctx.cancel.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("Upload cancelled by user"));
            }
            let overhead = 2 + rel_path.len() + 8;
            let remaining = PACK_BUFFER_SIZE.saturating_sub(pack.buffer.len());
            if remaining <= overhead {
                ctx.send_pack_inline(pack, Some(rel_path_string.clone()))?;
                *ctx.total_sent_bytes += pack.bytes_added;
                *ctx.total_sent_files += pack.files_added;
                pack.reset();
                continue;
            }
            let max_data = remaining - overhead;
            let chunk_size = std::cmp::min(max_data, n - offset);
            pack.add_record(rel_path, &buf[offset..offset + chunk_size]);
            offset += chunk_size;
        }
    }

    if !saw_data {
        if !pack.can_fit(rel_path.len(), 0) {
            ctx.send_pack_inline(pack, Some(rel_path_string.clone()))?;
            *ctx.total_sent_bytes += pack.bytes_added;
            *ctx.total_sent_files += pack.files_added;
            pack.reset();
        }
        pack.add_record(rel_path, &[]);
    }
    pack.files_added += 1;
    Ok(())
}

fn write_all_retry(
    stream: &mut std::net::TcpStream,
    data: &[u8],
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let mut offset = 0usize;
    let mut last_progress = std::time::Instant::now();
    while offset < data.len() {
        if cancel.load(Ordering::Relaxed) {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(anyhow::anyhow!("Upload cancelled by user"));
        }
        if last_progress.elapsed() >= std::time::Duration::from_secs(WRITE_IDLE_TIMEOUT_SECS) {
            return Err(anyhow::anyhow!("Upload timed out waiting for socket write"));
        }
        match stream.write(&data[offset..]) {
            Ok(0) => return Err(anyhow::anyhow!("Socket closed during send")),
            Ok(n) => {
                offset += n;
                last_progress = std::time::Instant::now();
            }
            Err(err) => match err.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => {
                    thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                _ => return Err(err.into()),
            },
        }
    }
    Ok(())
}

fn send_frame_header(
    stream: &mut std::net::TcpStream,
    ftype: FrameType,
    len: u64,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    write_all_retry(stream, &MAGIC_FTX1.to_le_bytes(), cancel)?;
    write_all_retry(stream, &(ftype as u32).to_le_bytes(), cancel)?;
    write_all_retry(stream, &len.to_le_bytes(), cancel)?;
    Ok(())
}

#[allow(dead_code)]
fn collect_files(base_path: &str) -> Vec<FileEntry> {
    collect_files_with_progress(base_path, Arc::new(AtomicBool::new(false)), |_, _| {}).0
}

/// Collect files with progress reporting and cancellation support.
/// Returns (files, was_cancelled).
/// The progress callback receives (files_found, total_size_so_far) and is called every 1000 files.
pub fn collect_files_with_progress<F>(
    base_path: &str,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> (Vec<FileEntry>, bool)
where
    F: FnMut(usize, u64),
{
    let path = Path::new(base_path);
    let mut files = Vec::new();
    let mut total_size: u64 = 0;

    if path.is_file() {
        if let Ok(meta) = path.metadata() {
            let rel_path = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            let size = meta.len();
            files.push(FileEntry {
                rel_path,
                abs_path: path.to_path_buf(),
                size,
                mtime: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64),
            });
            progress(1, size);
        }
        return (files, false);
    }

    let mut last_report = 0usize;
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        // Check cancellation every iteration
        if cancel.load(Ordering::Relaxed) {
            return (files, true);
        }

        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let size = meta.len();
        let rel_path = entry_path
            .strip_prefix(path)
            .unwrap_or(entry_path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push(FileEntry {
            rel_path,
            abs_path: entry_path.to_path_buf(),
            size,
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64),
        });
        total_size += size;

        // Report progress every 1000 files
        if files.len() - last_report >= 1000 {
            progress(files.len(), total_size);
            last_report = files.len();
        }
    }

    // Final progress report
    progress(files.len(), total_size);
    (files, false)
}

/// Stream files with progress reporting and cancellation support.
/// Returns a Receiver that yields FileEntry items.
pub fn stream_files_with_progress<F>(
    base_path: String,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> std::sync::mpsc::Receiver<FileEntry>
where
    F: FnMut(usize, u64) + Send + 'static,
{
    let (tx, rx) = sync_channel(10_000);
    thread::spawn(move || {
        let path = Path::new(&base_path);
        let mut total_size: u64 = 0;
        let mut file_count: usize = 0;

        if path.is_file() {
            if let Ok(meta) = path.metadata() {
                let rel_path = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let size = meta.len();
                let entry = FileEntry {
                    rel_path,
                    abs_path: path.to_path_buf(),
                    size,
                    mtime: meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64),
                };
                progress(1, size);
                let _ = tx.send(entry);
            }
            return;
        }

        let mut last_report = 0usize;
        for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            let entry_path = entry.path();
            if !entry_path.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let size = meta.len();
            let rel_path = entry_path
                .strip_prefix(path)
                .unwrap_or(entry_path)
                .to_string_lossy()
                .replace('\\', "/");

            let entry = FileEntry {
                rel_path,
                abs_path: entry_path.to_path_buf(),
                size,
                mtime: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64),
            };

            total_size += size;
            file_count += 1;

            if tx.send(entry).is_err() {
                break; // Receiver dropped
            }

            // Report progress every 1000 files
            if file_count - last_report >= 1000 {
                progress(file_count, total_size);
                last_report = file_count;
            }
        }
        progress(file_count, total_size);
    });

    rx
}

pub fn send_files_v2_for_list<I, F, L>(
    files: I,
    mut stream: std::net::TcpStream,
    config: SendFilesConfig<F, L>,
) -> Result<()>
where
    I: IntoIterator<Item = FileEntry> + Send + 'static,
    I::IntoIter: Send + 'static,
    F: FnMut(u64, i32, Option<String>),
    L: Fn(String) + Send + Sync + 'static,
{
    let SendFilesConfig {
        cancel,
        mut progress,
        log,
        worker_id,
        allowed_connections,
        compression,
        rate_limit_bps,
    } = config;

    // Optimize socket for high throughput
    let _ = stream.set_nodelay(true);

    // Increase TCP buffer sizes to 16MB for better throughput
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();
        unsafe {
            let buf_size: libc::c_int = 16 * 1024 * 1024; // 16MB
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &buf_size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

    // Windows socket buffer configuration
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        let socket = stream.as_raw_socket();
        unsafe {
            let buf_size: libc::c_int = 16 * 1024 * 1024; // 16MB

            // On Windows: SOL_SOCKET = 0xFFFF, SO_SNDBUF = 0x1001, SO_RCVBUF = 0x1002
            const SOL_SOCKET: libc::c_int = 0xFFFF;
            const SO_SNDBUF: libc::c_int = 0x1001;
            const SO_RCVBUF: libc::c_int = 0x1002;

            libc::setsockopt(
                socket as usize,
                SOL_SOCKET,
                SO_SNDBUF,
                &buf_size as *const libc::c_int as *const libc::c_char,
                std::mem::size_of::<libc::c_int>() as libc::c_int,
            );
            libc::setsockopt(
                socket as usize,
                SOL_SOCKET,
                SO_RCVBUF,
                &buf_size as *const libc::c_int as *const libc::c_char,
                std::mem::size_of::<libc::c_int>() as libc::c_int,
            );
        }
    }

    // Channel for Pipelining: Packer -> Sender
    // Deep pipeline to keep all 10 payload workers saturated
    let (tx, rx) = sync_channel::<ReadyPack>(PIPELINE_DEPTH);
    let cancel_packer = cancel.clone();
    let log_packer = Arc::new(log);
    let log_packer_clone = log_packer.clone();
    let pack_error = Arc::new(AtomicBool::new(false));
    let pack_error_msg = Arc::new(Mutex::new(String::new()));
    let pack_error_packer = pack_error.clone();
    let pack_error_msg_packer = pack_error_msg.clone();
    let current_file = Arc::new(Mutex::new(String::new()));
    let current_file_packer = current_file.clone();

    // Packer Thread
    thread::spawn(move || {
        let mut pack = PackBuffer::new();

        for entry in files {
            if cancel_packer.load(Ordering::Relaxed) {
                break;
            }

            let rel_path_str = entry.rel_path;
            if let Ok(mut guard) = current_file_packer.lock() {
                *guard = rel_path_str.clone();
            }
            (log_packer_clone)(format!("Packing: {}", rel_path_str));

            let file_size = entry.size;
            let mut file = match File::open(&entry.abs_path) {
                Ok(file) => file,
                Err(err) => {
                    let msg = format!("Failed to open file {}: {}", rel_path_str, err);
                    if let Ok(mut guard) = pack_error_msg_packer.lock() {
                        *guard = msg.clone();
                    }
                    pack_error_packer.store(true, Ordering::Relaxed);
                    (log_packer_clone)(msg);
                    cancel_packer.store(true, Ordering::Relaxed);
                    break;
                }
            };

            // Empty file case
            if file_size == 0 {
                if !pack.can_fit(rel_path_str.len(), 0) {
                    let _ = tx.send(pack.take_ready_pack());
                }
                pack.add_record(&rel_path_str, &[]);
                pack.files_added += 1;
                continue;
            }

            let mut file_remaining = file_size;
            let mut chunk_buf: Vec<u8> = Vec::new();
            while file_remaining > 0 {
                if cancel_packer.load(Ordering::Relaxed) {
                    break;
                }

                let overhead = 2 + rel_path_str.len() + 8;
                let remaining = PACK_BUFFER_SIZE.saturating_sub(pack.buffer.len());

                if remaining <= overhead {
                    let _ = tx.send(pack.take_ready_pack());
                    continue;
                }

                let max_data = remaining - overhead;
                let to_read = std::cmp::min(max_data as u64, file_remaining) as usize;
                if to_read == 0 {
                    let _ = tx.send(pack.take_ready_pack());
                    continue;
                }

                chunk_buf.resize(to_read, 0u8);
                let mut filled = 0usize;
                let mut read_failed = false;
                while filled < to_read {
                    match file.read(&mut chunk_buf[filled..to_read]) {
                        Ok(0) => {
                            let msg = format!(
                                "Failed to read file {}: file size changed during upload",
                                rel_path_str
                            );
                            if let Ok(mut guard) = pack_error_msg_packer.lock() {
                                *guard = msg.clone();
                            }
                            pack_error_packer.store(true, Ordering::Relaxed);
                            (log_packer_clone)(msg);
                            cancel_packer.store(true, Ordering::Relaxed);
                            read_failed = true;
                            break;
                        }
                        Ok(n) => filled += n,
                        Err(err) => {
                            let msg = format!("Failed to read file {}: {}", rel_path_str, err);
                            if let Ok(mut guard) = pack_error_msg_packer.lock() {
                                *guard = msg.clone();
                            }
                            pack_error_packer.store(true, Ordering::Relaxed);
                            (log_packer_clone)(msg);
                            cancel_packer.store(true, Ordering::Relaxed);
                            read_failed = true;
                            break;
                        }
                    }
                }
                if read_failed {
                    break;
                }
                pack.add_record(&rel_path_str, &chunk_buf[..to_read]);
                file_remaining -= to_read as u64;
            }
            // Finished file
            pack.files_added += 1;
        }

        // Flush remaining
        if pack.record_count() > 0 {
            let _ = tx.send(pack.take_ready_pack());
        }
    });

    // Sender Loop (Main Thread)
    let mut total_sent_bytes = 0u64;
    let mut total_sent_files = 0i32;

    let mut last_progress_sent = 0u64;
    let mut last_progress_file = String::new();

    let mut limiter = RateLimiter::new(rate_limit_bps);
    for ready_pack in rx {
        if let Some(allowed) = &allowed_connections {
            while worker_id >= allowed.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    let _ = stream.shutdown(Shutdown::Both);
                    return Err(anyhow::anyhow!("Upload cancelled by user"));
                }
                thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(anyhow::anyhow!("Upload cancelled by user"));
        }

        let (frame_type, payload) = match compression {
            CompressionMode::Lz4 => {
                let compressed = compress_prepend_size(&ready_pack.buffer);
                if compressed.len() >= ready_pack.buffer.len() {
                    (FrameType::Pack, ready_pack.buffer)
                } else {
                    (FrameType::PackLz4, compressed)
                }
            }
            CompressionMode::Zstd => {
                let compressed = zstd_compress(&ready_pack.buffer, 19).unwrap_or_default();
                if compressed.is_empty() || compressed.len() + 4 >= ready_pack.buffer.len() {
                    (FrameType::Pack, ready_pack.buffer)
                } else {
                    let mut payload = Vec::with_capacity(compressed.len() + 4);
                    payload.extend_from_slice(&(ready_pack.buffer.len() as u32).to_le_bytes());
                    payload.extend_from_slice(&compressed);
                    (FrameType::PackZstd, payload)
                }
            }
            CompressionMode::Lzma => {
                let mut compressed = Vec::new();
                let cursor = std::io::Cursor::new(&ready_pack.buffer);
                let mut input = std::io::BufReader::new(cursor);
                let lzma_ok = lzma_compress(&mut input, &mut compressed).is_ok();
                if !lzma_ok || compressed.len() + 4 >= ready_pack.buffer.len() {
                    (FrameType::Pack, ready_pack.buffer)
                } else {
                    let mut payload = Vec::with_capacity(compressed.len() + 4);
                    payload.extend_from_slice(&(ready_pack.buffer.len() as u32).to_le_bytes());
                    payload.extend_from_slice(&compressed);
                    (FrameType::PackLzma, payload)
                }
            }
            CompressionMode::None => (FrameType::Pack, ready_pack.buffer),
        };

        send_frame_header(&mut stream, frame_type, payload.len() as u64, &cancel)?;

        let mut sent_payload = 0usize;
        let pack_len = payload.len();
        while sent_payload < pack_len {
            if cancel.load(Ordering::Relaxed) {
                let _ = stream.shutdown(Shutdown::Both);
                return Err(anyhow::anyhow!("Upload cancelled by user"));
            }
            let end = std::cmp::min(sent_payload + SEND_CHUNK_SIZE, pack_len);
            write_all_retry(&mut stream, &payload[sent_payload..end], &cancel)?;
            limiter.throttle((end - sent_payload) as u64);
            sent_payload = end;

            let approx = if pack_len > 0 {
                (ready_pack.bytes_in_pack as u128 * sent_payload as u128 / pack_len as u128) as u64
            } else {
                0
            };
            let approx_total = total_sent_bytes + approx;
            if approx_total != last_progress_sent {
                let mut file_update = None;
                if let Ok(guard) = current_file.lock() {
                    if *guard != last_progress_file {
                        last_progress_file = guard.clone();
                        file_update = Some(last_progress_file.clone());
                    }
                }
                progress(approx_total, total_sent_files, file_update);
                last_progress_sent = approx_total;
            }
        }

        total_sent_bytes += ready_pack.bytes_in_pack;
        total_sent_files += ready_pack.files_in_pack;

        let mut file_update = None;
        if let Ok(guard) = current_file.lock() {
            if *guard != last_progress_file {
                last_progress_file = guard.clone();
                file_update = Some(last_progress_file.clone());
            }
        }
        progress(total_sent_bytes, total_sent_files, file_update);
        last_progress_sent = total_sent_bytes;
    }

    if pack_error.load(Ordering::Relaxed) {
        let msg = pack_error_msg
            .lock()
            .ok()
            .and_then(|g| if g.is_empty() { None } else { Some(g.clone()) })
            .unwrap_or_else(|| "Upload failed while reading files".to_string());
        return Err(anyhow::anyhow!(msg));
    }

    send_frame_header(&mut stream, FrameType::Finish, 0, &cancel)?;

    Ok(())
}

pub struct SharedReceiverIterator {
    rx: Arc<Mutex<std::sync::mpsc::Receiver<FileEntry>>>,
}

impl SharedReceiverIterator {
    pub fn new(rx: Arc<Mutex<std::sync::mpsc::Receiver<FileEntry>>>) -> Self {
        Self { rx }
    }
}

impl Iterator for SharedReceiverIterator {
    type Item = FileEntry;
    fn next(&mut self) -> Option<Self::Item> {
        self.rx.lock().ok()?.recv().ok()
    }
}

pub struct SendFilesConfig<F, L>
where
    F: FnMut(u64, i32, Option<String>),
    L: Fn(String) + Send + Sync + 'static,
{
    pub cancel: Arc<AtomicBool>,
    pub progress: F,
    pub log: L,
    pub worker_id: usize,
    pub allowed_connections: Option<Arc<AtomicUsize>>,
    pub compression: CompressionMode,
    pub rate_limit_bps: Option<u64>,
}

pub fn scan_zip_archive(path: &str) -> Result<(usize, u64)> {
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut count = 0;
    let mut size = 0;
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        if file.is_file() {
            count += 1;
            size += file.size();
        }
    }
    Ok((count, size))
}

pub fn scan_7z_archive(path: &str) -> Result<(usize, u64)> {
    let mut count = 0;
    let mut size = 0;
    sevenz_rust::decompress_file_with_extract_fn(
        path,
        "",
        |entry: &sevenz_rust::SevenZArchiveEntry, _, _| {
            if !entry.is_directory() {
                count += 1;
                size += entry.size();
            }
            Ok(true)
        },
    )?;
    Ok((count, size))
}

pub fn send_zip_archive<F, L>(
    path: String,
    mut stream: std::net::TcpStream,
    cancel: Arc<AtomicBool>,
    mut progress: F,
    log: L,
    rate_limit_bps: Option<u64>,
) -> Result<()>
where
    F: FnMut(u64, i32, Option<String>),
    L: Fn(String),
{
    let file = File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut pack = PackBuffer::new();
    let mut total_sent_bytes = 0u64;
    let mut total_sent_files = 0i32;
    let mut last_progress_sent = 0u64;
    let mut limiter = RateLimiter::new(rate_limit_bps);
    let mut ctx = SendContext {
        stream: &mut stream,
        cancel: &cancel,
        limiter: &mut limiter,
        progress: &mut progress,
        total_sent_bytes: &mut total_sent_bytes,
        total_sent_files: &mut total_sent_files,
        last_progress_sent: &mut last_progress_sent,
    };

    for i in 0..archive.len() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut file = archive.by_index(i)?;
        if !file.is_file() {
            continue;
        }

        let rel_path = file.name().replace('\\', "/");
        log(format!("Packing: {}", rel_path));
        pack_reader_into_stream(&mut file, &rel_path, &mut pack, &mut ctx)?;
    }
    if pack.record_count() > 0 {
        ctx.send_pack_inline(&pack, None)?;
    }
    send_frame_header(&mut stream, FrameType::Finish, 0, &cancel)?;
    Ok(())
}

pub fn send_7z_archive<F, L>(
    path: String,
    mut stream: std::net::TcpStream,
    cancel: Arc<AtomicBool>,
    mut progress: F,
    log: L,
    rate_limit_bps: Option<u64>,
) -> Result<()>
where
    F: FnMut(u64, i32, Option<String>),
    L: Fn(String),
{
    let mut pack = PackBuffer::new();
    let mut total_sent_bytes = 0u64;
    let mut total_sent_files = 0i32;
    let mut last_progress_sent = 0u64;
    let mut limiter = RateLimiter::new(rate_limit_bps);
    let mut ctx = SendContext {
        stream: &mut stream,
        cancel: &cancel,
        limiter: &mut limiter,
        progress: &mut progress,
        total_sent_bytes: &mut total_sent_bytes,
        total_sent_files: &mut total_sent_files,
        last_progress_sent: &mut last_progress_sent,
    };

    sevenz_rust::decompress_file_with_extract_fn(
        &path,
        "",
        |entry: &sevenz_rust::SevenZArchiveEntry, reader: &mut dyn std::io::Read, _| {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            if entry.is_directory() {
                return Ok(true);
            }

            let rel_path = entry.name().replace('\\', "/");
            log(format!("Packing: {}", rel_path));

            if let Err(e) = pack_reader_into_stream(reader, &rel_path, &mut pack, &mut ctx) {
                return Err(std::io::Error::other(e.to_string()).into());
            }
            Ok(true)
        },
    )?;

    if pack.record_count() > 0 {
        ctx.send_pack_inline(&pack, None)?;
    }
    send_frame_header(&mut stream, FrameType::Finish, 0, &cancel)?;
    Ok(())
}
