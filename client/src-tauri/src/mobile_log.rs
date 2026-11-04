//! Phone apps' log file and logcat.
//!
//! A phone app has no terminal: on Android stderr goes nowhere, so every `eprintln!` from this
//! crate and every line the in-process engine mirrors to stderr was lost, and nothing wrote the
//! `engine/engine.log` the bug report promises. Phone reports are the only evidence from phones,
//! and the background and crash cases are the ones the page's own log cannot show.
//!
//! So stderr is pointed at a pipe, once, at start-up. One thread reads it and writes each line to
//! `<app_local_data_dir>/engine/engine.log` (rotated like the desktop's), to logcat on Android and
//! back to the original stderr (Xcode's console on iOS). That catches the whole process with no
//! change at any call site. A panic hook adds the message and a backtrace, written straight to
//! the file, because the app may be about to end.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

/// Same cap as the desktop's engine.log (engine.rs).
const MAX_BYTES: u64 = 1024 * 1024;

/// The log file with the size it has reached; passing the cap moves it to `.old`.
struct LogFile {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> Self {
        let mut log = LogFile {
            path,
            file: None,
            size: 0,
        };
        log.reopen();
        log
    }

    fn reopen(&mut self) {
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        self.size = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
    }

    fn line(&mut self, line: &[u8]) {
        let Some(f) = self.file.as_mut() else {
            return;
        };
        if f.write_all(line).is_ok() && f.write_all(b"\n").is_ok() {
            self.size += line.len() as u64 + 1;
        }
        if self.size > MAX_BYTES {
            let old = self.path.with_extension("log.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&self.path, &old);
            self.reopen();
        }
    }
}

/// Cut `buf` into complete lines, keeping what follows the last newline for the next read.
fn take_lines(buf: &mut Vec<u8>, mut sink: impl FnMut(&[u8])) {
    let mut start = 0;
    while let Some(i) = buf[start..].iter().position(|b| *b == b'\n') {
        let end = start + i;
        let line = buf[start..end]
            .strip_suffix(b"\r")
            .unwrap_or(&buf[start..end]);
        sink(line);
        start = end + 1;
    }
    buf.drain(..start);
}

#[cfg(mobile)]
pub use on_phone::start;

#[cfg(mobile)]
mod on_phone {
    use super::*;
    use std::os::fd::{FromRawFd, RawFd};
    use std::path::Path;
    use std::sync::OnceLock;
    use tauri::{AppHandle, Manager};

    static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

    #[cfg(target_os = "android")]
    fn logcat(line: &[u8]) {
        #[link(name = "log")]
        extern "C" {
            fn __android_log_write(
                prio: libc::c_int,
                tag: *const libc::c_char,
                text: *const libc::c_char,
            ) -> libc::c_int;
        }
        const ANDROID_LOG_INFO: libc::c_int = 4;
        // A NUL inside a line ends it early; logcat is only a convenience copy.
        let end = line.iter().position(|b| *b == 0).unwrap_or(line.len());
        let Ok(text) = std::ffi::CString::new(&line[..end]) else {
            return;
        };
        unsafe { __android_log_write(ANDROID_LOG_INFO, c"PS5Upload".as_ptr(), text.as_ptr()) };
    }

    #[cfg(not(target_os = "android"))]
    fn logcat(_line: &[u8]) {}

    /// Never ends and never panics: a writer to a pipe nobody reads would get EPIPE, and
    /// `eprintln!` panics on that.
    fn pump(read: RawFd, original: Option<File>, mut log: LogFile) {
        let mut read = unsafe { File::from_raw_fd(read) };
        let mut original = original;
        let mut pending = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = match std::io::Read::read(&mut read, &mut chunk) {
                Ok(0) => {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
            };
            if let Some(o) = original.as_mut() {
                let _ = o.write_all(&chunk[..n]);
            }
            pending.extend_from_slice(&chunk[..n]);
            take_lines(&mut pending, |line| {
                log.line(line);
                logcat(line);
            });
            // A line this long is not a line: keep the memory bounded.
            if pending.len() > 256 * 1024 {
                log.line(&pending);
                logcat(&pending);
                pending.clear();
            }
        }
    }

    fn redirect_stderr(log: LogFile) -> std::io::Result<()> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let [read, write] = fds;
        let saved = unsafe { libc::dup(libc::STDERR_FILENO) };
        let original = (saved >= 0).then(|| unsafe { File::from_raw_fd(saved) });
        std::thread::Builder::new()
            .name("mobile-log".into())
            .spawn(move || pump(read, original, log))?;
        if unsafe { libc::dup2(write, libc::STDERR_FILENO) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        unsafe { libc::close(write) };
        Ok(())
    }

    fn install_panic_hook() {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let backtrace = std::backtrace::Backtrace::force_capture();
            let text = format!("PANIC {info}\n{backtrace}");
            // Straight to the file: the pipe's thread may not get another turn.
            if let Some(path) = LOG_PATH.get() {
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(f, "{text}");
                }
            }
            // The in-app log and the bug report's copy of it.
            ps5upload_engine::record_host_panic(text);
            default(info);
        }));
    }

    /// Once, before the engine starts. A second call (Android can run setup again when its
    /// Activity is recreated) does nothing.
    pub fn start(app: &AppHandle) {
        static STARTED: OnceLock<()> = OnceLock::new();
        if STARTED.set(()).is_err() {
            return;
        }
        let Ok(dir) = app.path().app_local_data_dir().map(|d| d.join("engine")) else {
            return;
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join("engine.log");
        let _ = LOG_PATH.set(path.clone());
        install_panic_hook();
        if let Err(e) = redirect_stderr(LogFile::open(path)) {
            // stderr may be where it was, or gone: there is nowhere better to say it.
            eprintln!("[log] could not capture stderr: {e}");
        }
    }

    /// For the engine diagnosis.
    pub fn log_path() -> Option<&'static Path> {
        LOG_PATH.get().map(PathBuf::as_path)
    }
}

#[cfg(mobile)]
pub use on_phone::log_path;

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(buf: &mut Vec<u8>) -> Vec<String> {
        let mut out = Vec::new();
        take_lines(buf, |l| out.push(String::from_utf8_lossy(l).into_owned()));
        out
    }

    #[test]
    fn takes_whole_lines_and_keeps_the_rest() {
        let mut buf = b"one\ntwo\r\nthr".to_vec();
        assert_eq!(lines(&mut buf), ["one", "two"]);
        assert_eq!(buf, b"thr");
        buf.extend_from_slice(b"ee\n");
        assert_eq!(lines(&mut buf), ["three"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn rotates_past_the_cap_and_keeps_writing() {
        let dir = std::env::temp_dir().join(format!("ps5u_mobile_log_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("engine.log");
        let mut log = LogFile::open(path.clone());
        let line = vec![b'x'; (MAX_BYTES as usize) + 1];
        log.line(&line);
        log.line(b"after");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
        assert_eq!(
            std::fs::metadata(path.with_extension("log.old"))
                .unwrap()
                .len(),
            MAX_BYTES + 2
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_to_what_an_earlier_run_left() {
        let dir = std::env::temp_dir().join(format!("ps5u_mobile_log2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("engine.log");
        std::fs::write(&path, "earlier\n").unwrap();
        let mut log = LogFile::open(path.clone());
        log.line(b"now");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "earlier\nnow\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
