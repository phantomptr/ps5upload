//! A DualSense's touchpad while Remote Play plays (#589).
//!
//! The webview's Gamepad API gives buttons and sticks but never the fingers on the touchpad,
//! and the Mac's WKWebView (like WebKitGTK) has no WebHID. So the desktop app reads the
//! controller's own input reports here, with hidapi, and hands the page the first finger and
//! the touchpad click as `pad-touchpad` events (lib/padTouchpad.ts feeds them into the pad the
//! console gets, next to the on-screen controller's touchpad).
//!
//! Shared, read-only access: the device is opened non-exclusively on macOS
//! (`macos-shared-device`), nothing is ever written to it, and it is not switched into another
//! report mode, so the system's own controller support (GameController, WebView2, the Gamepad
//! API) keeps reading it as before. That also means a DualSense on Bluetooth gives touches only
//! when it already sends its full report (0x31): macOS does put it in that mode, other
//! systems may leave it in the short report, which carries no touchpad.
//!
//! Reading starts when a stream plays and stops when it ends (`pad_touchpad_set`); a
//! controller plugged in or paired later is found within a second. Desktop only: on Android
//! and iOS the command answers that it has no reader.

use serde::Serialize;

/// Sony's USB vendor id.
pub const SONY_VID: u16 = 0x054c;
/// DualSense, DualSense Edge.
pub const DUALSENSE_PIDS: [u16; 2] = [0x0ce6, 0x0df2];

/// One finger on the touchpad: its tracking id and where it is (x 0-1919, y 0-1079).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finger {
    pub id: u8,
    pub x: u16,
    pub y: u16,
}

/// The touchpad part of one input report: up to two fingers and the click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TouchReport {
    pub fingers: [Option<Finger>; 2],
    pub click: bool,
}

/// The touchpad in a DualSense input report, as read with its report id first: USB report
/// 0x01 (64 bytes) or Bluetooth report 0x31 (78 bytes). None for any other report, including
/// the short Bluetooth 0x01 report, which has no touchpad.
///
/// After the report id (and, on Bluetooth, one more byte) the state is laid out the same way
/// (SDL's `PS5StatePacket_t`): byte 9 holds PS (bit 0) and the touchpad click (bit 1); bytes
/// 32-35 and 36-39 are the two fingers, each a byte with bit 7 set while the finger is up and
/// the tracking id below it, then 12-bit x and y packed into three bytes.
pub fn parse_dualsense_report(buf: &[u8]) -> Option<TouchReport> {
    let state = match buf.first()? {
        0x01 if buf.len() >= 64 => &buf[1..],
        0x31 if buf.len() >= 78 => &buf[2..],
        _ => return None,
    };
    let finger = |at: usize| {
        let b = &state[at..at + 4];
        if b[0] & 0x80 != 0 {
            return None;
        }
        Some(Finger {
            id: b[0] & 0x7f,
            x: u16::from(b[1]) | (u16::from(b[2] & 0x0f) << 8),
            y: u16::from(b[2] >> 4) | (u16::from(b[3]) << 4),
        })
    };
    Some(TouchReport {
        fingers: [finger(32), finger(36)],
        click: state[9] & 0x02 != 0,
    })
}

/// What the page is told: the finger it follows (raw x 0-1919, y 0-1079) and the click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct TouchEvent {
    pub touch: Option<[u16; 2]>,
    pub click: bool,
}

/// Follows one finger from the moment it lands until it lifts, so a second finger never makes
/// the point jump; once it lifts the other finger (if still down) is followed from its own
/// position, after the page has been told about the lift.
#[derive(Debug, Default)]
pub struct Follower {
    following: Option<u8>,
    /// The followed finger lifted: report the lift before taking up another finger.
    lifted: bool,
}

impl Follower {
    pub fn next(&mut self, r: &TouchReport) -> TouchEvent {
        let down = |id: u8| r.fingers.iter().flatten().find(|f| f.id == id).copied();
        let mut touch = None;
        if let Some(id) = self.following {
            match down(id) {
                Some(f) => touch = Some(f),
                None => {
                    self.following = None;
                    self.lifted = true;
                }
            }
        } else if self.lifted {
            self.lifted = false;
        } else if let Some(f) = r.fingers.iter().flatten().next() {
            self.following = Some(f.id);
            touch = Some(*f);
        }
        TouchEvent {
            touch: touch.map(|f| [f.x.min(1919), f.y.min(1079)]),
            click: r.click,
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod reader {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Emitter};

    /// The running reader's stop flag.
    static RUNNING: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

    /// Moves closer together than this are coalesced (the pad reports at 250 Hz or more; the
    /// page reads once a frame). A lift, a landing or a click is always sent at once.
    const MOVE_EVERY: Duration = Duration::from_millis(8);

    pub fn set(app: AppHandle, on: bool) {
        let mut cur = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            if cur.is_some() {
                return;
            }
            let stop = Arc::new(AtomicBool::new(false));
            *cur = Some(stop.clone());
            let spawned = std::thread::Builder::new()
                .name("pad-touchpad".into())
                .spawn(move || run(app, stop));
            if let Err(e) = spawned {
                eprintln!("[pad-touchpad] could not start: {e}");
                *cur = None;
            }
        } else if let Some(stop) = cur.take() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Sleep `d` in short steps; false once stopped.
    fn wait(stop: &AtomicBool, d: Duration) -> bool {
        let until = Instant::now() + d;
        while Instant::now() < until {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        !stop.load(Ordering::Relaxed)
    }

    fn run(app: AppHandle, stop: Arc<AtomicBool>) {
        let mut api = match hidapi::HidApi::new() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[pad-touchpad] no HID access: {e}");
                return;
            }
        };
        let mut refused: Option<std::ffi::CString> = None;
        while !stop.load(Ordering::Relaxed) {
            let _ = api.reset_devices();
            let _ = api.add_devices(SONY_VID, 0);
            // The game pad collection (a platform that does not say which is which: any).
            let found = api
                .device_list()
                .find(|d| {
                    DUALSENSE_PIDS.contains(&d.product_id())
                        && (d.usage_page() == 0 || (d.usage_page() == 1 && d.usage() == 5))
                })
                .map(|d| d.path().to_owned());
            let Some(path) = found else {
                if !wait(&stop, Duration::from_secs(1)) {
                    break;
                }
                continue;
            };
            let dev = match api.open_path(&path) {
                Ok(d) => d,
                Err(e) => {
                    // Linux without a udev rule for it, say: tell the log once per device.
                    if refused.as_ref() != Some(&path) {
                        eprintln!("[pad-touchpad] cannot open the DualSense: {e}");
                        refused = Some(path);
                    }
                    if !wait(&stop, Duration::from_secs(2)) {
                        break;
                    }
                    continue;
                }
            };
            refused = None;
            eprintln!("[pad-touchpad] reading the DualSense touchpad");
            read(&app, &dev, &stop);
        }
    }

    /// Read reports until stopped or the controller goes away; the page is left with no finger
    /// and no click either way.
    fn read(app: &AppHandle, dev: &hidapi::HidDevice, stop: &AtomicBool) {
        let mut buf = [0u8; 128];
        let mut follower = Follower::default();
        let mut sent = TouchEvent::default();
        let mut sent_at = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let n = match dev.read_timeout(&mut buf, 100) {
                Ok(0) => continue,
                Ok(n) => n,
                Err(_) => break,
            };
            let Some(r) = parse_dualsense_report(&buf[..n]) else {
                continue;
            };
            let ev = follower.next(&r);
            if ev == sent {
                continue;
            }
            let moved_only = ev.click == sent.click && ev.touch.is_some() && sent.touch.is_some();
            if moved_only && sent_at.elapsed() < MOVE_EVERY {
                continue;
            }
            let _ = app.emit("pad-touchpad", ev);
            sent = ev;
            sent_at = Instant::now();
        }
        if sent != TouchEvent::default() {
            let _ = app.emit("pad-touchpad", TouchEvent::default());
        }
    }
}

/// Read the DualSense touchpad (on) or stop (off). True where the app has a reader (desktop).
#[tauri::command]
pub fn pad_touchpad_set(app: tauri::AppHandle, on: bool) -> bool {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        reader::set(app, on);
        true
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = (app, on);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finger's four bytes: up flag + id, then 12-bit x and y.
    fn finger(down: bool, id: u8, x: u16, y: u16) -> [u8; 4] {
        [
            (if down { 0 } else { 0x80 }) | (id & 0x7f),
            x as u8,
            ((x >> 8) as u8 & 0x0f) | ((y as u8 & 0x0f) << 4),
            (y >> 4) as u8,
        ]
    }

    /// A report with the state at `off`: `state` is the 40+ state bytes after the header.
    fn report(id: u8, len: usize, off: usize, click: bool, f1: [u8; 4], f2: [u8; 4]) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0] = id;
        if click {
            b[off + 9] |= 0x02;
        }
        b[off + 9] |= 0x01; // PS held too: only bit 1 is the click
        b[off + 32..off + 36].copy_from_slice(&f1);
        b[off + 36..off + 40].copy_from_slice(&f2);
        b
    }

    fn usb(click: bool, f1: [u8; 4], f2: [u8; 4]) -> Vec<u8> {
        report(0x01, 64, 1, click, f1, f2)
    }

    fn bt(click: bool, f1: [u8; 4], f2: [u8; 4]) -> Vec<u8> {
        report(0x31, 78, 2, click, f1, f2)
    }

    const UP: [u8; 4] = [0x80, 0, 0, 0];

    #[test]
    fn usb_report_with_two_fingers() {
        let r = parse_dualsense_report(&usb(
            false,
            finger(true, 5, 1919, 1079),
            finger(true, 6, 0, 1),
        ))
        .unwrap();
        assert_eq!(
            r.fingers,
            [
                Some(Finger {
                    id: 5,
                    x: 1919,
                    y: 1079
                }),
                Some(Finger { id: 6, x: 0, y: 1 })
            ]
        );
        assert!(!r.click);
    }

    #[test]
    fn bluetooth_report_with_one_finger_and_the_click() {
        let r = parse_dualsense_report(&bt(true, finger(true, 9, 960, 540), UP)).unwrap();
        assert_eq!(
            r.fingers,
            [
                Some(Finger {
                    id: 9,
                    x: 960,
                    y: 540
                }),
                None
            ]
        );
        assert!(r.click);
    }

    #[test]
    fn a_lifted_finger_keeps_its_last_position_but_is_up() {
        let r = parse_dualsense_report(&usb(false, finger(false, 5, 700, 300), UP)).unwrap();
        assert_eq!(r.fingers, [None, None]);
    }

    #[test]
    fn known_bytes_decode() {
        // x = 0x345, y = 0x2a1: bytes 0x45, (0x1 << 4) | 0x3, 0x2a.
        let mut b = vec![0u8; 64];
        b[0] = 0x01;
        b[33..37].copy_from_slice(&[0x03, 0x45, 0x13, 0x2a]);
        b[37] = 0x80;
        let r = parse_dualsense_report(&b).unwrap();
        assert_eq!(
            r.fingers[0],
            Some(Finger {
                id: 3,
                x: 0x345,
                y: 0x2a1
            })
        );
    }

    #[test]
    fn other_reports_are_not_touchpads() {
        // The short Bluetooth report (no touchpad), a truncated report, other ids, nothing.
        assert_eq!(parse_dualsense_report(&[0x01; 10]), None);
        assert_eq!(parse_dualsense_report(&[0x31; 40]), None);
        assert_eq!(parse_dualsense_report(&[0x02; 64]), None);
        assert_eq!(parse_dualsense_report(&[]), None);
    }

    fn ev(f: &mut Follower, b: Vec<u8>) -> TouchEvent {
        f.next(&parse_dualsense_report(&b).unwrap())
    }

    #[test]
    fn the_first_finger_is_followed_until_it_lifts() {
        let mut f = Follower::default();
        assert_eq!(
            ev(&mut f, usb(false, finger(true, 1, 100, 200), UP)).touch,
            Some([100, 200])
        );
        // A second finger lands in slot 0's place; the first moved to slot 1.
        let two = usb(false, finger(true, 2, 1500, 900), finger(true, 1, 110, 210));
        assert_eq!(ev(&mut f, two).touch, Some([110, 210]));
        // The first lifts: the page hears the lift before the other finger is taken up.
        let one = usb(
            false,
            finger(true, 2, 1500, 900),
            finger(false, 1, 110, 210),
        );
        assert_eq!(ev(&mut f, one.clone()).touch, None);
        assert_eq!(ev(&mut f, one.clone()).touch, None);
        assert_eq!(ev(&mut f, one).touch, Some([1500, 900]));
        // All up.
        assert_eq!(ev(&mut f, usb(false, UP, UP)), TouchEvent::default());
    }

    #[test]
    fn the_click_is_reported_with_or_without_a_finger() {
        let mut f = Follower::default();
        let e = ev(&mut f, bt(true, finger(true, 4, 10, 20), UP));
        assert_eq!(
            e,
            TouchEvent {
                touch: Some([10, 20]),
                click: true
            }
        );
        assert!(ev(&mut f, bt(true, UP, UP)).click);
        assert!(!ev(&mut f, bt(false, UP, UP)).click);
    }
}
