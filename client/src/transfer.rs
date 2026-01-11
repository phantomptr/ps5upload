use anyhow::Result;
use std::io::{Read, Write};
use std::fs::File;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use std::sync::mpsc::sync_channel;
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const PACK_BUFFER_SIZE: usize = 16 * 1024 * 1024; // 16MB Packs
const MAGIC_FTX1: u32 = 0x31585446;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub size: u64,
}

#[derive(Debug)]
enum FrameType {
    Pack = 4,
    Finish = 6,
}

// Sent from Packer to Sender
struct ReadyPack {
    buffer: Vec<u8>,
    bytes_in_pack: u64,
    files_in_pack: i32,
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

fn send_frame_header(stream: &mut std::net::TcpStream, ftype: FrameType, len: u64) -> Result<()> {
    stream.write_all(&MAGIC_FTX1.to_le_bytes())?;
    stream.write_all(&(ftype as u32).to_le_bytes())?;
    stream.write_all(&len.to_le_bytes())?;
    Ok(())
}

pub fn collect_files(base_path: &str) -> Vec<FileEntry> {
    let path = Path::new(base_path);
    let mut files = Vec::new();

    if path.is_file() {
        if let Ok(meta) = path.metadata() {
            let rel_path = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            files.push(FileEntry {
                rel_path,
                abs_path: path.to_path_buf(),
                size: meta.len(),
            });
        }
        return files;
    }

    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue; };
        let rel_path = entry_path
            .strip_prefix(path)
            .unwrap_or(entry_path)
            .to_string_lossy()
            .replace('\\', "/");
        files.push(FileEntry {
            rel_path,
            abs_path: entry_path.to_path_buf(),
            size: meta.len(),
        });
    }

    files
}

pub fn send_files_v2_for_list<F, L>(
    files: Vec<FileEntry>,
    mut stream: std::net::TcpStream,
    cancel: Arc<AtomicBool>,
    mut progress: F,
    log: L,
) -> Result<()>
where
    F: FnMut(u64, i32),
    L: Fn(String) + Send + Sync + 'static,
{
    // Optimize socket
    let _ = stream.set_nodelay(true);

    // Channel for Pipelining: Packer -> Sender
    let (tx, rx) = sync_channel::<ReadyPack>(2);
    let cancel_packer = cancel.clone();
    let log_packer = Arc::new(log);
    let log_packer_clone = log_packer.clone();

    // Packer Thread
    thread::spawn(move || {
        let mut pack = PackBuffer::new();
        
        for entry in files {
            if cancel_packer.load(Ordering::Relaxed) { break; }
            
            let rel_path_str = entry.rel_path;
            (log_packer_clone)(format!("Packing: {}", rel_path_str));

            let file_size = entry.size;
            let Ok(mut file) = File::open(entry.abs_path) else { continue; };
            
            // Empty file case
            if file_size == 0 {
                 if !pack.can_fit(rel_path_str.len(), 0) {
                     let _ = tx.send(ReadyPack { 
                         buffer: pack.buffer.clone(), 
                         bytes_in_pack: pack.bytes_added, 
                         files_in_pack: pack.files_added 
                     });
                     pack.reset();
                 }
                 pack.add_record(&rel_path_str, &[]);
                 pack.files_added += 1;
                 continue;
            }

            let mut file_remaining = file_size;
            while file_remaining > 0 {
                 if cancel_packer.load(Ordering::Relaxed) { break; }

                 let overhead = 2 + rel_path_str.len() + 8;
                 let max_data = PACK_BUFFER_SIZE - pack.buffer.len() - overhead;
                 
                 if PACK_BUFFER_SIZE - pack.buffer.len() < overhead {
                     let _ = tx.send(ReadyPack { 
                         buffer: pack.buffer.clone(), 
                         bytes_in_pack: pack.bytes_added, 
                         files_in_pack: pack.files_added 
                     });
                     pack.reset();
                     continue;
                 }

                 let to_read = std::cmp::min(max_data as u64, file_remaining) as usize;
                 if to_read == 0 {
                     let _ = tx.send(ReadyPack { 
                         buffer: pack.buffer.clone(), 
                         bytes_in_pack: pack.bytes_added, 
                         files_in_pack: pack.files_added 
                     });
                     pack.reset();
                     continue;
                 }

                 let mut chunk_buf = vec![0u8; to_read];
                 if file.read_exact(&mut chunk_buf).is_ok() {
                     pack.add_record(&rel_path_str, &chunk_buf);
                     file_remaining -= to_read as u64;
                 } else {
                     break; // Error reading
                 }
            }
            // Finished file
            pack.files_added += 1;
        }

        // Flush remaining
        if pack.record_count() > 0 {
            let _ = tx.send(ReadyPack { 
                buffer: pack.buffer, 
                bytes_in_pack: pack.bytes_added, 
                files_in_pack: pack.files_added 
            });
        }
    });

    // Sender Loop (Main Thread)
    let mut total_sent_bytes = 0u64;
    let mut total_sent_files = 0i32;

    for ready_pack in rx {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Upload cancelled by user"));
        }

        send_frame_header(&mut stream, FrameType::Pack, ready_pack.buffer.len() as u64)?;
        stream.write_all(&ready_pack.buffer)?;
        
        total_sent_bytes += ready_pack.bytes_in_pack;
        total_sent_files += ready_pack.files_in_pack;
        
        progress(total_sent_bytes, total_sent_files);
    }

    send_frame_header(&mut stream, FrameType::Finish, 0)?;

    Ok(())
}

pub fn send_files_v2<F, L>(
    base_path: &str,
    stream: std::net::TcpStream,
    cancel: Arc<AtomicBool>,
    progress: F,
    log: L,
) -> Result<()>
where
    F: FnMut(u64, i32),
    L: Fn(String) + Send + Sync + 'static,
{
    let files = collect_files(base_path);
    send_files_v2_for_list(files, stream, cancel, progress, log)
}
