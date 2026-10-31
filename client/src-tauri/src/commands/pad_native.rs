//! Controllers read natively on Linux, for a WebKitGTK without a Gamepad API that sees them.
//!
//! WebKitGTK reports controllers only when it is built with libmanette, and some distributions
//! ship it without (Ubuntu 26.04's libwebkit2gtk-4.1 2.52, for one): there the Gamepad API
//! never sees a controller at all, even one Steam plays with. So the Linux app reads the
//! kernel's input devices itself, with the pure-Rust `evdev` crate, and hands the page each
//! controller in the W3C standard layout as `pad-native` events. The page forwards them into
//! the same bridge the Android app uses (lib/nativePad.ts), which offers them only while the
//! browser sees no controller of its own, so a WebKitGTK that does see one is never read twice.
//!
//! Which devices: an event node with the gamepad buttons (BTN_SOUTH) and a left stick
//! (ABS_X/ABS_Y). A DualSense's motion sensors and touchpad are nodes of their own without
//! BTN_SOUTH, so they are left alone (the touchpad and motion come from pad_touchpad.rs's
//! hidapi reader). Nodes are opened read-only and never grabbed, so Steam, the hidapi reader
//! and everything else keep reading them. Controllers plugged in later are found within two
//! seconds; one unplugged is reported gone at once.
//!
//! The mapping follows the kernel's gamepad spec (Documentation/input/gamepad.rst), which names
//! the face buttons by position, with one exception: the xpad driver (and controllers that copy
//! it) reports Xbox X (left) as BTN_X, the code the spec calls BTN_NORTH, as SDL also knows.

// The reader is Linux-only; elsewhere only the mapping (and its tests) remain.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use serde::Serialize;

/// Linux input event codes (include/uapi/linux/input-event-codes.h).
pub mod code {
    pub const BTN_SOUTH: u16 = 0x130;
    pub const BTN_EAST: u16 = 0x131;
    pub const BTN_NORTH: u16 = 0x133;
    pub const BTN_WEST: u16 = 0x134;
    pub const BTN_TL: u16 = 0x136;
    pub const BTN_TR: u16 = 0x137;
    pub const BTN_TL2: u16 = 0x138;
    pub const BTN_TR2: u16 = 0x139;
    pub const BTN_SELECT: u16 = 0x13a;
    pub const BTN_START: u16 = 0x13b;
    pub const BTN_MODE: u16 = 0x13c;
    pub const BTN_THUMBL: u16 = 0x13d;
    pub const BTN_THUMBR: u16 = 0x13e;
    pub const BTN_DPAD_UP: u16 = 0x220;
    pub const BTN_DPAD_DOWN: u16 = 0x221;
    pub const BTN_DPAD_LEFT: u16 = 0x222;
    pub const BTN_DPAD_RIGHT: u16 = 0x223;

    pub const ABS_X: u16 = 0x00;
    pub const ABS_Y: u16 = 0x01;
    pub const ABS_Z: u16 = 0x02;
    pub const ABS_RX: u16 = 0x03;
    pub const ABS_RY: u16 = 0x04;
    pub const ABS_RZ: u16 = 0x05;
    pub const ABS_HAT0X: u16 = 0x10;
    pub const ABS_HAT0Y: u16 = 0x11;
}
use code::*;

/// The absolute axes the mapping reads: ABS_X up to ABS_HAT0Y.
pub const ABS_READ: usize = ABS_HAT0Y as usize + 1;

/// An absolute axis's range, from the device's absinfo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AbsRange {
    pub min: i32,
    pub max: i32,
    pub flat: i32,
}

/// What a controller has, decided once when it is opened.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    /// The axes it has (by code), with their ranges.
    pub abs: [Option<AbsRange>; ABS_READ],
    /// Xbox X / Y reported as BTN_X (= BTN_NORTH) / BTN_Y (= BTN_WEST), as the xpad driver does.
    pub xpad: bool,
}

/// A controller in the W3C standard layout: 17 buttons (0-1) and 4 axes (-1-1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Standard {
    pub buttons: Vec<f64>,
    pub axes: Vec<f64>,
}

/// Is a device with these keys, axes and properties a game controller? The gamepad buttons and
/// a left stick; never an accelerometer (a controller's motion node).
pub fn is_gamepad(
    has_key: impl Fn(u16) -> bool,
    has_abs: impl Fn(u16) -> bool,
    accelerometer: bool,
) -> bool {
    !accelerometer && has_key(BTN_SOUTH) && has_abs(ABS_X) && has_abs(ABS_Y)
}

/// Does the device report its face buttons the way xpad does (see the module notes)? The xpad
/// and xone drivers, Microsoft's controllers on Bluetooth, and Steam's virtual controller.
pub fn xpad_style(vendor: u16, product: u16, driver: &str) -> bool {
    matches!(driver, "xpad" | "xone-gip-gamepad" | "xone_gip_gamepad")
        || vendor == 0x045e
        || (vendor == 0x28de && product == 0x11ff)
}

/// A stick axis as -1-1, centred, with the device's flat zone (at least 2 %) read as 0.
fn stick(r: AbsRange, v: i32) -> f64 {
    let half = (f64::from(r.max) - f64::from(r.min)) / 2.0;
    if half <= 0.0 {
        return 0.0;
    }
    let centre = (f64::from(r.max) + f64::from(r.min)) / 2.0;
    let x = ((f64::from(v) - centre) / half).clamp(-1.0, 1.0);
    let dead = (f64::from(r.flat) / half).clamp(0.02, 0.25);
    if x.abs() < dead {
        0.0
    } else {
        x
    }
}

/// A trigger axis as 0-1 from its rest at the minimum.
fn trigger(r: AbsRange, v: i32) -> f64 {
    let span = f64::from(r.max) - f64::from(r.min);
    if span <= 0.0 {
        return 0.0;
    }
    ((f64::from(v) - f64::from(r.min)) / span).clamp(0.0, 1.0)
}

/// The controller's state in the W3C standard layout, from the keys held and the axis values.
///
/// The right stick is ABS_RX/ABS_RY, and the analog triggers ABS_Z/ABS_RZ (hid-playstation,
/// xpad); a controller without ABS_RX/ABS_RY that has ABS_Z/ABS_RZ (a generic HID pad) has
/// its right stick there, and digital triggers. The d-pad is the hat or the BTN_DPAD_* keys.
pub fn standard_state(
    layout: &Layout,
    key: impl Fn(u16) -> bool,
    abs: impl Fn(u16) -> i32,
) -> Standard {
    let range = |c: u16| layout.abs[c as usize];
    let held = |c: u16| if key(c) { 1.0 } else { 0.0 };
    let right_on_rx = range(ABS_RX).is_some() && range(ABS_RY).is_some();
    let (rsx, rsy) = if right_on_rx {
        (ABS_RX, ABS_RY)
    } else {
        (ABS_Z, ABS_RZ)
    };
    let analog_trigger = |axis: u16, button: u16| match range(axis) {
        Some(r) if right_on_rx => trigger(r, abs(axis)),
        _ => held(button),
    };
    let hat = |axis: u16| range(axis).map(|_| abs(axis).signum()).unwrap_or(0);
    let (hx, hy) = (hat(ABS_HAT0X), hat(ABS_HAT0Y));
    let dpad = |button: u16, on: bool| if key(button) || on { 1.0 } else { 0.0 };
    let (left, top) = if layout.xpad {
        (BTN_NORTH, BTN_WEST)
    } else {
        (BTN_WEST, BTN_NORTH)
    };
    let buttons = vec![
        held(BTN_SOUTH),
        held(BTN_EAST),
        held(left),
        held(top),
        held(BTN_TL),
        held(BTN_TR),
        analog_trigger(ABS_Z, BTN_TL2),
        analog_trigger(ABS_RZ, BTN_TR2),
        held(BTN_SELECT),
        held(BTN_START),
        held(BTN_THUMBL),
        held(BTN_THUMBR),
        dpad(BTN_DPAD_UP, hy < 0),
        dpad(BTN_DPAD_DOWN, hy > 0),
        dpad(BTN_DPAD_LEFT, hx < 0),
        dpad(BTN_DPAD_RIGHT, hx > 0),
        held(BTN_MODE),
    ];
    let axis = |c: u16| range(c).map(|r| stick(r, abs(c))).unwrap_or(0.0);
    Standard {
        buttons,
        axes: vec![axis(ABS_X), axis(ABS_Y), axis(rsx), axis(rsy)],
    }
}

/// What the page gets: one controller's state, or (`connected` false) that it is gone.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NativePadEvent {
    pub slot: u32,
    pub id: String,
    pub connected: bool,
    pub buttons: Vec<f64>,
    pub axes: Vec<f64>,
}

#[cfg(target_os = "linux")]
pub mod reader {
    use super::*;
    use evdev::{AbsoluteAxisCode, EventType, KeyCode, PropType, RawDevice};
    use std::collections::HashSet;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Changes closer together than this are sent together (the pad reports at 250 Hz or
    /// more; the page reads once a frame).
    const EMIT_EVERY: Duration = Duration::from_millis(4);
    /// How often /dev/input is looked at again for a controller plugged in.
    const RESCAN_EVERY: Duration = Duration::from_secs(2);
    /// The longest the reader sleeps before it looks at its stop flag.
    const TICK: Duration = Duration::from_millis(100);

    /// The running reader's stop flag.
    static RUNNING: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
    /// Raised by the page (loaded or reloaded): send every controller again.
    static RESEND: AtomicBool = AtomicBool::new(false);

    struct Pad {
        path: PathBuf,
        dev: RawDevice,
        slot: u32,
        id: String,
        layout: Layout,
        keys: HashSet<u16>,
        abs: [i32; ABS_READ],
        /// Inside a SYN_DROPPED gap: events up to the next report are stale.
        dropped: bool,
        sent: Option<Standard>,
        last_emit: Option<Instant>,
    }

    impl Pad {
        /// Read the whole state from the kernel (on open, and after events were dropped).
        fn resync(&mut self) {
            if let Ok(keys) = self.dev.get_key_state() {
                self.keys = keys.iter().map(|k| k.code()).collect();
            }
            if let Ok(abs) = self.dev.get_abs_state() {
                for (i, v) in self.abs.iter_mut().enumerate() {
                    *v = abs[i].value;
                }
            }
        }

        fn state(&self) -> Standard {
            standard_state(
                &self.layout,
                |c| self.keys.contains(&c),
                |c| self.abs[c as usize],
            )
        }
    }

    /// Open `path` if it is a game controller. Read-only and never grabbed.
    fn open_pad(path: &Path, slot: u32) -> Option<Pad> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        let dev = RawDevice::from_fd(file.into()).ok()?;
        let keys = dev.supported_keys()?;
        let axes = dev.supported_absolute_axes()?;
        let accel = dev.properties().contains(PropType::ACCELEROMETER);
        if !is_gamepad(
            |c| keys.contains(KeyCode::new(c)),
            |c| axes.contains(AbsoluteAxisCode(c)),
            accel,
        ) {
            return None;
        }
        let info = dev.get_abs_state().ok()?;
        let mut layout = Layout::default();
        for (c, r) in layout.abs.iter_mut().enumerate() {
            if axes.contains(AbsoluteAxisCode(c as u16)) {
                let i = info[c];
                *r = Some(AbsRange {
                    min: i.minimum,
                    max: i.maximum,
                    flat: i.flat,
                });
            }
        }
        let id = dev.input_id();
        layout.xpad = xpad_style(id.vendor(), id.product(), &driver_of(path));
        let name = dev.name().unwrap_or("Controller").to_string();
        let mut pad = Pad {
            path: path.to_owned(),
            dev,
            slot,
            id: name,
            layout,
            keys: HashSet::new(),
            abs: [0; ABS_READ],
            dropped: false,
            sent: None,
            last_emit: None,
        };
        pad.resync();
        Some(pad)
    }

    /// The kernel driver behind an event node ("xpad", "playstation", ...), or "".
    fn driver_of(path: &Path) -> String {
        let Some(node) = path.file_name() else {
            return String::new();
        };
        let link = Path::new("/sys/class/input")
            .join(node)
            .join("device/device/driver");
        std::fs::read_link(link)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }

    /// Open every controller in /dev/input not open yet.
    fn rescan(pads: &mut Vec<Pad>) {
        let Ok(dir) = std::fs::read_dir("/dev/input") else {
            return;
        };
        let mut paths: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("event"))
            })
            .filter(|p| !pads.iter().any(|pad| &pad.path == p))
            .collect();
        paths.sort();
        for path in paths {
            let slot = (0..)
                .find(|s| !pads.iter().any(|p| p.slot == *s))
                .unwrap_or(0);
            if let Some(pad) = open_pad(&path, slot) {
                eprintln!("[pad-native] controller {} at {}", pad.id, path.display());
                pads.push(pad);
            }
        }
    }

    /// Read what a pad has queued. False once it is gone.
    fn drain(pad: &mut Pad) -> bool {
        loop {
            let events = match pad.dev.fetch_events() {
                Ok(events) => events.collect::<Vec<_>>(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            };
            if events.is_empty() {
                return true;
            }
            for ev in events {
                match ev.event_type() {
                    EventType::SYNCHRONIZATION => match ev.code() {
                        // SYN_DROPPED: the queue overflowed; read the state anew at the next report.
                        3 => pad.dropped = true,
                        // SYN_REPORT
                        0 if pad.dropped => {
                            pad.dropped = false;
                            pad.resync();
                        }
                        _ => {}
                    },
                    _ if pad.dropped => {}
                    EventType::KEY => {
                        if ev.value() != 0 {
                            pad.keys.insert(ev.code());
                        } else {
                            pad.keys.remove(&ev.code());
                        }
                    }
                    EventType::ABSOLUTE if (ev.code() as usize) < ABS_READ => {
                        pad.abs[ev.code() as usize] = ev.value();
                    }
                    _ => {}
                }
            }
        }
    }

    /// Read controllers until `stop`, handing every change to `emit`.
    pub fn run(stop: &AtomicBool, mut emit: impl FnMut(&NativePadEvent)) {
        let mut pads: Vec<Pad> = Vec::new();
        let mut next_scan = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            if Instant::now() >= next_scan {
                rescan(&mut pads);
                next_scan = Instant::now() + RESCAN_EVERY;
            }
            if RESEND.swap(false, Ordering::Relaxed) {
                for p in &mut pads {
                    p.sent = None;
                }
            }

            // Send what changed, at most once per EMIT_EVERY a pad; note when to send the rest.
            let now = Instant::now();
            let mut wake = next_scan.min(now + TICK);
            for p in &mut pads {
                let st = p.state();
                if p.sent.as_ref() == Some(&st) {
                    continue;
                }
                match p.last_emit.map(|t| t + EMIT_EVERY) {
                    Some(due) if due > now => wake = wake.min(due),
                    _ => {
                        emit(&NativePadEvent {
                            slot: p.slot,
                            id: p.id.clone(),
                            connected: true,
                            buttons: st.buttons.clone(),
                            axes: st.axes.clone(),
                        });
                        p.sent = Some(st);
                        p.last_emit = Some(now);
                    }
                }
            }

            let mut fds: Vec<libc::pollfd> = pads
                .iter()
                .map(|p| libc::pollfd {
                    fd: p.dev.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            let timeout = wake
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(1000) as libc::c_int;
            if fds.is_empty() {
                std::thread::sleep(Duration::from_millis(timeout as u64));
                continue;
            }
            // SAFETY: `fds` is a valid array of `fds.len()` pollfd structs for the whole call.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if n <= 0 {
                continue;
            }
            let mut gone = Vec::new();
            for (i, fd) in fds.iter().enumerate() {
                let hung = fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
                if (fd.revents & libc::POLLIN != 0 && !drain(&mut pads[i])) || hung {
                    gone.push(i);
                }
            }
            for i in gone.into_iter().rev() {
                let p = pads.remove(i);
                eprintln!("[pad-native] controller {} gone", p.id);
                emit(&NativePadEvent {
                    slot: p.slot,
                    id: p.id,
                    connected: false,
                    buttons: Vec::new(),
                    axes: Vec::new(),
                });
            }
        }
    }

    /// Start reading (once), and send every controller to the page again.
    pub fn start(app: tauri::AppHandle) {
        use tauri::Emitter;
        RESEND.store(true, Ordering::Relaxed);
        let mut cur = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if cur.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        *cur = Some(stop.clone());
        let spawned = std::thread::Builder::new()
            .name("pad-native".into())
            .spawn(move || run(&stop, |ev| drop(app.emit("pad-native", ev))));
        if let Err(e) = spawned {
            eprintln!("[pad-native] could not start: {e}");
            *cur = None;
        }
    }

    /// The app is quitting: stop reading (the reader looks at its flag at least every 100 ms).
    pub fn stop() {
        if let Some(stop) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take() {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// The page asks for the controllers (on load): start the Linux reader and send them all again.
/// False where there is no such reader (the browser's Gamepad API is used as it is).
#[tauri::command]
pub fn pad_native_start(app: tauri::AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    {
        reader::start(app);
        true
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        false
    }
}

/// The app is quitting.
pub fn pad_native_stop_on_exit() {
    #[cfg(target_os = "linux")]
    reader::stop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// hid-playstation's DualSense: sticks and triggers 0-255, the hat for the d-pad.
    fn dualsense() -> Layout {
        let mut l = Layout::default();
        for c in [ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ] {
            l.abs[c as usize] = Some(AbsRange {
                min: 0,
                max: 255,
                flat: 0,
            });
        }
        for c in [ABS_HAT0X, ABS_HAT0Y] {
            l.abs[c as usize] = Some(AbsRange {
                min: -1,
                max: 1,
                flat: 0,
            });
        }
        l
    }

    fn state(l: &Layout, keys: &[u16], abs: &[(u16, i32)]) -> Standard {
        let a: HashMap<u16, i32> = abs.iter().copied().collect();
        let centred = |c: u16| match l.abs[c as usize] {
            Some(r) if !matches!(c, ABS_Z | ABS_RZ) || l.abs[ABS_RX as usize].is_none() => {
                (r.min + r.max + 1) / 2
            }
            Some(r) => r.min,
            None => 0,
        };
        standard_state(
            l,
            |c| keys.contains(&c),
            |c| a.get(&c).copied().unwrap_or_else(|| centred(c)),
        )
    }

    fn pressed(s: &Standard) -> Vec<usize> {
        s.buttons
            .iter()
            .enumerate()
            .filter(|(_, v)| **v > 0.5)
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn a_resting_dualsense_is_all_zero() {
        let s = state(&dualsense(), &[], &[(ABS_HAT0X, 0), (ABS_HAT0Y, 0)]);
        assert_eq!(s.buttons, vec![0.0; 17]);
        assert_eq!(s.axes, vec![0.0; 4]);
    }

    #[test]
    fn dualsense_buttons_land_on_the_standard_layout() {
        let l = dualsense();
        let hat = [(ABS_HAT0X, 0), (ABS_HAT0Y, 0)];
        let one = |k: u16| pressed(&state(&l, &[k], &hat));
        assert_eq!(one(BTN_SOUTH), [0]); // cross
        assert_eq!(one(BTN_EAST), [1]); // circle
        assert_eq!(one(BTN_WEST), [2]); // square
        assert_eq!(one(BTN_NORTH), [3]); // triangle
        assert_eq!(one(BTN_TL), [4]);
        assert_eq!(one(BTN_TR), [5]);
        assert_eq!(one(BTN_SELECT), [8]); // create
        assert_eq!(one(BTN_START), [9]); // options
        assert_eq!(one(BTN_THUMBL), [10]);
        assert_eq!(one(BTN_THUMBR), [11]);
        assert_eq!(one(BTN_MODE), [16]); // PS
                                         // L2/R2 come from the analog axes; their digital bits alone do not press them.
        assert_eq!(one(BTN_TL2), Vec::<usize>::new());
    }

    #[test]
    fn the_hat_is_the_dpad() {
        let l = dualsense();
        assert_eq!(
            pressed(&state(&l, &[], &[(ABS_HAT0X, 0), (ABS_HAT0Y, -1)])),
            [12]
        );
        assert_eq!(
            pressed(&state(&l, &[], &[(ABS_HAT0X, 0), (ABS_HAT0Y, 1)])),
            [13]
        );
        assert_eq!(
            pressed(&state(&l, &[], &[(ABS_HAT0X, -1), (ABS_HAT0Y, 0)])),
            [14]
        );
        assert_eq!(
            pressed(&state(&l, &[], &[(ABS_HAT0X, 1), (ABS_HAT0Y, -1)])),
            [12, 15]
        );
    }

    #[test]
    fn dpad_keys_work_without_a_hat() {
        let l = Layout::default();
        let s = state(&l, &[BTN_DPAD_DOWN, BTN_DPAD_RIGHT], &[]);
        assert_eq!(pressed(&s), [13, 15]);
    }

    #[test]
    fn triggers_are_analog_from_z_and_rz() {
        let l = dualsense();
        let s = state(
            &l,
            &[BTN_TL2],
            &[(ABS_Z, 255), (ABS_RZ, 51), (ABS_HAT0X, 0), (ABS_HAT0Y, 0)],
        );
        assert_eq!(s.buttons[6], 1.0);
        assert!((s.buttons[7] - 0.2).abs() < 1e-9);
    }

    #[test]
    fn sticks_are_normalised_with_y_down() {
        let l = dualsense();
        let s = state(
            &l,
            &[],
            &[
                (ABS_X, 0),
                (ABS_Y, 255),
                (ABS_RX, 128),
                (ABS_RY, 0),
                (ABS_HAT0X, 0),
                (ABS_HAT0Y, 0),
            ],
        );
        assert_eq!(s.axes[0], -1.0);
        assert_eq!(s.axes[1], 1.0);
        assert_eq!(s.axes[2], 0.0); // 128 of 0-255 is the centre, within the dead zone
        assert_eq!(s.axes[3], -1.0);
        // Half way right.
        let s = state(&l, &[], &[(ABS_X, 191), (ABS_HAT0X, 0), (ABS_HAT0Y, 0)]);
        assert!((s.axes[0] - 0.498).abs() < 0.01);
    }

    #[test]
    fn the_flat_zone_reads_as_zero() {
        let mut l = Layout::default();
        let r = AbsRange {
            min: -32768,
            max: 32767,
            flat: 4096,
        };
        l.abs[ABS_X as usize] = Some(r);
        l.abs[ABS_Y as usize] = Some(r);
        let s = standard_state(&l, |_| false, |c| if c == ABS_X { 3000 } else { -8000 });
        assert_eq!(s.axes[0], 0.0);
        assert!(s.axes[1] < -0.2);
    }

    #[test]
    fn xpad_reports_x_as_btn_x() {
        let mut l = Layout {
            xpad: true,
            ..Default::default()
        };
        let r = AbsRange {
            min: -32768,
            max: 32767,
            flat: 128,
        };
        for c in [ABS_X, ABS_Y, ABS_RX, ABS_RY] {
            l.abs[c as usize] = Some(r);
        }
        let t = AbsRange {
            min: 0,
            max: 1023,
            flat: 0,
        };
        l.abs[ABS_Z as usize] = Some(t);
        l.abs[ABS_RZ as usize] = Some(t);
        let s = |k: &[u16]| {
            standard_state(
                &l,
                |c| k.contains(&c),
                |c| if matches!(c, ABS_Z | ABS_RZ) { 0 } else { -1 },
            )
        };
        assert_eq!(pressed(&s(&[BTN_NORTH])), [2]); // BTN_X: Xbox X, on the left
        assert_eq!(pressed(&s(&[BTN_WEST])), [3]); // BTN_Y: Xbox Y, on top
        assert!(xpad_style(0, 0, "xpad"));
        assert!(xpad_style(0x045e, 0x0b13, "microsoft"));
        assert!(!xpad_style(0x054c, 0x0ce6, "playstation"));
    }

    #[test]
    fn a_generic_pad_has_its_right_stick_on_z_and_rz() {
        let mut l = Layout::default();
        let r = AbsRange {
            min: 0,
            max: 255,
            flat: 0,
        };
        for c in [ABS_X, ABS_Y, ABS_Z, ABS_RZ] {
            l.abs[c as usize] = Some(r);
        }
        let s = standard_state(&l, |c| c == BTN_TR2, |c| if c == ABS_Z { 255 } else { 128 });
        assert_eq!(s.axes, vec![0.0, 0.0, 1.0, 0.0]);
        assert_eq!(s.buttons[6], 0.0);
        assert_eq!(s.buttons[7], 1.0); // digital R2
    }

    #[test]
    fn only_gamepads_are_read() {
        let keys = |k: &'static [u16]| move |c: u16| k.contains(&c);
        let abs = |a: &'static [u16]| move |c: u16| a.contains(&c);
        assert!(is_gamepad(keys(&[BTN_SOUTH]), abs(&[ABS_X, ABS_Y]), false));
        // A DualSense's motion node (accelerometer) and touchpad node (no BTN_SOUTH).
        assert!(!is_gamepad(keys(&[]), abs(&[ABS_X, ABS_Y, ABS_Z]), true));
        assert!(!is_gamepad(
            keys(&[0x110, 0x14a]),
            abs(&[ABS_X, ABS_Y]),
            false
        ));
        // Buttons but no stick.
        assert!(!is_gamepad(keys(&[BTN_SOUTH]), abs(&[]), false));
    }

    /// On the Linux machine itself: `cargo test pad_native -- --ignored --nocapture` lists the
    /// controllers found and every change for 10 s (or PAD_NATIVE_SECS).
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "reads the machine's real controllers"]
    fn reads_real_controllers() {
        let secs = std::env::var("PAD_NATIVE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            s2.store(true, Ordering::Relaxed);
        });
        let mut seen = 0;
        reader::run(&stop, |ev| {
            seen += 1;
            println!("{}", serde_json::to_string(ev).unwrap_or_default());
        });
        println!("{seen} event(s)");
    }
}
