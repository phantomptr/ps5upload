//! The PC's keyboard and mouse as the console's own: the helper's `vinput.*` methods (185-187,
//! `payload/include/vinput.h`, `protocol/ps5upload-ava1/MGMT_METHODS.md`).
//!
//! A [`Report`] is the whole input state at one moment (mouse buttons, keyboard modifiers and up to
//! eight held keys as USB HID usage IDs) plus the mouse movement and wheel since the last one. Its
//! 16-byte form is the same everywhere: the page's Remote Play binary kind 8, and (as hex) the
//! helper's `vinput.report`.
//!
//! The helper owns the devices and lets go of everything on its own when the heartbeat stops
//! ([`TIMEOUT`]); the engine keeps it alive with [`SessionOp::Heartbeat`] every [`HEARTBEAT`] while a
//! page wants input sent. Today's helpers make no devices (their firmware backend is a stub): `caps`
//! answers both false and `open` answers `vinput_unsupported`.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m, MgmtError};

/// One report's bytes.
pub const REPORT_LEN: usize = 16;
/// Keys a report holds at once (besides the modifier byte).
pub const MAX_KEYS: usize = 8;
/// Reports one `vinput.report` call carries (`VINPUT_MAX_REPORTS`).
pub const MAX_REPORTS: usize = 64;
/// How often a heartbeat keeps a session (the page's and the engine's).
pub const HEARTBEAT: Duration = Duration::from_millis(500);
/// A session with no heartbeat this long is let go (`VINPUT_HEARTBEAT_TIMEOUT_MS`).
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// The deadline of one call: input that cannot get through in this long is not worth waiting for.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// Mouse buttons (report byte 0).
pub mod button {
    pub const LEFT: u8 = 0x01;
    pub const RIGHT: u8 = 0x02;
    pub const MIDDLE: u8 = 0x04;
    pub const BACK: u8 = 0x08;
    pub const FORWARD: u8 = 0x10;
}

/// The input state at one moment, and the motion since the last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub buttons: u8,
    /// The HID modifier byte (bit 0 left Ctrl ... bit 7 right GUI).
    pub mods: u8,
    pub dx: i16,
    pub dy: i16,
    pub wheel: i8,
    pub hwheel: i8,
    /// HID keyboard usage IDs held, 0 = none.
    pub keys: [u8; MAX_KEYS],
}

impl Report {
    /// Nothing held and no motion: what "let go of everything" sends.
    pub const RELEASED: Report = Report {
        buttons: 0,
        mods: 0,
        dx: 0,
        dy: 0,
        wheel: 0,
        hwheel: 0,
        keys: [0; MAX_KEYS],
    };

    /// The 16-byte form (little-endian).
    pub fn to_bytes(&self) -> [u8; REPORT_LEN] {
        let mut b = [0u8; REPORT_LEN];
        b[0] = self.buttons;
        b[1] = self.mods;
        b[2..4].copy_from_slice(&self.dx.to_le_bytes());
        b[4..6].copy_from_slice(&self.dy.to_le_bytes());
        b[6] = self.wheel as u8;
        b[7] = self.hwheel as u8;
        b[8..].copy_from_slice(&self.keys);
        b
    }

    /// From exactly [`REPORT_LEN`] bytes.
    pub fn from_bytes(b: &[u8]) -> Option<Report> {
        if b.len() != REPORT_LEN {
            return None;
        }
        let mut keys = [0u8; MAX_KEYS];
        keys.copy_from_slice(&b[8..]);
        Some(Report {
            buttons: b[0],
            mods: b[1],
            dx: i16::from_le_bytes([b[2], b[3]]),
            dy: i16::from_le_bytes([b[4], b[5]]),
            wheel: b[6] as i8,
            hwheel: b[7] as i8,
            keys,
        })
    }

    /// The same buttons, modifiers and keys held (the motion aside).
    pub fn same_held(&self, o: &Report) -> bool {
        self.buttons == o.buttons && self.mods == o.mods && self.keys == o.keys
    }

    /// Anything held at all.
    pub fn holds_anything(&self) -> bool {
        self.buttons != 0 || self.mods != 0 || self.keys.iter().any(|&k| k != 0)
    }

    /// This report's motion added to `self`, when it fits (no clamping: a sum that would not fit
    /// stays a report of its own, so no movement is lost).
    fn absorb(&mut self, o: &Report) -> bool {
        let (Some(dx), Some(dy), Some(w), Some(hw)) = (
            self.dx.checked_add(o.dx),
            self.dy.checked_add(o.dy),
            self.wheel.checked_add(o.wheel),
            self.hwheel.checked_add(o.hwheel),
        ) else {
            return false;
        };
        self.dx = dx;
        self.dy = dy;
        self.wheel = w;
        self.hwheel = hw;
        true
    }
}

/// Merges queued reports without changing what the console sees: neighbours that hold the same
/// buttons and keys become one with their motion added; every change of what is held stays its own
/// report, in order (a quick tap is a press and a release, never lost).
pub fn coalesce(reports: impl IntoIterator<Item = Report>) -> Vec<Report> {
    let mut out: Vec<Report> = Vec::new();
    for r in reports {
        if let Some(last) = out.last_mut() {
            if last.same_held(&r) && last.absorb(&r) {
                continue;
            }
        }
        out.push(r);
    }
    out
}

/// Reports as `vinput.report`'s `r`: lower-case hex of their 16-byte forms.
pub fn to_hex(reports: &[Report]) -> String {
    let mut s = String::with_capacity(reports.len() * REPORT_LEN * 2);
    for r in reports {
        for b in r.to_bytes() {
            s.push_str(&format!("{b:02x}"));
        }
    }
    s
}

/// What the console can make (`vinput.caps`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    #[serde(default)]
    pub mouse: bool,
    #[serde(default)]
    pub keyboard: bool,
    /// A session is open (this engine's or another's).
    #[serde(default)]
    pub open: bool,
    /// The helper's backend: "stub" until the firmware's calls are in.
    #[serde(default)]
    pub backend: String,
    /// Why nothing can be made (`vinput_unsupported`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The session's state after a `vinput.session` call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub mouse: bool,
    #[serde(default)]
    pub keyboard: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

/// The answer to `vinput.report`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportAck {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub applied: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

/// A `vinput.session` operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionOp {
    /// Make (or keep) the devices asked for, remove the others.
    Open {
        mouse: bool,
        keyboard: bool,
    },
    Heartbeat,
    /// Let go of everything and remove the devices.
    Close,
}

impl SessionOp {
    /// The request body, `id` the owner's token.
    pub fn body(&self, id: u64) -> serde_json::Value {
        match *self {
            SessionOp::Open { mouse, keyboard } => {
                serde_json::json!({"op": "open", "id": id, "mouse": mouse, "keyboard": keyboard})
            }
            SessionOp::Heartbeat => serde_json::json!({"op": "hb", "id": id}),
            SessionOp::Close => serde_json::json!({"op": "close", "id": id}),
        }
    }
}

/// True when `e` is the helper saying it has no such method (an older helper).
pub fn is_unsupported(e: &anyhow::Error) -> bool {
    e.downcast_ref::<MgmtError>()
        .is_some_and(|m| m.status == crate::console_files::ERR_UNKNOWN_METHOD)
}

fn supported<T>(r: Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if is_unsupported(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// What the console at `addr` can make; `None` from a helper without the method.
pub fn caps(addr: &str) -> Result<Option<Caps>> {
    supported(
        mgmt::call_with(
            addr,
            m::VINPUT_CAPS,
            m::VINPUT_CAPS.label,
            b"{}",
            Some(CALL_TIMEOUT),
        )
        .and_then(|b| Ok(serde_json::from_slice(&b)?)),
    )
}

/// One `vinput.session` call for owner `id`; `None` from a helper without the method.
pub fn session(addr: &str, op: SessionOp, id: u64) -> Result<Option<SessionState>> {
    let body = serde_json::to_vec(&op.body(id))?;
    supported(
        mgmt::call_with(
            addr,
            m::VINPUT_SESSION,
            m::VINPUT_SESSION.label,
            &body,
            Some(CALL_TIMEOUT),
        )
        .and_then(|b| Ok(serde_json::from_slice(&b)?)),
    )
}

/// Sends `reports` (in calls of at most [`MAX_REPORTS`]) for owner `id`; the last answer, or the
/// first that was not ok. `None` from a helper without the method.
pub fn report(addr: &str, id: u64, reports: &[Report]) -> Result<Option<ReportAck>> {
    let mut last = ReportAck {
        ok: true,
        ..Default::default()
    };
    for chunk in reports.chunks(MAX_REPORTS) {
        let body = serde_json::to_vec(&serde_json::json!({"id": id, "r": to_hex(chunk)}))?;
        let ack: Option<ReportAck> = supported(
            mgmt::call_with(
                addr,
                m::VINPUT_REPORT,
                m::VINPUT_REPORT.label,
                &body,
                Some(CALL_TIMEOUT),
            )
            .and_then(|b| Ok(serde_json::from_slice(&b)?)),
        )?;
        let Some(ack) = ack else { return Ok(None) };
        if !ack.ok {
            return Ok(Some(ack));
        }
        last = ack;
    }
    Ok(Some(last))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(buttons: u8, dx: i16, dy: i16, key: u8) -> Report {
        let mut keys = [0; MAX_KEYS];
        keys[0] = key;
        Report {
            buttons,
            dx,
            dy,
            keys,
            ..Default::default()
        }
    }

    #[test]
    fn the_16_bytes_are_the_layout_the_helper_reads() {
        let r = Report {
            buttons: button::LEFT | button::FORWARD,
            mods: 0x22,
            dx: -300,
            dy: 32767,
            wheel: -1,
            hwheel: 2,
            keys: [0x04, 0xe1, 0, 0, 0, 0, 0, 0],
        };
        let b = r.to_bytes();
        // the same bytes payload/tests/vinput_selftest.c test_decode feeds the C decoder
        assert_eq!(
            to_hex(&[r]),
            "1122d4feff7fff0204e1000000000000",
            "layout drifted from payload/include/vinput.h"
        );
        assert_eq!(Report::from_bytes(&b), Some(r));
        assert_eq!(Report::from_bytes(&b[..15]), None);
        assert!(!Report::RELEASED.holds_anything());
        assert!(r.holds_anything());
    }

    #[test]
    fn coalescing_adds_motion_but_keeps_every_change_of_what_is_held() {
        let q = vec![
            rep(0, 1, 1, 0),
            rep(0, 2, -1, 0),
            rep(1, 0, 0, 0), // press
            rep(0, 0, 0, 0), // release (a quick click survives)
            rep(0, 5, 5, 0),
            rep(0, 0, 0, 4), // key down
            rep(0, 0, 0, 0), // key up
        ];
        let out = coalesce(q);
        assert_eq!(
            out,
            vec![
                rep(0, 3, 0, 0),
                rep(1, 0, 0, 0),
                rep(0, 5, 5, 0),
                rep(0, 0, 0, 4),
                rep(0, 0, 0, 0),
            ]
        );
    }

    #[test]
    fn motion_that_would_overflow_stays_a_report_of_its_own() {
        let out = coalesce(vec![rep(0, 30000, 0, 0), rep(0, 30000, 0, 0)]);
        assert_eq!(out.len(), 2);
        let sum: i32 = out.iter().map(|r| r.dx as i32).sum();
        assert_eq!(sum, 60000);
    }

    #[test]
    fn session_bodies_carry_the_owner_and_the_devices() {
        assert_eq!(
            SessionOp::Open {
                mouse: true,
                keyboard: false
            }
            .body(7),
            serde_json::json!({"op":"open","id":7,"mouse":true,"keyboard":false})
        );
        assert_eq!(
            SessionOp::Heartbeat.body(u64::MAX).to_string(),
            r#"{"id":18446744073709551615,"op":"hb"}"#
        );
        assert_eq!(SessionOp::Close.body(1)["op"], "close");
    }

    /// The helper's answers, spelled as payload/src/vinput.c writes them (the snake_case contract:
    /// a key spelled differently would decode as false and look like "not supported").
    #[test]
    fn the_helpers_answers_decode() {
        let c: Caps = serde_json::from_str(
            r#"{"mouse":false,"keyboard":false,"open":false,"backend":"stub","reason":"vinput_unsupported"}"#,
        )
        .unwrap();
        assert_eq!(c.backend, "stub");
        assert_eq!(c.reason.as_deref(), Some("vinput_unsupported"));
        let s: SessionState = serde_json::from_str(
            r#"{"ok":false,"open":true,"mouse":true,"keyboard":false,"err":"vinput_unsupported"}"#,
        )
        .unwrap();
        assert!(s.open && s.mouse && !s.keyboard && !s.ok);
        let a: ReportAck = serde_json::from_str(r#"{"ok":true,"applied":3}"#).unwrap();
        assert_eq!(a.applied, 3);
    }
}
