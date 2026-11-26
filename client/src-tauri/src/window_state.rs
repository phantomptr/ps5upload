//! The main window opens where it was left: size, position and maximized, kept in
//! `~/.ps5upload/window-state.json` (its own file: the renderer rewrites settings.json whole).
//! Desktop only, and only the main window (picture in picture keeps its own size).
//!
//! Units. The size is kept in logical pixels, so a window looks the same size after a change of
//! display scaling. The position is kept in the desktop's own coordinates, the ones the monitors
//! are reported in on that system: points on macOS (where each monitor's "physical" position is
//! scaled by its own factor, so physical values from two monitors don't line up), physical pixels
//! on Windows and Linux (one virtual desktop in pixels). Wayland has no window positions: the
//! compositor places the window and only the size comes back.
//!
//! Off screen. A saved window that touches no connected monitor (one unplugged, a new
//! resolution, a different machine) opens centred on the primary monitor instead. One that
//! does is pulled fully onto the monitor it overlaps most, shrunk to that monitor's work area
//! (never below the window's minimum size).
//!
//! The window is created hidden (pip_window::build_main), placed, then shown, so it never
//! jumps. Changes are saved half a second after the last move or resize, and once more at exit.
//! Not while minimized or full screen; while maximized only the flag changes, so un-maximizing
//! after the next launch returns to the size it had before.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, Window, WindowEvent};

const FILE_NAME: &str = "window-state.json";
const MAIN: &str = "main";
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// Logical pixels the frame adds above the content (title bar, borders), allowed for when the
/// window has to fit a monitor.
const FRAME_HEIGHT: f64 = 40.0;
/// Logical pixels the frame may sit past a monitor's edge: Windows counts its invisible resize
/// borders (about 7 px) in the window's outer position.
const EDGE_SLACK: f64 = 8.0;
/// Far beyond any real desktop: a value past this is not a window that was ever on screen.
const MAX_COORD: f64 = 1.0e6;

/// What is saved.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// Outer top-left, in the desktop's own coordinates (see the module doc).
    pub x: f64,
    pub y: f64,
    /// Inner (content) size, logical pixels. The size before maximizing when `maximized`.
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub maximized: bool,
}

/// A connected monitor's work area (without taskbar / menu bar / Dock), in the desktop's own
/// coordinates, and how many of those units one logical pixel is on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// Where the window goes: position in desktop units, inner size in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

/// The saved file's contents, or None for anything unusable (not JSON, missing fields, sizes
/// that are not positive, numbers that are not finite or absurdly far off).
pub fn parse(bytes: &[u8]) -> Option<Saved> {
    let s: Saved = serde_json::from_slice(bytes).ok()?;
    let finite = [s.x, s.y, s.width, s.height].iter().all(|v| v.is_finite());
    let sane = s.x.abs() < MAX_COORD
        && s.y.abs() < MAX_COORD
        && s.width >= 1.0
        && s.height >= 1.0
        && s.width < MAX_COORD
        && s.height < MAX_COORD;
    (finite && sane).then_some(s)
}

/// Where a saved window opens, given the connected monitors (`primary` indexes into them) and
/// the window's minimum inner size. None when there are no monitors to place it on.
pub fn place(
    saved: &Saved,
    monitors: &[Monitor],
    primary: Option<usize>,
    min: (f64, f64),
) -> Option<Placement> {
    let usable: Vec<&Monitor> = monitors
        .iter()
        .filter(|m| m.width > 0.0 && m.height > 0.0 && m.scale > 0.0 && m.scale.is_finite())
        .collect();
    if usable.is_empty() {
        return None;
    }
    // The monitor the saved window overlaps most (its size measured on that monitor).
    let best = usable
        .iter()
        .map(|m| {
            let (w, h) = outer_size(saved.width, saved.height, m.scale);
            let ix = overlap(saved.x, w, m.x, m.width);
            let iy = overlap(saved.y, h, m.y, m.height);
            (*m, ix * iy)
        })
        .filter(|(_, area)| *area > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(m, _)| m);
    if let Some(m) = best {
        let (width, height) = fit_size(saved.width, saved.height, m, min);
        let (ow, oh) = outer_size(width, height, m.scale);
        let slack = EDGE_SLACK * m.scale;
        return Some(Placement {
            x: clamp_edge(saved.x, ow, m.x, m.width, slack),
            y: clamp_edge(saved.y, oh, m.y, m.height, slack),
            width,
            height,
            maximized: saved.maximized,
        });
    }
    // Off every monitor: centred on the primary one.
    let m = primary
        .and_then(|i| monitors.get(i))
        .filter(|m| usable.contains(m))
        .unwrap_or(usable[0]);
    let (width, height) = fit_size(saved.width, saved.height, m, min);
    let (ow, oh) = outer_size(width, height, m.scale);
    Some(Placement {
        x: m.x + ((m.width - ow) / 2.0).max(0.0),
        y: m.y + ((m.height - oh) / 2.0).max(0.0),
        width,
        height,
        maximized: saved.maximized,
    })
}

fn outer_size(width: f64, height: f64, scale: f64) -> (f64, f64) {
    (width * scale, (height + FRAME_HEIGHT) * scale)
}

fn overlap(a: f64, a_len: f64, b: f64, b_len: f64) -> f64 {
    ((a + a_len).min(b + b_len) - a.max(b)).max(0.0)
}

/// The inner size made to fit the monitor's work area, but not smaller than the minimum (the
/// system enforces the minimum anyway; on a monitor smaller than that, the work area wins).
fn fit_size(width: f64, height: f64, m: &Monitor, min: (f64, f64)) -> (f64, f64) {
    let avail_w = m.width / m.scale;
    let avail_h = (m.height / m.scale - FRAME_HEIGHT).max(1.0);
    (
        width.max(min.0).min(avail_w).round(),
        height.max(min.1).min(avail_h).round(),
    )
}

/// Moves a span of `len` at `pos` inside `[start, start + size]`, allowing `slack` past each
/// edge; a span longer than the monitor starts at its edge.
fn clamp_edge(pos: f64, len: f64, start: f64, size: f64, slack: f64) -> f64 {
    let lo = start - slack;
    let hi = start + size - len + slack;
    if hi < lo {
        start
    } else {
        pos.clamp(lo, hi)
    }
}

// ── The app side ────────────────────────────────────────────────────────────

static PATH: OnceLock<PathBuf> = OnceLock::new();
/// The latest state of the main window, written by the saver.
static LATEST: Mutex<Option<Saved>> = Mutex::new(None);
/// Bumped on every change; the saver writes only when it moved past what was written.
static CHANGES: AtomicU64 = AtomicU64::new(0);
static WRITTEN: AtomicU64 = AtomicU64::new(0);
/// One writer at a time (the saver thread and the exit).
static WRITE: Mutex<()> = Mutex::new(());
static SAVER: Mutex<Option<Sender<()>>> = Mutex::new(None);

/// macOS: the desktop is in points; elsewhere in physical pixels (see the module doc).
const IN_POINTS: bool = cfg!(target_os = "macos");

/// Places the main window (the saved place, or centred when there is none) and shows it.
/// `center` is the app's default placement.
pub fn place_main<R: Runtime>(app: &AppHandle<R>, center: impl FnOnce(&AppHandle<R>)) {
    let Some(win) = app.get_webview_window(MAIN) else {
        return;
    };
    if let Ok(home) = app.path().home_dir() {
        let _ = PATH.set(home.join(".ps5upload").join(FILE_NAME));
    }
    let restored = crate::crash_hook::optional("restoring the window's place", || restore(app));
    if restored != Some(true) {
        center(app);
    }
    let _ = win.show();
    let _ = win.set_focus();
    if restored == Some(true)
        && LATEST
            .lock()
            .ok()
            .and_then(|l| *l)
            .is_some_and(|s| s.maximized)
    {
        let _ = win.maximize();
    }
}

fn restore<R: Runtime>(app: &AppHandle<R>) -> bool {
    let Some(win) = app.get_webview_window(MAIN) else {
        return false;
    };
    let Some(saved) = PATH
        .get()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| parse(&b))
    else {
        return false;
    };
    let Ok(list) = win.available_monitors() else {
        return false;
    };
    let primary = win.primary_monitor().ok().flatten();
    let mut primary_index = None;
    let monitors: Vec<Monitor> = list
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if primary
                .as_ref()
                .is_some_and(|p| p.position() == m.position() && p.size() == m.size())
            {
                primary_index = Some(i);
            }
            let area = m.work_area();
            let s = m.scale_factor();
            let to_native = if IN_POINTS { 1.0 / s } else { 1.0 };
            Monitor {
                x: area.position.x as f64 * to_native,
                y: area.position.y as f64 * to_native,
                width: area.size.width as f64 * to_native,
                height: area.size.height as f64 * to_native,
                scale: if IN_POINTS { 1.0 } else { s },
            }
        })
        .collect();
    let min = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == MAIN)
        .map(|w| (w.min_width.unwrap_or(0.0), w.min_height.unwrap_or(0.0)))
        .unwrap_or((0.0, 0.0));
    let Some(p) = place(&saved, &monitors, primary_index, min) else {
        return false;
    };
    // The position first: moving onto a monitor with another scale resizes the window there,
    // and the logical size below is then measured on the right monitor.
    if IN_POINTS {
        let _ = win.set_position(tauri::LogicalPosition::new(p.x, p.y));
    } else {
        let _ = win.set_position(tauri::PhysicalPosition::new(
            p.x.round() as i32,
            p.y.round() as i32,
        ));
    }
    let _ = win.set_size(tauri::LogicalSize::new(p.width, p.height));
    if let Ok(mut l) = LATEST.lock() {
        *l = Some(Saved {
            x: p.x,
            y: p.y,
            width: p.width,
            height: p.height,
            maximized: p.maximized,
        });
    }
    true
}

/// From the app's window events: the main window moved or resized.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if window.label() != MAIN
        || !matches!(
            event,
            WindowEvent::Moved(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
        )
    {
        return;
    }
    if let Some(next) = current(window) {
        let changed = match LATEST.lock() {
            Ok(mut l) => {
                let differs = *l != Some(next);
                *l = Some(next);
                differs
            }
            Err(_) => false,
        };
        if changed {
            CHANGES.fetch_add(1, Ordering::SeqCst);
            schedule();
        }
    }
}

/// The window's state to save now, or None when it should not be saved (minimized, full screen,
/// not readable). Maximized keeps the last normal geometry.
fn current<R: Runtime>(window: &Window<R>) -> Option<Saved> {
    if window.is_minimized().unwrap_or(true) || window.is_fullscreen().unwrap_or(true) {
        return None;
    }
    let previous = LATEST.lock().ok().and_then(|l| *l);
    if window.is_maximized().unwrap_or(false) {
        return previous.map(|p| Saved {
            maximized: true,
            ..p
        });
    }
    let scale = window.scale_factor().ok()?;
    let pos = window.outer_position().ok()?;
    let size = window.inner_size().ok()?.to_logical::<f64>(scale);
    let (x, y) = if IN_POINTS {
        (pos.x as f64 / scale, pos.y as f64 / scale)
    } else {
        (pos.x as f64, pos.y as f64)
    };
    // Windows parks a minimized window at -32000 (in case the flag lags the event).
    if x <= -30000.0 || y <= -30000.0 || size.width < 1.0 || size.height < 1.0 {
        return None;
    }
    Some(Saved {
        x,
        y,
        width: size.width.round(),
        height: size.height.round(),
        maximized: false,
    })
}

/// Writes the latest state half a second after the last change (one thread, started on first use).
fn schedule() {
    let Ok(mut saver) = SAVER.lock() else {
        return;
    };
    if saver.as_ref().is_some_and(|tx| tx.send(()).is_ok()) {
        return;
    }
    let (tx, rx) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("window-state".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                // Quiet for SAVE_DELAY, then write.
                while rx.recv_timeout(SAVE_DELAY).is_ok() {}
                flush();
            }
        });
    if spawned.is_ok() {
        let _ = tx.send(());
        *saver = Some(tx);
    }
}

/// Writes the latest state if it changed since the last write. Called at exit too.
pub fn flush() {
    let Ok(_guard) = WRITE.lock() else {
        return;
    };
    let change = CHANGES.load(Ordering::SeqCst);
    if change == WRITTEN.load(Ordering::SeqCst) {
        return;
    }
    let (Some(path), Some(state)) = (PATH.get(), LATEST.lock().ok().and_then(|l| *l)) else {
        return;
    };
    if write(path, &state).is_ok() {
        WRITTEN.store(change, Ordering::SeqCst);
    }
}

fn write(path: &Path, state: &Saved) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Whole or not at all: a temporary file beside it, then a rename in the same folder.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: (f64, f64) = (960.0, 640.0);

    fn mon(x: f64, y: f64, w: f64, h: f64, scale: f64) -> Monitor {
        Monitor {
            x,
            y,
            width: w,
            height: h,
            scale,
        }
    }

    fn saved(x: f64, y: f64, w: f64, h: f64) -> Saved {
        Saved {
            x,
            y,
            width: w,
            height: h,
            maximized: false,
        }
    }

    #[test]
    fn a_window_on_screen_opens_where_it_was() {
        let monitors = [mon(0.0, 0.0, 2560.0, 1400.0, 1.0)];
        let p = place(&saved(100.0, 50.0, 1280.0, 820.0), &monitors, Some(0), MIN).unwrap();
        assert_eq!((p.x, p.y, p.width, p.height), (100.0, 50.0, 1280.0, 820.0));
    }

    #[test]
    fn a_window_on_an_unplugged_monitor_is_centred_on_the_primary() {
        // Was on a monitor to the right that is gone now.
        let monitors = [mon(0.0, 0.0, 1920.0, 1040.0, 1.0)];
        let p = place(
            &saved(2500.0, 100.0, 1280.0, 820.0),
            &monitors,
            Some(0),
            MIN,
        )
        .unwrap();
        assert_eq!((p.width, p.height), (1280.0, 820.0));
        assert_eq!(p.x, (1920.0 - 1280.0) / 2.0);
        assert_eq!(p.y, (1040.0 - 860.0) / 2.0);
    }

    #[test]
    fn the_primary_monitor_is_used_even_when_it_is_not_first() {
        let monitors = [
            mon(-1920.0, 0.0, 1920.0, 1080.0, 1.0),
            mon(0.0, 0.0, 2560.0, 1440.0, 1.0),
        ];
        let p = place(
            &saved(9000.0, 9000.0, 1280.0, 820.0),
            &monitors,
            Some(1),
            MIN,
        )
        .unwrap();
        assert!(p.x >= 0.0 && p.x + 1280.0 <= 2560.0);
    }

    #[test]
    fn a_window_partly_off_screen_is_pulled_on_and_shrunk_to_fit() {
        // A smaller screen than when it was saved.
        let monitors = [mon(0.0, 0.0, 1366.0, 728.0, 1.0)];
        let p = place(
            &saved(800.0, 400.0, 1800.0, 1000.0),
            &monitors,
            Some(0),
            MIN,
        )
        .unwrap();
        assert_eq!(p.width, 1366.0);
        assert_eq!(p.height, 728.0 - FRAME_HEIGHT);
        // Fits within the slack.
        assert!(p.x >= -EDGE_SLACK && p.x + p.width <= 1366.0 + EDGE_SLACK);
        assert!(p.y >= -EDGE_SLACK && p.y + p.height + FRAME_HEIGHT <= 728.0 + EDGE_SLACK);
    }

    #[test]
    fn the_size_never_goes_below_the_minimum_unless_the_screen_is_smaller() {
        let big = [mon(0.0, 0.0, 1920.0, 1040.0, 1.0)];
        let p = place(&saved(10.0, 10.0, 300.0, 200.0), &big, Some(0), MIN).unwrap();
        assert_eq!((p.width, p.height), MIN);
        let tiny = [mon(0.0, 0.0, 800.0, 600.0, 1.0)];
        let p = place(&saved(10.0, 10.0, 1280.0, 820.0), &tiny, Some(0), MIN).unwrap();
        assert_eq!((p.width, p.height), (800.0, 600.0 - FRAME_HEIGHT));
    }

    #[test]
    fn mixed_scaling_measures_the_size_on_the_target_monitor() {
        // Windows / Linux: a 4K monitor at 200 % right of a 1080p one at 100 %, physical pixels.
        let monitors = [
            mon(0.0, 0.0, 1920.0, 1040.0, 1.0),
            mon(1920.0, 0.0, 3840.0, 2080.0, 2.0),
        ];
        let p = place(
            &saved(2200.0, 100.0, 1280.0, 820.0),
            &monitors,
            Some(0),
            MIN,
        )
        .unwrap();
        // Stays on the 4K monitor, same logical size (2560 x 1720 physical fits in 3840 x 2080).
        assert_eq!(
            (p.x, p.y, p.width, p.height),
            (2200.0, 100.0, 1280.0, 820.0)
        );
        // Too far right for its doubled size: moved left onto the 4K monitor, not the 1080p one.
        let p = place(
            &saved(4800.0, 100.0, 1280.0, 820.0),
            &monitors,
            Some(0),
            MIN,
        )
        .unwrap();
        assert_eq!(p.x, 1920.0 + 3840.0 - 2560.0 + EDGE_SLACK * 2.0);
    }

    #[test]
    fn the_monitor_it_overlaps_most_wins() {
        let monitors = [
            mon(0.0, 0.0, 1920.0, 1080.0, 1.0),
            mon(1920.0, 0.0, 1920.0, 1080.0, 1.0),
        ];
        // Mostly on the second monitor.
        let p = place(
            &saved(1700.0, 100.0, 1280.0, 820.0),
            &monitors,
            Some(0),
            MIN,
        )
        .unwrap();
        assert_eq!(p.x, 1920.0 - EDGE_SLACK);
    }

    #[test]
    fn maximized_is_kept() {
        let monitors = [mon(0.0, 0.0, 1920.0, 1080.0, 1.0)];
        let mut s = saved(100.0, 100.0, 1280.0, 820.0);
        s.maximized = true;
        assert!(place(&s, &monitors, Some(0), MIN).unwrap().maximized);
        // Off screen too.
        s.x = -5000.0;
        assert!(place(&s, &monitors, Some(0), MIN).unwrap().maximized);
    }

    #[test]
    fn no_monitors_means_no_placement() {
        let s = saved(0.0, 0.0, 1280.0, 820.0);
        assert_eq!(place(&s, &[], None, MIN), None);
        assert_eq!(
            place(&s, &[mon(0.0, 0.0, 0.0, 0.0, 1.0)], Some(0), MIN),
            None
        );
    }

    #[test]
    fn a_saved_file_reads_back() {
        let s = Saved {
            x: -1200.0,
            y: 30.0,
            width: 1400.0,
            height: 900.0,
            maximized: true,
        };
        assert_eq!(parse(&serde_json::to_vec(&s).unwrap()), Some(s));
        // Written before `maximized` existed / by hand.
        assert_eq!(
            parse(br#"{"x":1,"y":2,"width":1000,"height":700}"#).map(|s| s.maximized),
            Some(false)
        );
    }

    #[test]
    fn a_corrupt_saved_file_is_ignored() {
        for bad in [
            &b""[..],
            b"not json",
            b"{",
            b"null",
            b"[]",
            b"{\"x\":1,\"y\":2}",
            b"{\"x\":\"a\",\"y\":2,\"width\":1000,\"height\":700}",
            b"{\"x\":1,\"y\":2,\"width\":0,\"height\":700}",
            b"{\"x\":1,\"y\":2,\"width\":-5,\"height\":700}",
            b"{\"x\":1e308,\"y\":2,\"width\":1000,\"height\":700}",
            b"{\"x\":1,\"y\":2,\"width\":1000,\"height\":1e9}",
            b"\xff\xfe\x00garbage",
        ] {
            assert_eq!(parse(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn a_write_then_read_round_trips_and_leaves_no_temporary_file() {
        let dir = crate::test_tmp::temp_path(format!("ps5u-window-state-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        let s = saved(10.0, 20.0, 1300.0, 850.0);
        write(&path, &s).unwrap();
        assert_eq!(parse(&std::fs::read(&path).unwrap()), Some(s));
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
