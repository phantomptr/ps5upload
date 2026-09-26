//! Bridge between UnRAR's push-style callback and the pull-style shard sender.
//!
//! UnRAR hands us decompressed bytes through a callback; `PipelinedSender`
//! wants something it can `read()`. A worker thread runs the UnRAR walk and
//! pushes entry-delimited messages down a bounded channel; this module turns
//! those back into one `Read` per entry.
//!
//! The framing matters: without `EntryEnd` the next file's bytes would flow
//! into the current file's shards and silently corrupt the upload. That is
//! why this lives in its own module with its own tests — the failure mode is
//! invisible at the archive level.

use std::io::{Error, ErrorKind, Read};
use std::sync::mpsc::Receiver;

/// One message from the decompression worker.
#[derive(Debug)]
pub enum StreamMsg {
    /// A new entry begins; carries the sanitised relative path.
    Entry(String),
    /// Decompressed bytes for the entry in progress.
    Chunk(Box<[u8]>),
    /// The entry in progress is complete.
    EntryEnd,
    /// Every entry has been walked.
    Finished,
    /// The worker gave up; the string is the reason.
    Failed(String),
}

/// Await the next entry. `Ok(None)` means the archive is done.
pub fn next_entry(rx: &Receiver<StreamMsg>) -> std::io::Result<Option<String>> {
    match rx.recv() {
        Ok(StreamMsg::Entry(name)) => Ok(Some(name)),
        Ok(StreamMsg::Finished) => Ok(None),
        Ok(StreamMsg::Failed(msg)) => Err(Error::other(msg)),
        // Bytes with no entry open means the worker's framing is broken.
        Ok(other) => Err(Error::other(format!(
            "rar stream out of order: expected an entry, got {other:?}"
        ))),
        Err(_) => Err(Error::new(
            ErrorKind::UnexpectedEof,
            "rar decompression worker stopped unexpectedly",
        )),
    }
}

/// A `Read` over exactly one entry's bytes. EOF at `EntryEnd`.
pub struct EntryReader<'a> {
    rx: &'a Receiver<StreamMsg>,
    leftover: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<'a> EntryReader<'a> {
    pub fn new(rx: &'a Receiver<StreamMsg>) -> Self {
        Self {
            rx,
            leftover: Vec::new(),
            pos: 0,
            done: false,
        }
    }
}

impl Read for EntryReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.pos >= self.leftover.len() {
            if self.done {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(StreamMsg::Chunk(c)) => {
                    self.leftover = c.into_vec();
                    self.pos = 0;
                }
                Ok(StreamMsg::EntryEnd) => {
                    self.done = true;
                    return Ok(0);
                }
                Ok(StreamMsg::Failed(msg)) => return Err(Error::other(msg)),
                Ok(other) => {
                    return Err(Error::other(format!(
                        "rar stream out of order mid-entry: {other:?}"
                    )))
                }
                Err(_) => {
                    return Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "rar decompression worker stopped mid-entry",
                    ))
                }
            }
        }
        let n = std::cmp::min(out.len(), self.leftover.len() - self.pos);
        out[..n].copy_from_slice(&self.leftover[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc::sync_channel;

    fn feed(msgs: Vec<StreamMsg>) -> std::sync::mpsc::Receiver<StreamMsg> {
        let (tx, rx) = sync_channel(64);
        for m in msgs {
            tx.send(m).unwrap();
        }
        drop(tx);
        rx
    }

    #[test]
    fn reassembles_chunks_into_one_stream() {
        let rx = feed(vec![
            StreamMsg::Entry("a.bin".into()),
            StreamMsg::Chunk(b"hello ".to_vec().into_boxed_slice()),
            StreamMsg::Chunk(b"world".to_vec().into_boxed_slice()),
            StreamMsg::EntryEnd,
        ]);
        assert_eq!(next_entry(&rx).unwrap().as_deref(), Some("a.bin"));
        let mut out = Vec::new();
        EntryReader::new(&rx).read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn entry_end_is_eof_not_the_next_entry() {
        // The reader must stop at EntryEnd, or one file's bytes bleed into
        // the next file's shards.
        let rx = feed(vec![
            StreamMsg::Entry("a".into()),
            StreamMsg::Chunk(b"AAA".to_vec().into_boxed_slice()),
            StreamMsg::EntryEnd,
            StreamMsg::Entry("b".into()),
            StreamMsg::Chunk(b"BBB".to_vec().into_boxed_slice()),
            StreamMsg::EntryEnd,
        ]);
        assert_eq!(next_entry(&rx).unwrap().as_deref(), Some("a"));
        let mut a = Vec::new();
        EntryReader::new(&rx).read_to_end(&mut a).unwrap();
        assert_eq!(a, b"AAA");
        assert_eq!(next_entry(&rx).unwrap().as_deref(), Some("b"));
        let mut b = Vec::new();
        EntryReader::new(&rx).read_to_end(&mut b).unwrap();
        assert_eq!(b, b"BBB");
    }

    #[test]
    fn a_short_read_buffer_keeps_the_leftover() {
        let rx = feed(vec![
            StreamMsg::Entry("a".into()),
            StreamMsg::Chunk(b"abcdef".to_vec().into_boxed_slice()),
            StreamMsg::EntryEnd,
        ]);
        next_entry(&rx).unwrap();
        let mut r = EntryReader::new(&rx);
        let mut two = [0u8; 2];
        r.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"ab");
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"cdef");
    }

    #[test]
    fn zero_byte_entry_reads_as_empty() {
        let rx = feed(vec![StreamMsg::Entry("z".into()), StreamMsg::EntryEnd]);
        next_entry(&rx).unwrap();
        let mut out = Vec::new();
        EntryReader::new(&rx).read_to_end(&mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn finished_reports_no_more_entries() {
        let rx = feed(vec![StreamMsg::Finished]);
        assert!(next_entry(&rx).unwrap().is_none());
    }

    #[test]
    fn worker_failure_surfaces_as_an_error_not_eof() {
        // Silently treating a decode failure as end-of-file would commit a
        // truncated game to the console.
        let rx = feed(vec![
            StreamMsg::Entry("a".into()),
            StreamMsg::Failed("corrupt block".into()),
        ]);
        next_entry(&rx).unwrap();
        let mut out = Vec::new();
        let err = EntryReader::new(&rx).read_to_end(&mut out).unwrap_err();
        assert!(err.to_string().contains("corrupt block"), "got {err}");
    }

    #[test]
    fn a_dead_worker_is_an_error_not_eof() {
        let (tx, rx) = sync_channel::<StreamMsg>(1);
        tx.send(StreamMsg::Entry("a".into())).unwrap();
        drop(tx); // worker vanished mid-entry
        next_entry(&rx).unwrap();
        let mut out = Vec::new();
        assert!(EntryReader::new(&rx).read_to_end(&mut out).is_err());
    }

    #[test]
    fn failed_before_any_entry_surfaces_from_next_entry() {
        let rx = feed(vec![StreamMsg::Failed("bad password".into())]);
        let err = next_entry(&rx).unwrap_err();
        assert!(err.to_string().contains("bad password"), "got {err}");
    }
}
