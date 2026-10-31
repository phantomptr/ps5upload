//! A DualSense's adaptive triggers while Remote Play plays.
//!
//! A session started in DualSense mode (the page asks for it when `pad_dualsense_info` finds
//! one) gets the console's trigger effects; the page hands them to `pad_dualsense_triggers`,
//! and the HID reader in pad_touchpad.rs, which already has the controller open, writes them
//! as an output report between two reads. When the stream ends, the reader stops, or the app
//! quits, the triggers are set back to off, so the pad never keeps a game's resistance.
//!
//! The report is the one SDL builds (`SDL_hidapi_ps5.c`, `DS5EffectsState_t`) with what
//! chiaki-ng's `Controller::SetTriggerEffects` puts in it: the trigger bits in the first flag
//! byte, 0x40 in the second (the effect-strength byte is valid), each trigger's mode and 10
//! parameters, and the strength the console's settings ask for. USB: report 0x02, 48 bytes.
//! Bluetooth: report 0x31, 78 bytes, `0x02` then the same state, and a CRC-32 over a 0xA2
//! byte (the HID transaction header) and the report, little-endian at the end. On Bluetooth it
//! is written only while the pad already sends its full report (0x31): writing to one in its
//! short-report mode would switch it over, and the system's own controller support (Windows,
//! say) would stop reading its buttons.

// The HID and audio sides are desktop-only; on a phone only the commands' answers remain.
#![cfg_attr(any(target_os = "android", target_os = "ios"), allow(dead_code))]

use std::sync::Mutex;

use serde::Serialize;

/// One trigger's effect: its mode and 10 parameter bytes.
pub type TriggerEffect = (u8, [u8; 10]);

/// chiaki's clear effect: mode 0x05 with no parameters, which leaves a trigger free.
pub const TRIGGER_OFF: TriggerEffect = (0x05, [0; 10]);

/// How the controller is connected, as its input reports show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    /// Bluetooth, sending its full report (0x31).
    Bluetooth,
}

/// The state part of an output report (`DS5EffectsState_t`), 47 bytes.
const STATE_LEN: usize = 47;
const USB_REPORT_LEN: usize = 48;
const BT_REPORT_LEN: usize = 78;

/// CRC-32 (IEEE, as zlib and SDL compute it), continuing from `crc` (0 to start).
pub fn crc32(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// The effect-strength byte (`rgucUnknown1[4]` in SDL's struct): the triggers in the high
/// nibble, rumble and haptics in the low one; 0 is full strength. From the console's settings,
/// 0 off, 1 strong, 2 medium, 3 weak (chiaki streamsession.cpp, HAPTIC_/TRIGGER_INTENSITY).
pub fn strength_byte(haptics: u8, triggers: u8) -> u8 {
    let t = match triggers {
        0 => 0xf0,
        2 => 0x60,
        3 => 0x90,
        _ => 0x00,
    };
    let h = match haptics {
        0 => 0x0f,
        2 => 0x02,
        3 => 0x03,
        _ => 0x00,
    };
    t | h
}

/// The output report that sets both triggers.
pub fn trigger_report(
    transport: Transport,
    left: TriggerEffect,
    right: TriggerEffect,
    strength: u8,
) -> Vec<u8> {
    let mut st = [0u8; STATE_LEN];
    st[0] = 0x04 | 0x08; // right and left trigger effects valid
    st[1] = 0x40; // the strength byte valid
    st[10] = right.0;
    st[11..21].copy_from_slice(&right.1);
    st[21] = left.0;
    st[22..32].copy_from_slice(&left.1);
    st[36] = strength;
    match transport {
        Transport::Usb => {
            let mut r = vec![0u8; USB_REPORT_LEN];
            r[0] = 0x02;
            r[1..1 + STATE_LEN].copy_from_slice(&st);
            r
        }
        Transport::Bluetooth => {
            let mut r = vec![0u8; BT_REPORT_LEN];
            r[0] = 0x31;
            r[1] = 0x02;
            r[2..2 + STATE_LEN].copy_from_slice(&st);
            let crc = crc32(crc32(0, &[0xa2]), &r[..BT_REPORT_LEN - 4]);
            r[BT_REPORT_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
            r
        }
    }
}

/// What the page asked for, waiting for the reader to write it.
#[derive(Debug, Clone, Copy)]
struct Effects {
    triggers: (TriggerEffect, TriggerEffect),
    haptics_strength: u8,
    trigger_strength: u8,
    /// Something changed since the reader last wrote.
    dirty: bool,
    /// The triggers are not off: they must be let go at the end.
    engaged: bool,
}

static EFFECTS: Mutex<Effects> = Mutex::new(Effects {
    triggers: (TRIGGER_OFF, TRIGGER_OFF),
    haptics_strength: 1,
    trigger_strength: 1,
    dirty: false,
    engaged: false,
});

fn effects() -> std::sync::MutexGuard<'static, Effects> {
    EFFECTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The report to write now, if the triggers changed (the reader calls this between reads).
pub fn take_report(transport: Transport) -> Option<Vec<u8>> {
    let mut e = effects();
    if !e.dirty {
        return None;
    }
    e.dirty = false;
    // The console's settings say triggers off: leave them free.
    let (l, r) = if e.trigger_strength == 0 {
        (TRIGGER_OFF, TRIGGER_OFF)
    } else {
        e.triggers
    };
    Some(trigger_report(
        transport,
        l,
        r,
        strength_byte(e.haptics_strength, e.trigger_strength),
    ))
}

/// Forget the session's effects. Returns the report that lets the triggers go when they were
/// set (the reader writes it as it stops); None when there is nothing to undo.
pub fn take_reset(transport: Transport) -> Option<Vec<u8>> {
    let mut e = effects();
    let engaged = e.engaged;
    *e = Effects {
        triggers: (TRIGGER_OFF, TRIGGER_OFF),
        haptics_strength: 1,
        trigger_strength: 1,
        dirty: false,
        engaged: false,
    };
    engaged.then(|| trigger_report(transport, TRIGGER_OFF, TRIGGER_OFF, 0))
}

fn set_triggers(left: TriggerEffect, right: TriggerEffect) {
    let mut e = effects();
    e.triggers = (left, right);
    e.dirty = true;
    e.engaged = true;
}

fn effect(v: &[u8]) -> Option<TriggerEffect> {
    if v.len() != 11 {
        return None;
    }
    Some((v[0], v[1..].try_into().ok()?))
}

/// The console's trigger effects for the DualSense: each `[mode, 10 parameters]`.
#[tauri::command]
pub fn pad_dualsense_triggers(left: Vec<u8>, right: Vec<u8>) -> bool {
    match (effect(&left), effect(&right)) {
        (Some(l), Some(r)) => {
            set_triggers(l, r);
            true
        }
        _ => false,
    }
}

/// The console's effect strengths (0 off, 1 strong, 2 medium, 3 weak).
#[tauri::command]
pub fn pad_dualsense_intensity(haptics: u8, triggers: u8) {
    let mut e = effects();
    e.haptics_strength = haptics;
    e.trigger_strength = triggers;
    e.dirty = true;
    crate::commands::pad_haptics::set_strength(haptics);
}

/// Let the triggers go now (the stream ended); the reader writes it.
#[tauri::command]
pub fn pad_dualsense_reset() {
    let mut e = effects();
    if e.engaged || e.triggers != (TRIGGER_OFF, TRIGGER_OFF) {
        e.triggers = (TRIGGER_OFF, TRIGGER_OFF);
        e.trigger_strength = 1;
        e.dirty = true;
    }
}

/// Whether a DualSense is attached, how, and whether its speaker-side actuators can be played.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct DualSenseInfo {
    pub connected: bool,
    pub usb: bool,
    /// Its USB audio device was found (haptics are played there).
    pub haptics: bool,
}

/// Is a DualSense attached to this computer? Desktop only; elsewhere it is never.
#[tauri::command]
pub async fn pad_dualsense_info() -> DualSenseInfo {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        tauri::async_runtime::spawn_blocking(|| {
            let (connected, usb) = crate::commands::pad_touchpad::find_dualsense();
            DualSenseInfo {
                connected,
                usb,
                haptics: usb && crate::commands::pad_haptics::device_present(),
            }
        })
        .await
        .unwrap_or_default()
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        DualSenseInfo::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
        // Continuing a CRC is the CRC of the two parts together.
        assert_eq!(crc32(crc32(0, b"1234"), b"56789"), 0xcbf4_3926);
    }

    fn sample() -> (TriggerEffect, TriggerEffect) {
        let mut l = [0u8; 10];
        let mut r = [0u8; 10];
        for i in 0..10 {
            l[i] = i as u8 + 1;
            r[i] = 0x10 + i as u8;
        }
        ((0x21, l), (0x26, r))
    }

    #[test]
    fn usb_report_layout() {
        let (l, r) = sample();
        let b = trigger_report(Transport::Usb, l, r, 0x63);
        assert_eq!(b.len(), 48);
        assert_eq!(&b[..3], &[0x02, 0x0c, 0x40]);
        // Right trigger at state byte 10, left at 21 (report byte +1).
        assert_eq!(b[11], 0x26);
        assert_eq!(&b[12..22], &r.1);
        assert_eq!(b[22], 0x21);
        assert_eq!(&b[23..33], &l.1);
        assert_eq!(b[37], 0x63);
        // Rumble, audio and lights untouched.
        assert_eq!(&b[3..11], &[0; 8]);
        assert!(b[38..].iter().all(|&x| x == 0));
    }

    #[test]
    fn bluetooth_report_layout_and_crc() {
        let (l, r) = sample();
        let b = trigger_report(Transport::Bluetooth, l, r, 0);
        assert_eq!(b.len(), 78);
        assert_eq!(&b[..4], &[0x31, 0x02, 0x0c, 0x40]);
        assert_eq!(b[12], 0x26);
        assert_eq!(b[23], 0x21);
        let mut seeded = vec![0xa2];
        seeded.extend_from_slice(&b[..74]);
        assert_eq!(
            u32::from_le_bytes(b[74..].try_into().unwrap()),
            crc32(0, &seeded)
        );
        // The same state as over USB, one byte later.
        let u = trigger_report(Transport::Usb, l, r, 0);
        assert_eq!(&b[2..49], &u[1..48]);
    }

    #[test]
    fn strengths_map_like_chiaki() {
        assert_eq!(strength_byte(1, 1), 0x00);
        assert_eq!(strength_byte(2, 2), 0x62);
        assert_eq!(strength_byte(3, 3), 0x93);
        assert_eq!(strength_byte(0, 0), 0xff);
    }

    #[test]
    fn effects_are_written_once_and_let_go_at_the_end() {
        let (l, r) = sample();
        // A fresh state: nothing to write, nothing to undo.
        let _ = take_reset(Transport::Usb);
        assert!(take_report(Transport::Usb).is_none());
        assert!(take_reset(Transport::Usb).is_none());
        let mut lv = vec![l.0];
        lv.extend_from_slice(&l.1);
        let mut rv = vec![r.0];
        rv.extend_from_slice(&r.1);
        assert!(pad_dualsense_triggers(lv, rv));
        assert!(!pad_dualsense_triggers(vec![1, 2], vec![]));
        let w = take_report(Transport::Usb).expect("written");
        assert_eq!(w[22], 0x21);
        assert!(take_report(Transport::Usb).is_none());
        // The console turns triggers off in its settings: written as free.
        {
            let mut e = effects();
            e.trigger_strength = 0;
            e.dirty = true;
        }
        let w = take_report(Transport::Usb).unwrap();
        assert_eq!((w[11], w[22], w[37]), (0x05, 0x05, 0xf0));
        // The end: the reset lets both go, at full strength.
        let z = take_reset(Transport::Usb).unwrap();
        assert_eq!((z[11], z[22], z[37]), (0x05, 0x05, 0x00));
        assert!(take_reset(Transport::Usb).is_none());
    }
}
