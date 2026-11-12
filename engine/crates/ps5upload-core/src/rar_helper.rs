//! The engine's end of the `ps5upload-unrar` pipe (desktop only).
//!
//! UnRAR's license is GPL-incompatible, and the engine binary carries third-party
//! GPL/AGPL code with no exception for it, so UnRAR is not linked here: it lives in
//! the separate program `ps5upload-unrar` (engine/crates/ps5upload-unrar), which holds
//! only ps5upload's own code and UnRAR. This module starts that program for each
//! archive operation and speaks its frame protocol (documented at the top of
//! `ps5upload-unrar/src/main.rs`) over stdin/stdout. It mirrors the small part of the
//! `unrar` crate's API that `transfer::rar_support` uses — open for listing or for a
//! forward walk, read a header, skip / decode an entry — so the RAR logic itself
//! (path safety, exclusions, error wording, resume) stays in the engine unchanged.
//!
//! The password is written to the program's stdin only: never argv, the environment
//! or a log line.
//!
//! Locating the program: `PS5UPLOAD_UNRAR_BIN` when set (the desktop app sets it to
//! the copy it extracts next to the engine), else `ps5upload-unrar[.exe]` next to the
//! running executable, else (a test binary) `target/<profile>`.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// The program's file name on this platform.
pub fn helper_file_name() -> String {
    format!("ps5upload-unrar{}", std::env::consts::EXE_SUFFIX)
}

/// The message for a missing helper program.
pub const MISSING: &str = "RAR support is not installed: ps5upload-unrar is missing";

/// Where the helper program is expected (it may not exist).
pub fn helper_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PS5UPLOAD_UNRAR_BIN").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let name = helper_file_name();
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let beside = dir.join(&name);
    if beside.is_file() {
        return beside;
    }
    // A test binary: the helper is built in target/<profile>.
    if let Some(up) = test_profile_dir(&dir)
        .map(|d| d.join(&name))
        .filter(|p| p.is_file())
    {
        return up;
    }
    beside
}

/// target/<profile> for a test binary's own directory: target/<profile>/deps up to Rust
/// 1.99, target/<profile>/build/<package>/<hash>/out since Cargo 1.100's build-dir layout.
fn test_profile_dir(dir: &Path) -> Option<&Path> {
    if dir.file_name()? == "deps" {
        return dir.parent();
    }
    if dir.file_name()? == "out" {
        let profile = dir.ancestors().nth(4)?;
        if dir.ancestors().nth(3)?.file_name()? == "build" {
            return Some(profile);
        }
    }
    None
}

#[cfg(test)]
mod test_profile_dir_tests {
    use super::test_profile_dir;
    use std::path::Path;

    #[test]
    fn finds_the_profile_in_both_cargo_layouts() {
        let old = Path::new("/t/target/debug/deps");
        assert_eq!(test_profile_dir(old), Some(Path::new("/t/target/debug")));
        let new = Path::new("/t/target/debug/build/ps5upload-ava1/25e5c988/out");
        assert_eq!(test_profile_dir(new), Some(Path::new("/t/target/debug")));
        // An installed engine is neither: only the copy beside it counts.
        assert_eq!(
            test_profile_dir(Path::new("/Applications/PS5Upload.app/Contents/MacOS")),
            None
        );
        assert_eq!(test_profile_dir(Path::new("/opt/x/out")), None);
    }
}

/// UnRAR's error codes, as far as the engine tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    MissingPassword,
    BadPassword,
    BadData,
    BadArchive,
    UnknownFormat,
    ERead,
    EReference,
    /// Any other UnRAR code.
    Other,
    /// The helper program is missing, could not start, died, or broke protocol.
    Helper,
}

impl Code {
    fn parse(s: &str) -> Self {
        match s {
            "MissingPassword" => Self::MissingPassword,
            "BadPassword" => Self::BadPassword,
            "BadData" => Self::BadData,
            "BadArchive" => Self::BadArchive,
            "UnknownFormat" => Self::UnknownFormat,
            "ERead" => Self::ERead,
            "EReference" => Self::EReference,
            _ => Self::Other,
        }
    }
}

/// A failure: UnRAR's code and its message (UnRAR's own wording), or a helper problem.
#[derive(Debug, Clone)]
pub struct RarError {
    pub code: Code,
    pub message: String,
}

impl std::fmt::Display for RarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RarError {}

fn helper_err(message: impl Into<String>) -> RarError {
    RarError {
        code: Code::Helper,
        message: message.into(),
    }
}

/// One entry header.
#[derive(Debug, Clone)]
pub struct Header {
    pub filename: PathBuf,
    pub unpacked_size: u64,
    /// UnRAR's packed DOS time (the host's local zone).
    pub file_time: u32,
    dir: bool,
    encrypted: bool,
    split_before: bool,
}

impl Header {
    pub fn is_directory(&self) -> bool {
        self.dir
    }
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }
    /// The rest of a file that began in the previous volume.
    pub fn is_split_before(&self) -> bool {
        self.split_before
    }
}

#[derive(serde::Deserialize)]
struct WireHeader {
    name: String,
    size: u64,
    time: u32,
    dir: bool,
    encrypted: bool,
    split_before: bool,
}

#[derive(serde::Deserialize)]
struct WireFailure {
    code: String,
    message: String,
}

#[derive(serde::Deserialize)]
struct WireOpened {
    solid: bool,
}

/// What a decode hands its callback.
pub enum Piece<'a> {
    /// Decoded bytes of the entry.
    Data(&'a [u8]),
    /// This many bytes were decoded and discarded by the helper.
    Discarded(u64),
}

impl Piece<'_> {
    /// How many bytes of the entry this piece stands for.
    pub fn bytes(&self) -> u64 {
        match self {
            Piece::Data(b) => b.len() as u64,
            Piece::Discarded(n) => *n,
        }
    }
}

/// One running helper. Dropping it stops the program (a cancelled or failed walk stops
/// decoding at once): see [`Process::stop`].
struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Option<BufReader<ChildStdout>>,
    buf: Vec<u8>,
    /// The program was killed or answered its last frame; nothing more will come.
    finished: bool,
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Process {
    /// Ends the program. Both pipes are closed first: the helper then hits a closed pipe
    /// (a write mid-decode, or the next command read) and exits on its own, so it ends
    /// cleanly (its exit work runs, which an instrumented test build needs to write its
    /// coverage data). Only one that has not exited after a moment is killed.
    fn stop(&mut self) {
        self.stdin = None;
        self.out = None;
        // Up to 2 s, checked often: a helper mid-decode ends within a write or two.
        for _ in 0..400 {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn start(
        bin: &Path,
        op: &str,
        archive: &str,
        password: Option<&str>,
    ) -> Result<Self, RarError> {
        let mut cmd = Command::new(bin);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            // No console window for a helper of a windowless engine.
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                helper_err(MISSING)
            } else {
                helper_err(format!(
                    "RAR support could not start ps5upload-unrar ({}): {e}",
                    bin.display()
                ))
            }
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(helper_err("ps5upload-unrar started without pipes"));
        };
        let mut p = Process {
            child,
            stdin: Some(stdin),
            out: Some(BufReader::with_capacity(1 << 20, stdout)),
            buf: Vec::new(),
            finished: false,
        };
        let mut line = serde_json::to_string(&serde_json::json!({
            "op": op,
            "path": archive,
            "password": password,
        }))
        .map_err(|e| helper_err(format!("encode rar request: {e}")))?;
        line.push('\n');
        // A write error means the program already died; reading says why.
        let _ = p.send(line.as_bytes());
        Ok(p)
    }

    fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let w = self
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::other("closed"))?;
        w.write_all(bytes)?;
        w.flush()
    }

    /// The program stopped without a final frame.
    fn died(&mut self) -> RarError {
        self.finished = true;
        self.stdin = None;
        let status = self
            .child
            .wait()
            .map(|s| s.to_string())
            .unwrap_or_else(|e| e.to_string());
        helper_err(format!("ps5upload-unrar stopped unexpectedly ({status})"))
    }

    /// The next frame: its tag, its payload in `self.buf`.
    fn frame(&mut self) -> Result<u8, RarError> {
        if self.finished {
            return Err(helper_err("ps5upload-unrar has already finished"));
        }
        let mut hdr = [0u8; 5];
        let Some(out) = self.out.as_mut() else {
            return Err(self.died());
        };
        if out.read_exact(&mut hdr).is_err() {
            return Err(self.died());
        }
        let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        self.buf.clear();
        self.buf.resize(len, 0);
        let Some(out) = self.out.as_mut() else {
            return Err(self.died());
        };
        if out.read_exact(&mut self.buf).is_err() {
            return Err(self.died());
        }
        if matches!(hdr[0], b'X' | b'Z') {
            self.finished = true;
        }
        Ok(hdr[0])
    }

    fn failure(&self) -> RarError {
        match serde_json::from_slice::<WireFailure>(&self.buf) {
            Ok(f) => RarError {
                code: if f.code == "Request" {
                    Code::Helper
                } else {
                    Code::parse(&f.code)
                },
                message: f.message,
            },
            Err(e) => helper_err(format!("ps5upload-unrar sent a bad error frame: {e}")),
        }
    }

    fn unexpected(&mut self, tag: u8, wanted: &str) -> RarError {
        self.finished = true;
        helper_err(format!(
            "ps5upload-unrar protocol error: expected {wanted}, got frame {:?}",
            tag as char
        ))
    }

    /// Waits for `O` and returns the solid flag.
    fn opened(&mut self) -> Result<bool, RarError> {
        match self.frame()? {
            b'O' => serde_json::from_slice::<WireOpened>(&self.buf)
                .map(|o| o.solid)
                .map_err(|e| helper_err(format!("ps5upload-unrar sent a bad open frame: {e}"))),
            b'X' => Err(self.failure()),
            t => Err(self.unexpected(t, "the archive to open")),
        }
    }

    fn header(&self) -> Result<Header, RarError> {
        let h: WireHeader = serde_json::from_slice(&self.buf)
            .map_err(|e| helper_err(format!("ps5upload-unrar sent a bad header: {e}")))?;
        Ok(Header {
            filename: PathBuf::from(h.name),
            unpacked_size: h.size,
            file_time: h.time,
            dir: h.dir,
            encrypted: h.encrypted,
            split_before: h.split_before,
        })
    }
}

/// Header listing (the `unrar` crate's `open_for_listing`): never decodes.
pub struct Listing {
    p: Process,
    solid: bool,
}

impl Listing {
    pub fn is_solid(&self) -> bool {
        self.solid
    }
}

impl Iterator for Listing {
    type Item = Result<Header, RarError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.p.finished {
            return None;
        }
        Some(match self.p.frame() {
            Ok(b'H') => self.p.header(),
            Ok(b'Z') => return None,
            Ok(b'X') => Err(self.p.failure()),
            Ok(t) => Err(self.p.unexpected(t, "a header")),
            Err(e) => Err(e),
        })
    }
}

/// Opens `archive` for a header listing.
pub fn list(archive: &str, password: Option<&str>) -> Result<Listing, RarError> {
    list_with(&helper_path(), archive, password)
}

pub(crate) fn list_with(
    bin: &Path,
    archive: &str,
    password: Option<&str>,
) -> Result<Listing, RarError> {
    let mut p = Process::start(bin, "list", archive, password)?;
    let solid = p.opened()?;
    Ok(Listing { p, solid })
}

/// A forward walk (the `unrar` crate's `open_for_processing`): after each
/// [`Walk::next_header`] call exactly one of [`Walk::skip`] or [`Walk::read`].
pub struct Walk {
    p: Process,
    solid: bool,
}

/// Opens `archive` for a forward walk.
pub fn walk(archive: &str, password: Option<&str>) -> Result<Walk, RarError> {
    walk_with(&helper_path(), archive, password)
}

pub(crate) fn walk_with(
    bin: &Path,
    archive: &str,
    password: Option<&str>,
) -> Result<Walk, RarError> {
    let mut p = Process::start(bin, "walk", archive, password)?;
    let solid = p.opened()?;
    Ok(Walk { p, solid })
}

impl Walk {
    pub fn is_solid(&self) -> bool {
        self.solid
    }

    /// The next entry's header; `None` at the end of the archive.
    pub fn next_header(&mut self) -> Result<Option<Header>, RarError> {
        match self.p.frame()? {
            b'H' => self.p.header().map(Some),
            b'Z' => Ok(None),
            b'X' => Err(self.p.failure()),
            t => Err(self.p.unexpected(t, "a header")),
        }
    }

    fn command(&mut self, c: u8) -> Result<(), RarError> {
        if self.p.send(&[c]).is_err() {
            // Reading the next frame reports how the program ended.
            return match self.p.frame() {
                Ok(b'X') => Err(self.p.failure()),
                Ok(t) => Err(self.p.unexpected(t, "the end of the entry")),
                Err(e) => Err(e),
            };
        }
        Ok(())
    }

    /// Skips the current entry without decoding it.
    pub fn skip(&mut self) -> Result<(), RarError> {
        self.command(b'S')?;
        match self.p.frame()? {
            b'E' => Ok(()),
            b'X' => Err(self.p.failure()),
            t => Err(self.p.unexpected(t, "the end of a skip")),
        }
    }

    /// Decodes the current entry through `f`, called with consecutive pieces;
    /// `discard` has the helper drop the bytes and report only their count. `f`
    /// returns false to stop: the helper is killed at once and the walk is over (the
    /// result is then an error; the caller knows why it stopped).
    pub fn read(
        &mut self,
        discard: bool,
        f: &mut dyn FnMut(Piece<'_>) -> bool,
    ) -> Result<(), RarError> {
        self.command(if discard { b'C' } else { b'R' })?;
        loop {
            let go = match self.p.frame()? {
                b'D' => f(Piece::Data(&self.p.buf)),
                b'N' => {
                    let n = self
                        .p
                        .buf
                        .get(..8)
                        .and_then(|b| b.try_into().ok())
                        .map_or(0, u64::from_le_bytes);
                    f(Piece::Discarded(n))
                }
                b'E' => return Ok(()),
                b'X' => return Err(self.p.failure()),
                t => return Err(self.p.unexpected(t, "entry data")),
            };
            if !go {
                self.p.finished = true;
                self.p.stop();
                return Err(helper_err("the rar decode was stopped"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_helper_says_rar_support_is_not_installed() {
        let bin = std::env::temp_dir().join("ps5upload-no-such-dir/ps5upload-unrar-missing");
        let e = list_with(&bin, "/x/game.rar", None).err().unwrap();
        assert_eq!(e.code, Code::Helper);
        assert_eq!(e.to_string(), MISSING);
        let e = walk_with(&bin, "/x/game.rar", Some("pw")).err().unwrap();
        assert_eq!(e.to_string(), MISSING);
    }
}
