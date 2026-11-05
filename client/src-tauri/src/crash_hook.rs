//! What the desktop app leaves behind when it dies (#672): a Windows user's app closed right
//! after opening with nothing but c0000409 in the event log, and no word of what had panicked.
//!
//! * Every panic, on any thread, goes to a text file in `~/.ps5upload/crash-reports/` (the bug
//!   report zip collects that folder) and a line in the app log.
//! * A panic on the main thread ends the app, and a failure to start does too: those also tell
//!   the person why, in a message box, and where the report is.
//! * Steps the app can live without run in `optional`: one failing costs that step only.
//!
//! A panic inside a panic hook aborts at once, so the hook takes no lock, unwraps nothing and
//! never uses `eprintln!` (which panics on a closed stderr). It writes files directly instead
//! of going through anything that may be what just panicked.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

/// The kept reports on disk are bounded (`crash_reports::prune`); this bounds one process in a
/// loop, a reader that panics on every scan for example. A main thread panic is always written.
const MAX_REPORTS_PER_RUN: usize = 8;
/// A deep stack's text is long and only the top matters.
const MAX_BACKTRACE_CHARS: usize = 32 * 1024;

static BASE_DIR: OnceLock<PathBuf> = OnceLock::new();
static MAIN_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();
static WRITTEN: AtomicUsize = AtomicUsize::new(0);
/// One message box per run: the first failure is the one that explains the rest.
static TOLD: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Depth of `optional` on this thread: a panic here is caught and the app goes on.
    static GUARDED: Cell<u32> = const { Cell::new(0) };
    /// A panic while reporting a panic: say nothing more.
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// `<home>/.ps5upload`, or the temp folder when this machine has no usable home.
fn base_dir() -> PathBuf {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    };
    match home.filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h).join(".ps5upload"),
        None => std::env::temp_dir().join("ps5upload"),
    }
}

/// Starts the reporting. First thing in `run()`.
pub fn install() {
    let _ = MAIN_THREAD.set(std::thread::current().id());
    let base = base_dir();
    let _ = BASE_DIR.set(base.clone());
    crate::commands::crash_reports::prune(&base.join("crash-reports"));
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !enter_hook() {
            return;
        }
        on_panic(info);
        // The default hook writes to stderr and ignores a failed write.
        default_hook(info);
        IN_HOOK.try_with(|c| c.set(false)).ok();
    }));
    run_requested_test();
}

/// For checking this on a real machine, as nothing else makes the app fail on demand.
/// `PS5UPLOAD_CRASH_TEST=thread` panics a background thread (the app stays up), `main` panics
/// the main thread.
fn run_requested_test() {
    match std::env::var("PS5UPLOAD_CRASH_TEST").as_deref() {
        Ok("thread") => {
            let _ = std::thread::Builder::new()
                .name("crash-test".into())
                .spawn(|| panic!("PS5UPLOAD_CRASH_TEST on a background thread"));
        }
        Ok("main") => panic!("PS5UPLOAD_CRASH_TEST on the main thread"),
        _ => {}
    }
}

fn enter_hook() -> bool {
    IN_HOOK.try_with(|c| !c.replace(true)).unwrap_or(false)
}

fn on_panic(info: &std::panic::PanicHookInfo<'_>) {
    let current = std::thread::current();
    let thread = current.name().unwrap_or("unnamed");
    let main = MAIN_THREAD.get() == Some(&current.id());
    let guarded = GUARDED.try_with(|c| c.get() > 0).unwrap_or(false);
    let ends_app = main && !guarded;
    let message = panic_message(info.payload());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown".to_string());

    let seen = WRITTEN.fetch_add(1, Ordering::Relaxed);
    let mut saved = None;
    if ends_app || seen < MAX_REPORTS_PER_RUN {
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let text = panic_report(thread, &location, &message, ends_app, &backtrace);
        saved = write_report(&text);
        log_line(&format!(
            "panic on thread {thread} at {location}: {message}"
        ));
    }
    if ends_app {
        let what = format!("{message}\n(at {location})");
        tell_person(&fatal_message(Stage::Panic, &what, saved.as_deref()));
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic without a text message".to_string()
    }
}

/// The report's text. Thread, place and message are the point; the rest helps read them.
fn panic_report(
    thread: &str,
    location: &str,
    message: &str,
    ends_app: bool,
    backtrace: &str,
) -> String {
    let bt: String = backtrace.chars().take(MAX_BACKTRACE_CHARS).collect();
    format!(
        "PS5Upload {version} panic report\n\
         when (ms since 1970): {when}\n\
         os: {os} {arch}\n\
         thread: {thread}\n\
         location: {location}\n\
         message: {message}\n\
         ends the app: {ends}\n\
         exe base: {base}\n\
         \n\
         backtrace:\n{bt}\n",
        version = env!("CARGO_PKG_VERSION"),
        when = crate::commands::diag_log::now_ms(),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        ends = if ends_app { "yes" } else { "no" },
        base = exe_base(),
    )
}

/// Where the program is loaded. Addresses in a stripped backtrace mean nothing without it.
fn exe_base() -> String {
    #[cfg(windows)]
    {
        extern "system" {
            fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
        }
        // SAFETY: a null name asks for this program's own handle; nothing is written.
        format!("{:p}", unsafe { GetModuleHandleW(std::ptr::null()) })
    }
    #[cfg(not(windows))]
    "n/a".to_string()
}

/// Writes a report; its path, or None if it could not be written.
fn write_report(text: &str) -> Option<PathBuf> {
    let base = BASE_DIR.get()?;
    let dir = base.join("crash-reports");
    std::fs::create_dir_all(&dir).ok()?;
    write_report_in(&dir, text)
}

fn write_report_in(dir: &Path, text: &str) -> Option<PathBuf> {
    let n = WRITTEN.load(Ordering::Relaxed);
    let name = format!(
        "ps5upload-report-{}-{}-{n}.txt",
        crate::commands::diag_log::now_ms(),
        std::process::id()
    );
    let path = dir.join(name);
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// One line in the app log, in the renderer's format. Appended directly: this takes no lock
/// anything else may hold, and a line this short lands whole beside the renderer's.
fn log_line(message: &str) {
    use std::io::Write;
    let Some(base) = BASE_DIR.get() else { return };
    let dir = base.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = crate::commands::diag_log::now_ms();
    let line = serde_json::json!({
        "ts": now,
        "level": "error",
        "source": "app",
        "message": message.replace('\n', " "),
    });
    let path = dir.join(crate::commands::diag_log::file_name_for(now));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Runs a step the app can live without. A panic in it is reported and costs only that step.
/// Also what tells the hook that a main thread panic here does not end the app.
pub fn optional<R>(what: &str, step: impl FnOnce() -> R) -> Option<R> {
    let _ = GUARDED.try_with(|c| c.set(c.get() + 1));
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(step));
    let _ = GUARDED.try_with(|c| c.set(c.get().saturating_sub(1)));
    match out {
        Ok(v) => Some(v),
        Err(_) => {
            let _ = stderr_line(&format!("[crash] {what} failed; carrying on without it"));
            None
        }
    }
}

fn stderr_line(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    writeln!(std::io::stderr(), "{text}")
}

/// Which step of starting failed, for the wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Starting the window system and the app's parts (`Builder::build`).
    Start,
    /// Creating the main window and its webview.
    Window,
    /// A panic on the main thread.
    Panic,
}

/// A failure to start: report it, tell the person, and end. Does not return.
pub fn fatal(stage: Stage, error: &str) -> ! {
    let text = format!(
        "PS5Upload {} failed to start\nstage: {stage:?}\nos: {} {}\nerror: {error}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let saved = write_report(&text);
    log_line(&format!("failed to start ({stage:?}): {error}"));
    tell_person(&fatal_message(stage, error, saved.as_deref()));
    std::process::exit(1)
}

/// What the person reads. Plain words: no language is loaded yet at this point.
fn fatal_message(stage: Stage, error: &str, saved: Option<&Path>) -> String {
    // Only asked when it is to be shown: this may be running in the panic hook.
    let webview = if webview_failed(stage, error) {
        webview_version()
    } else {
        None
    };
    fatal_message_for(stage, error, saved, cfg!(windows), webview.as_deref())
}

fn webview_failed(stage: Stage, error: &str) -> bool {
    stage == Stage::Window || error.to_lowercase().contains("webview")
}

fn fatal_message_for(
    stage: Stage,
    error: &str,
    saved: Option<&Path>,
    windows: bool,
    webview: Option<&str>,
) -> String {
    let mut out = String::new();
    if windows && webview_failed(stage, error) {
        out.push_str(
            "PS5Upload could not open its window, because the Microsoft Edge WebView2 Runtime \
             failed to start.\n\n\
             To fix it, open Windows Settings > Apps > Installed apps, find \"Microsoft Edge \
             WebView2 Runtime\" and choose Modify, then Repair. If it is not listed or that does \
             not help, install it again from \
             https://developer.microsoft.com/microsoft-edge/webview2/\n\n",
        );
        match webview {
            Some(v) => out.push_str(&format!("WebView2 version found: {v}\n")),
            None => out.push_str("WebView2 was not found on this PC.\n"),
        }
    } else {
        match stage {
            Stage::Start => out.push_str("PS5Upload could not start.\n\n"),
            Stage::Window => out.push_str("PS5Upload could not open its window.\n\n"),
            Stage::Panic => out.push_str("PS5Upload hit an internal error and has to close.\n\n"),
        }
    }
    out.push_str(&format!("The error was: {error}\n\n"));
    match saved {
        Some(p) => out.push_str(&format!("A report was saved here:\n{}\n\n", p.display())),
        None => out.push_str("The report could not be saved.\n\n"),
    }
    out.push_str(
        "Please send this text or that file with a bug report: \
         https://github.com/phantomptr/ps5upload/issues",
    );
    out
}

/// Asks the WebView2 loader; an error where there is none.
fn webview_version() -> Option<String> {
    #[cfg(windows)]
    {
        tauri::webview_version().ok()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Stderr always; a window as well, the first time. Blocks until the person has read it, as
/// the process ends right after.
fn tell_person(message: &str) {
    let _ = stderr_line(message);
    if TOLD.swap(true, Ordering::SeqCst) {
        return;
    }
    show_dialog(message);
}

/// From a thread of its own: this thread may be inside a window procedure, and a message box
/// here would pump messages for the windows of a thread that just panicked.
#[cfg(windows)]
fn show_dialog(message: &str) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    const MB_ICONERROR: u32 = 0x10;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;
    const MB_TOPMOST: u32 = 0x0004_0000;
    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .filter(|c| *c != 0)
            .chain(std::iter::once(0))
            .collect()
    }
    let (text, caption) = (wide(message), wide("PS5Upload"));
    let shown = std::thread::Builder::new()
        .name("crash-dialog".into())
        .spawn(move || {
            // SAFETY: both strings are NUL terminated and outlive the call; no owner window.
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    text.as_ptr(),
                    caption.as_ptr(),
                    MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
                );
            }
        });
    if let Ok(thread) = shown {
        wait_for_thread(thread, DIALOG_LIMIT);
    }
}

/// No one may be there to dismiss a dialog (a scheduled task, a service session): a process that has
/// failed must not stay alive for it. The report is written by now.
#[cfg(not(target_os = "macos"))]
const DIALOG_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Until the thread ends, or `limit`.
#[cfg(any(test, windows))]
fn wait_for_thread(thread: std::thread::JoinHandle<()>, limit: std::time::Duration) {
    let end = std::time::Instant::now() + limit;
    while !thread.is_finished() && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Until the program ends, or `limit`, and then it is closed. Whether it could start at all.
#[cfg(any(all(test, unix), not(any(windows, target_os = "macos"))))]
fn wait_for_program(
    command: &mut std::process::Command,
    limit: std::time::Duration,
) -> std::io::Result<()> {
    let mut child = command.spawn()?;
    let end = std::time::Instant::now() + limit;
    while std::time::Instant::now() < end {
        if !matches!(child.try_wait(), Ok(None)) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// An alert from outside the process: AppKit may be what failed, and may not be usable here.
#[cfg(target_os = "macos")]
fn show_dialog(message: &str) {
    const SCRIPT: &str = "on run argv\n\
         display alert \"PS5Upload\" message (item 1 of argv) as critical giving up after 600\n\
         end run";
    let _ = std::process::Command::new("osascript")
        .args(["-e", SCRIPT, message])
        .status();
}

/// Whichever of the desktops' dialog tools is there; without one, stderr and the file remain.
#[cfg(not(any(windows, target_os = "macos")))]
fn show_dialog(message: &str) {
    let zenity = wait_for_program(
        std::process::Command::new("zenity")
            .args(["--error", "--no-markup", "--title=PS5Upload", "--text"])
            .arg(message),
        DIALOG_LIMIT,
    );
    if zenity.is_err() {
        let _ = wait_for_program(
            std::process::Command::new("kdialog").args([
                "--title",
                "PS5Upload",
                "--error",
                message,
            ]),
            DIALOG_LIMIT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_nobody_answers_is_not_waited_for_forever() {
        use std::time::{Duration, Instant};
        #[cfg(unix)]
        {
            let t0 = Instant::now();
            let mut sleep = std::process::Command::new("sleep");
            sleep.arg("30");
            wait_for_program(&mut sleep, Duration::from_millis(200)).unwrap();
            assert!(t0.elapsed() < Duration::from_secs(10));
            assert!(wait_for_program(
                &mut std::process::Command::new("/no/such/dialog/tool"),
                Duration::from_secs(1)
            )
            .is_err());
        }
        let t1 = Instant::now();
        let stuck = std::thread::spawn(|| std::thread::sleep(Duration::from_secs(5)));
        wait_for_thread(stuck, Duration::from_millis(100));
        assert!(t1.elapsed() < Duration::from_secs(3));
    }

    fn saved() -> PathBuf {
        PathBuf::from(r"C:\Users\a\.ps5upload\crash-reports\r.txt")
    }

    #[test]
    fn a_webview_failure_on_windows_says_to_repair_webview2_and_shows_the_error() {
        let m = fatal_message_for(
            Stage::Window,
            "failed to create webview: 0x80070005",
            Some(&saved()),
            true,
            Some("131.0.2903.86"),
        );
        assert!(m.contains("WebView2 Runtime"), "{m}");
        assert!(m.contains("Repair"), "{m}");
        assert!(m.contains("0x80070005"), "{m}");
        assert!(m.contains("131.0.2903.86"), "{m}");
        assert!(m.contains(r"crash-reports\r.txt"), "{m}");
    }

    #[test]
    fn no_webview2_on_the_pc_is_said_so() {
        let m = fatal_message_for(Stage::Window, "webview", None, true, None);
        assert!(m.contains("not found on this PC"), "{m}");
        assert!(m.contains("could not be saved"), "{m}");
    }

    #[test]
    fn elsewhere_the_message_does_not_mention_webview2() {
        let m = fatal_message_for(Stage::Window, "no display", Some(&saved()), false, None);
        assert!(m.starts_with("PS5Upload could not open its window."), "{m}");
        assert!(!m.contains("WebView2"), "{m}");
        assert!(m.contains("no display"), "{m}");
    }

    #[test]
    fn a_panic_that_is_no_webview_failure_is_called_an_internal_error() {
        let m = fatal_message_for(Stage::Panic, "index out of range", None, true, Some("1"));
        assert!(m.starts_with("PS5Upload hit an internal error"), "{m}");
        assert!(!m.contains("WebView2"), "{m}");
    }

    #[test]
    fn the_report_holds_thread_place_and_message_and_stays_bounded() {
        let long = "x".repeat(MAX_BACKTRACE_CHARS * 2);
        let r = panic_report("hid-host", "a/b.rs:1:2", "Invalid UTF-16", false, &long);
        assert!(r.contains("thread: hid-host"), "{r}");
        assert!(r.contains("location: a/b.rs:1:2"));
        assert!(r.contains("message: Invalid UTF-16"));
        assert!(r.contains("ends the app: no"));
        assert!(r.len() < MAX_BACKTRACE_CHARS + 1024);
    }

    #[test]
    fn a_report_is_a_txt_the_report_list_finds() {
        let dir = std::env::temp_dir().join(format!("ps5upload-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_report_in(&dir, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        assert!(crate::commands::crash_reports::list_report_files(&dir).contains(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panic_payloads_of_both_kinds_read_as_text() {
        let a = std::panic::catch_unwind(|| panic!("plain")).unwrap_err();
        assert_eq!(panic_message(a.as_ref()), "plain");
        let n = 3;
        let b = std::panic::catch_unwind(|| panic!("number {n}")).unwrap_err();
        assert_eq!(panic_message(b.as_ref()), "number 3");
    }

    #[test]
    fn an_optional_step_that_panics_costs_only_that_step() {
        assert_eq!(optional("fine", || 7), Some(7));
        let out: Option<()> = optional("bad", || panic!("boom"));
        assert_eq!(out, None);
        // The guard is released: this thread is back to unguarded.
        assert_eq!(GUARDED.with(|c| c.get()), 0);
    }
}
