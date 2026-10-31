//! A DualSense's touchpad while Remote Play plays (#589).
//!
//! The webview's Gamepad API gives buttons and sticks but never the fingers on the touchpad,
//! and the Mac's WKWebView (like WebKitGTK) has no WebHID. So the desktop app reads the
//! controller's own input reports here, with hidapi, and hands the page the first finger and
//! the touchpad click as `pad-touchpad` events (lib/padTouchpad.ts feeds them into the pad the
//! console gets, next to the on-screen controller's touchpad).
//!
//! The same reports carry the motion sensors: the gyro and accelerometer, scaled with the pad's
//! own calibration as SDL does, go to the page as `pad-motion` events and on to the console's
//! state packet (engine rp_stream/feedback.rs), so gyro aiming and tilt work.
//!
//! Shared access: the device is opened non-exclusively on macOS (`macos-shared-device`) and
//! never switched into another report mode, so the system's own controller support
//! (GameController, WebView2, the Gamepad API) keeps reading it as before. The only writes are
//! the adaptive triggers of a DualSense-mode session (pad_dualsense.rs), and over Bluetooth only
//! while the pad already sends its full report. That also means a DualSense on Bluetooth gives
//! touches and motion only when it already sends its full report (0x31): macOS does put it in
//! that mode, other systems may leave it in the short report, which carries neither.
//!
//! Reading starts when a stream plays and stops when it ends (`pad_touchpad_set`); a
//! controller plugged in or paired later is found within a second. Desktop only: on Android
//! and iOS the command answers that it has no reader.

// The reader is desktop-only; on a phone only the commands' answers remain.
#![cfg_attr(any(target_os = "android", target_os = "ios"), allow(dead_code))]

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

/// The motion sensors in a DualSense input report (same reports as the touchpad): gyro (pitch,
/// yaw, roll) and accelerometer (x, y, z), raw, and the sensor clock (0.33 µs ticks, wrapping).
/// At state bytes 15-26 and 27-30 (SDL `PS5StatePacket_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMotion {
    pub gyro: [i16; 3],
    pub accel: [i16; 3],
    pub ticks: u32,
}

/// The state bytes of a full report: USB 0x01 (64 bytes) or Bluetooth 0x31 (78 bytes).
fn full_state(buf: &[u8]) -> Option<&[u8]> {
    match buf.first()? {
        0x01 if buf.len() >= 64 => Some(&buf[1..]),
        0x31 if buf.len() >= 78 => Some(&buf[2..]),
        _ => None,
    }
}

pub fn parse_dualsense_motion(buf: &[u8]) -> Option<RawMotion> {
    let st = full_state(buf)?;
    let w = |at: usize| i16::from_le_bytes([st[at], st[at + 1]]);
    Some(RawMotion {
        gyro: [w(15), w(17), w(19)],
        accel: [w(21), w(23), w(25)],
        ticks: u32::from_le_bytes(st[27..31].try_into().ok()?),
    })
}

/// SDL's resolutions: raw units per °/s and per g.
const GYRO_RES_PER_DEGREE: f32 = 1024.0;
const ACCEL_RES_PER_G: f32 = 8192.0;

/// One axis's factory calibration: subtract the bias, multiply by the sensitivity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Axis {
    bias: f32,
    sensitivity: f32,
}

/// A pad's factory calibration (feature report 0x05), as SDL reads it
/// (`HIDAPI_DriverPS5_LoadCalibrationData`). Without it (or with a bad one) the raw values are
/// scaled as SDL does: the gyro ×64, the accelerometer as is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    axes: [Axis; 6],
}

impl Default for Calibration {
    fn default() -> Self {
        let a = |s| Axis {
            bias: 0.0,
            sensitivity: s,
        };
        Calibration {
            axes: [a(64.0), a(64.0), a(64.0), a(1.0), a(1.0), a(1.0)],
        }
    }
}

impl Calibration {
    /// From feature report 0x05, report id first (41 bytes). None if short or implausible.
    pub fn parse(d: &[u8]) -> Option<Self> {
        if d.len() < 35 || d[0] != 0x05 {
            return None;
        }
        let w = |at: usize| f32::from(i16::from_le_bytes([d[at], d[at + 1]]));
        let speed = (w(19) + w(21)) * GYRO_RES_PER_DEGREE;
        let gyro = |bias: usize, plus: usize, minus: usize| Axis {
            bias: w(bias),
            sensitivity: speed / (w(plus) - w(minus)),
        };
        let accel = |plus: usize, minus: usize| {
            let range = w(plus) - w(minus);
            Axis {
                // SDL computes this in integers.
                bias: w(plus) - (range / 2.0).trunc(),
                sensitivity: 2.0 * ACCEL_RES_PER_G / range,
            }
        };
        let c = Calibration {
            axes: [
                gyro(1, 7, 9),
                gyro(3, 11, 13),
                gyro(5, 15, 17),
                accel(23, 25),
                accel(27, 29),
                accel(31, 33),
            ],
        };
        // Some controllers have a bad calibration (SDL): then none.
        let ok = c.axes.iter().enumerate().all(|(i, a)| {
            let divisor = if i < 3 { 64.0 } else { 1.0 };
            a.bias.abs() <= 1024.0
                && a.sensitivity.is_finite()
                && (1.0 - a.sensitivity / divisor).abs() <= 0.5
        });
        ok.then_some(c)
    }

    /// Gyro in mrad/s and acceleration in thousandths of g, in the pad's axes (x right, y up out
    /// of its face, z toward the player: the axes the console and SDL use).
    pub fn apply(&self, m: &RawMotion) -> ([i32; 3], [i32; 3]) {
        let v =
            |i: usize, raw: i16| (f32::from(raw) - self.axes[i].bias) * self.axes[i].sensitivity;
        let gyro = |i: usize| {
            let rad = v(i, m.gyro[i]) / GYRO_RES_PER_DEGREE * std::f32::consts::PI / 180.0;
            (rad * 1000.0).round() as i32
        };
        let accel = |i: usize| (v(i + 3, m.accel[i]) / ACCEL_RES_PER_G * 1000.0).round() as i32;
        ([gyro(0), gyro(1), gyro(2)], [accel(0), accel(1), accel(2)])
    }
}

/// The sensor clock in µs, from its 0.33 µs ticks: counted up across the tick counter's wrap,
/// then wrapping at 2^32 µs as the console's state expects.
#[derive(Debug, Default)]
pub struct SensorClock {
    last: Option<u32>,
    ticks: u64,
}

impl SensorClock {
    pub fn us(&mut self, ticks: u32) -> u32 {
        if let Some(l) = self.last {
            self.ticks += u64::from(ticks.wrapping_sub(l));
        }
        self.last = Some(ticks);
        (self.ticks / 3) as u32
    }
}

/// A motion reading as the page sends it on (engine `MotionSample`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MotionEvent {
    pub gyro: [i32; 3],
    pub accel: [i32; 3],
    pub t: u32,
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
    use crate::commands::pad_dualsense::{self, Transport};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Emitter};

    /// The running reader's stop flag, and a flag it raises once it has let go of the pad.
    static RUNNING: Mutex<Option<(Arc<AtomicBool>, Arc<AtomicBool>)>> = Mutex::new(None);

    /// Moves closer together than this are coalesced (the pad reports at 250 Hz or more; the
    /// page reads once a frame). A lift, a landing or a click is always sent at once.
    const MOVE_EVERY: Duration = Duration::from_millis(8);
    /// Motion readings closer together than this are skipped (the page reads once a frame;
    /// each reading carries its own sensor time, so the console's orientation stays right).
    const MOTION_EVERY: Duration = Duration::from_millis(8);

    pub fn set(app: AppHandle, on: bool) {
        let mut cur = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            if cur.is_some() {
                return;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            *cur = Some((stop.clone(), done.clone()));
            let spawned = std::thread::Builder::new()
                .name("pad-touchpad".into())
                .spawn(move || {
                    run(app, stop);
                    done.store(true, Ordering::Relaxed);
                });
            if let Err(e) = spawned {
                eprintln!("[pad-touchpad] could not start: {e}");
                *cur = None;
            }
        } else if let Some((stop, _)) = cur.take() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// The app is quitting: stop reading and give the reader a moment to let the triggers go.
    pub fn stop_on_exit() {
        let cur = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some((stop, done)) = cur {
            stop.store(true, Ordering::Relaxed);
            let until = Instant::now() + Duration::from_millis(300);
            while !done.load(Ordering::Relaxed) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Is a DualSense attached, and on USB? A quick look, without opening it.
    pub fn find() -> (bool, bool) {
        let Ok(mut api) = hidapi::HidApi::new() else {
            return (false, false);
        };
        let _ = api.reset_devices();
        let _ = api.add_devices(SONY_VID, 0);
        let mut found = (false, false);
        for d in api
            .device_list()
            .filter(|d| DUALSENSE_PIDS.contains(&d.product_id()))
        {
            found.0 = true;
            if matches!(d.bus_type(), hidapi::BusType::Usb) {
                found.1 = true;
            }
        }
        found
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

    /// The pad's factory calibration, read once its full report shows (on Bluetooth, asking
    /// for it in the short-report mode would switch the pad over).
    fn calibration(dev: &hidapi::HidDevice) -> Calibration {
        let mut f = [0u8; 64];
        f[0] = 0x05;
        match dev.get_feature_report(&mut f) {
            Ok(n) => Calibration::parse(&f[..n]).unwrap_or_default(),
            Err(_) => Calibration::default(),
        }
    }

    /// Read reports until stopped or the controller goes away; the page is left with no finger,
    /// no click and no motion either way, and the triggers are let go.
    fn read(app: &AppHandle, dev: &hidapi::HidDevice, stop: &AtomicBool) {
        let mut buf = [0u8; 128];
        let mut follower = Follower::default();
        let mut sent = TouchEvent::default();
        let mut sent_at = Instant::now();
        let mut cal: Option<Calibration> = None;
        let mut clock = SensorClock::default();
        let mut motion_at: Option<Instant> = None;
        let mut transport: Option<Transport> = None;
        while !stop.load(Ordering::Relaxed) {
            // The console's trigger effects, between two reads (pad_dualsense.rs).
            if let Some(t) = transport {
                if let Some(report) = pad_dualsense::take_report(t) {
                    if let Err(e) = dev.write(&report) {
                        eprintln!("[pad-touchpad] could not set the triggers: {e}");
                    }
                }
            }
            let n = match dev.read_timeout(&mut buf, 20) {
                Ok(0) => continue,
                Ok(n) => n,
                Err(_) => break,
            };
            let Some(r) = parse_dualsense_report(&buf[..n]) else {
                continue;
            };
            transport = Some(if buf[0] == 0x31 {
                Transport::Bluetooth
            } else {
                Transport::Usb
            });
            if let Some(m) = parse_dualsense_motion(&buf[..n]) {
                let cal = *cal.get_or_insert_with(|| calibration(dev));
                let t = clock.us(m.ticks);
                if motion_at.map_or(true, |at| at.elapsed() >= MOTION_EVERY) {
                    let (gyro, accel) = cal.apply(&m);
                    let _ = app.emit("pad-motion", Some(MotionEvent { gyro, accel, t }));
                    motion_at = Some(Instant::now());
                }
            }
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
        if motion_at.is_some() {
            let _ = app.emit("pad-motion", None::<MotionEvent>);
        }
        if let Some(report) = transport.and_then(pad_dualsense::take_reset) {
            let _ = dev.write(&report);
        }
    }
}

/// Is a DualSense attached (first), and on USB (second)? Desktop only.
pub fn find_dualsense() -> (bool, bool) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        reader::find()
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        (false, false)
    }
}

/// The app is quitting: let the triggers go (desktop).
pub fn pad_touchpad_stop_on_exit() {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    reader::stop_on_exit();
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

    /// A USB report with motion: gyro, accel and the sensor clock at state bytes 15-30.
    fn motion_report(gyro: [i16; 3], accel: [i16; 3], ticks: u32) -> Vec<u8> {
        let mut b = usb(false, UP, UP);
        for i in 0..3 {
            b[1 + 15 + i * 2..1 + 17 + i * 2].copy_from_slice(&gyro[i].to_le_bytes());
            b[1 + 21 + i * 2..1 + 23 + i * 2].copy_from_slice(&accel[i].to_le_bytes());
        }
        b[1 + 27..1 + 31].copy_from_slice(&ticks.to_le_bytes());
        b
    }

    #[test]
    fn motion_is_read_from_usb_and_bluetooth_reports() {
        let b = motion_report([1, -2, 300], [0, 8192, -8192], 0xdead_beef);
        let m = parse_dualsense_motion(&b).unwrap();
        assert_eq!(m.gyro, [1, -2, 300]);
        assert_eq!(m.accel, [0, 8192, -8192]);
        assert_eq!(m.ticks, 0xdead_beef);
        // Bluetooth: the same state one byte later.
        let mut bt = vec![0u8; 78];
        bt[0] = 0x31;
        bt[2..65].copy_from_slice(&b[1..64]);
        assert_eq!(parse_dualsense_motion(&bt), Some(m));
        assert_eq!(parse_dualsense_motion(&[0x01; 10]), None);
    }

    #[test]
    fn uncalibrated_scaling_is_sdls() {
        // SDL without calibration: gyro raw × 64 / 1024 °/s, accel raw / 8192 g.
        let c = Calibration::default();
        let m = parse_dualsense_motion(&motion_report([16, 0, -160], [0, 8192, -4096], 0)).unwrap();
        let (g, a) = c.apply(&m);
        // 16 raw = 1 °/s = 17.45 mrad/s.
        assert_eq!(g, [17, 0, -175]);
        assert_eq!(a, [0, 1000, -500]);
    }

    /// A calibration report as a DualSense sends it (feature 0x05): biases, ±ranges, speeds.
    fn calibration_report() -> Vec<u8> {
        let mut d = vec![0u8; 41];
        d[0] = 0x05;
        let mut put = |at: usize, v: i16| d[at..at + 2].copy_from_slice(&v.to_le_bytes());
        put(1, 2); // pitch bias
        put(3, -3); // yaw bias
        put(5, 0); // roll bias
        for (plus, minus) in [(7, 9), (11, 13), (15, 17)] {
            put(plus, 8640);
            put(minus, -8640);
        }
        put(19, 540); // speed +
        put(21, 540); // speed -
        for (plus, minus) in [(23, 25), (27, 29), (31, 33)] {
            put(plus, 8200);
            put(minus, -8184);
        }
        d
    }

    #[test]
    fn calibration_is_read_and_applied_like_sdl() {
        let c = Calibration::parse(&calibration_report()).expect("plausible");
        // Gyro sensitivity (540 + 540) × 1024 / 17280 = 64: the bias comes off, then as before.
        let m = parse_dualsense_motion(&motion_report([18, -3, 0], [8, 8200, -8184], 0)).unwrap();
        let (g, a) = c.apply(&m);
        assert_eq!(g, [17, 0, 0]);
        // Accel: range 16384 = 2 g, bias 8200 - 8192 = 8.
        assert_eq!(a, [0, 1000, -1000]);
        // A bad calibration (zero range) is ignored.
        let mut bad = calibration_report();
        bad[7..9].copy_from_slice(&0i16.to_le_bytes());
        bad[9..11].copy_from_slice(&0i16.to_le_bytes());
        assert_eq!(Calibration::parse(&bad), None);
        assert_eq!(Calibration::parse(&[0x05; 10]), None);
    }

    #[test]
    fn the_sensor_clock_counts_across_its_wrap() {
        let mut c = SensorClock::default();
        assert_eq!(c.us(u32::MAX - 2), 0);
        // 6 ticks later, across the wrap: 2 µs.
        assert_eq!(c.us(3), 2);
        assert_eq!(c.us(3 + 3000), 1002);
    }
}
