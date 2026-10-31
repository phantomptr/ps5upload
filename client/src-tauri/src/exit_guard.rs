//! Quitting must never hang. The app's exit handler stops the engine and the keep-awake helper
//! with async code; when that async work cannot finish (a macOS hang report from 8.1.1 showed
//! the main thread parked in the handler for minutes after "Install and restart", the window
//! frozen, the update waiting for it), the app sat there "not responding" until force-quit.
//!
//! So the async clean-up gets a wall-clock limit that needs no async runtime, and every child
//! process the app starts is registered here by PID: when the limit passes, they are killed
//! directly (SIGKILL / TerminateProcess) and the app exits anyway.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// The app is quitting: an engine that exits now is being shut down, not crashing, and must not
/// be started again (engine.rs's exit watcher reads this).
static EXITING: AtomicBool = AtomicBool::new(false);

/// Mark the app as quitting (the exit handler, before it stops anything).
pub fn set_exiting() {
    EXITING.store(true, Ordering::SeqCst);
}

pub fn exiting() -> bool {
    EXITING.load(Ordering::SeqCst)
}

/// How long the exit handler waits for the orderly clean-up (the engine's own stop is time-boxed
/// at 5 s; this leaves room for it, and no more).
pub const CLEANUP_LIMIT: Duration = Duration::from_secs(7);

static CHILDREN: Mutex<Vec<(&'static str, u32)>> = Mutex::new(Vec::new());

/// A child process the app started and must not leave behind.
pub fn register(label: &'static str, pid: u32) {
    let mut c = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
    if !c.iter().any(|(_, p)| *p == pid) {
        c.push((label, pid));
    }
}

/// The child is gone (stopped or reaped): never kill its PID later, it may be reused.
pub fn unregister(pid: u32) {
    CHILDREN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(_, p)| *p != pid);
}

/// Kills every registered child at once. Last resort, from the exit path only.
pub fn kill_all() {
    let children = std::mem::take(&mut *CHILDREN.lock().unwrap_or_else(|e| e.into_inner()));
    for (label, pid) in children {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("[exit] clean-up overran: killing {label} (pid {pid})\n"),
        );
        kill(pid);
    }
}

#[cfg(unix)]
fn kill(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: plain syscall on a PID this process started and has not reaped.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
fn kill(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
fn kill(_pid: u32) {}

/// Runs the orderly clean-up `f` on the async runtime and waits for it at most `limit`, on the
/// calling (main) thread, with no async machinery of its own. Past the limit, or when the
/// runtime cannot run it at all, the registered children are killed and this returns.
pub fn run_bounded<F>(limit: Duration, f: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        f.await;
        let _ = tx.send(());
    });
    if rx.recv_timeout(limit).is_err() {
        kill_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the registry: one at a time.
    static ONE: Mutex<()> = Mutex::new(());

    #[test]
    fn a_clean_up_that_never_finishes_does_not_hold_the_exit() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let t = std::time::Instant::now();
        run_bounded(Duration::from_millis(300), std::future::pending());
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn an_overrun_kills_what_was_registered_and_forgets_it() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(unix)]
        {
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            register("test", child.id());
            run_bounded(Duration::from_millis(200), std::future::pending());
            let status = child.wait().unwrap();
            assert!(!status.success());
            assert!(CHILDREN
                .lock()
                .unwrap()
                .iter()
                .all(|(_, p)| *p != child.id()));
        }
    }

    #[test]
    fn a_finished_clean_up_kills_nothing() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(unix)]
        {
            let mut child = std::process::Command::new("sleep")
                .arg("2")
                .spawn()
                .unwrap();
            register("test-keep", child.id());
            run_bounded(Duration::from_secs(5), async {});
            assert!(child.try_wait().unwrap().is_none());
            unregister(child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
