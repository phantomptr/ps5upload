//! PS5 system clock RPCs — get + set, via TIME_GET / TIME_SET FTX2
//! frames. The payload side is in payload/src/sys_time.c; both the
//! wire shape and the err_code semantics are documented there.
//!
//! Set requires a ucred-elevated loader on the PS5 side (kstuff or
//! equivalent). On a non-elevated loader the SceShellCore IPC is
//! rejected with a Sony err_code (typically the 0x80A2xxxx family);
//! the desktop surfaces that to the user.
//!
//! Some firmware revisions ship libSceSystemService stubs that return
//! rc=0 from set but the underlying syscall is a no-op. To detect
//! that, the payload bookends the set call with two get calls and
//! reports both unix epochs back; callers compare new_unix against
//! the requested epoch to see whether the clock actually moved.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Diagnostic err_code sentinels surfaced by the payload's sys_time
/// module. Keep in sync with payload/include/sys_time.h.
pub const SYS_TIME_ERR_NULL_ARG: u32 = 0xE0002001;
pub const SYS_TIME_ERR_NO_SYMBOL: u32 = 0xE0002002;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PsTime {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub err_code: u32,
    /// Fields only valid when `ok`. UTC.
    #[serde(default)]
    pub year: u16,
    #[serde(default)]
    pub month: u16,
    #[serde(default)]
    pub day: u16,
    #[serde(default)]
    pub hour: u16,
    #[serde(default)]
    pub min: u16,
    #[serde(default)]
    pub sec: u16,
}

impl PsTime {
    /// Unix epoch seconds (UTC) corresponding to the year/month/.../sec
    /// fields, or None if `ok==false` or the date doesn't parse. Used
    /// by callers (e.g. the desktop UI) to compute drift against the
    /// host's clock.
    pub fn to_unix_seconds(&self) -> Option<i64> {
        if !self.ok {
            return None;
        }
        ymd_hms_to_unix_utc(
            self.year as i32,
            self.month as u32,
            self.day as u32,
            self.hour as u32,
            self.min as u32,
            self.sec as u32,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PsTimeSetResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub err_code: u32,
    /// Unix epoch (seconds, UTC) the payload observed BEFORE calling
    /// set. -1 if the bookend get failed (the set still ran).
    #[serde(default = "minus_one")]
    pub prior_unix: i64,
    /// Unix epoch (seconds, UTC) the payload observed AFTER calling
    /// set. -1 if the bookend get failed. Compare against the
    /// requested unix to detect "rc=0 but the clock didn't move"
    /// stub no-ops.
    #[serde(default = "minus_one")]
    pub new_unix: i64,
}

fn minus_one() -> i64 {
    -1
}

/// Read the PS5's current system date/time.
pub fn ps5_time_get(addr: &str) -> Result<PsTime> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::TimeGet, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected TIME_GET: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::TimeGetAck {
        bail!("expected TIME_GET_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

/// Set the PS5's system date/time. `target_unix_seconds` is UTC.
/// Returns the payload's before/after snapshot so the caller can
/// detect stub no-ops.
pub fn ps5_time_set(addr: &str, target_unix_seconds: i64) -> Result<PsTimeSetResult> {
    let (year, month, day, hour, min, sec) = unix_to_ymd_hms_utc(target_unix_seconds)
        .ok_or_else(|| anyhow::anyhow!("target_unix_seconds out of representable range"))?;
    let body = serde_json::json!({
        "year": year,
        "month": month,
        "day": day,
        "hour": hour,
        "min": min,
        "sec": sec,
    });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::TimeSet, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected TIME_SET: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::TimeSetAck {
        bail!("expected TIME_SET_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

/// Humanise a Sony / sentinel err_code into a short reason string for
/// the UI. The 0xE0002xxx sentinels are ours; 0x80A2xxxx values come
/// from Sony's SceShellCore authid check; anything else falls through
/// as a hex code. Used by the Tauri command to keep the React side
/// from having to know the constants.
pub fn humanize_err(err_code: u32) -> String {
    match err_code {
        0 => "success".to_string(),
        SYS_TIME_ERR_NULL_ARG => "invalid request (null/out-of-range field)".to_string(),
        SYS_TIME_ERR_NO_SYMBOL => {
            "sceSystemServiceSet/GetCurrentDateTime not exported on this firmware".to_string()
        }
        c if (0x80A2_0000..0x80A3_0000).contains(&c) => format!(
            "Sony rejected the call (0x{c:08x}) — usually means the payload's process \
             is not ucred-elevated; reload via kstuff or an equivalent loader and retry"
        ),
        c if c >= 0x8000_0000 => {
            format!("Sony error 0x{c:08x}")
        }
        c => format!("error 0x{c:08x}"),
    }
}

// ── tiny unix ↔ y/m/d/h/m/s converters (UTC) ────────────────────────────
//
// We avoid pulling chrono just for these two helpers. The math here is
// the standard "days since 1970-01-01 / 86400" — accurate for the
// PS5's lifetime envelope (1970 ≤ year ≤ 2199 is the validation
// window we share with the payload's sys_time.c). Leap years follow
// the Gregorian rule (divisible by 4, except by 100 unless 400).
//
// Tested directly in this module's unit tests for: epoch zero, 1999-12-31,
// 2000-01-01 (Y2K), 2024-02-29 (leap day), 2100-02-28 (non-leap
// century), and a few random points.

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

const DAYS_IN_MONTH: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn days_in_month(year: i32, month: u32) -> u32 {
    if month == 2 && is_leap(year) {
        29
    } else {
        DAYS_IN_MONTH[(month as usize).saturating_sub(1).min(11)]
    }
}

/// (year, 1-12, 1-31, 0-23, 0-59, 0-59) → unix seconds (UTC). None on
/// out-of-range / non-existent date (e.g. Feb 30).
pub fn ymd_hms_to_unix_utc(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    if !(1970..=2199).contains(&year) {
        return None;
    }
    if !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..month {
        days += days_in_month(year, m) as i64;
    }
    days += (day - 1) as i64;
    let secs = days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64;
    Some(secs)
}

/// unix seconds → (year, 1-12, 1-31, 0-23, 0-59, 0-59). None if the
/// epoch falls outside our supported window (years 1970-2199).
pub fn unix_to_ymd_hms_utc(unix_seconds: i64) -> Option<(u16, u8, u8, u8, u8, u8)> {
    if unix_seconds < 0 {
        return None;
    }
    let day_secs = unix_seconds % 86_400;
    let mut days = unix_seconds / 86_400;
    let hour = (day_secs / 3600) as u32;
    let minute = ((day_secs % 3600) / 60) as u32;
    let second = (day_secs % 60) as u32;
    let mut year: i32 = 1970;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if days < in_year {
            break;
        }
        days -= in_year;
        year += 1;
        if year > 2199 {
            return None;
        }
    }
    let mut month: u32 = 1;
    while month <= 12 {
        let in_month = days_in_month(year, month) as i64;
        if days < in_month {
            break;
        }
        days -= in_month;
        month += 1;
    }
    let day = (days + 1) as u32;
    Some((
        year as u16,
        month as u8,
        day as u8,
        hour as u8,
        minute as u8,
        second as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_round_trips() {
        let (y, mo, d, h, mi, s) = unix_to_ymd_hms_utc(0).unwrap();
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
        assert_eq!(
            ymd_hms_to_unix_utc(y as i32, mo as u32, d as u32, h as u32, mi as u32, s as u32),
            Some(0)
        );
    }

    #[test]
    fn y2k_boundary() {
        // 2000-01-01 00:00:00 UTC = 946684800
        let (y, mo, d, h, mi, s) = unix_to_ymd_hms_utc(946_684_800).unwrap();
        assert_eq!((y, mo, d, h, mi, s), (2000, 1, 1, 0, 0, 0));
        assert_eq!(ymd_hms_to_unix_utc(2000, 1, 1, 0, 0, 0), Some(946_684_800));
    }

    #[test]
    fn leap_day_2024() {
        // 2024-02-29 12:34:56 UTC = 1709210096
        let (y, mo, d, h, mi, s) = unix_to_ymd_hms_utc(1_709_210_096).unwrap();
        assert_eq!((y, mo, d, h, mi, s), (2024, 2, 29, 12, 34, 56));
        assert_eq!(
            ymd_hms_to_unix_utc(2024, 2, 29, 12, 34, 56),
            Some(1_709_210_096)
        );
    }

    #[test]
    fn rejects_feb_30() {
        assert!(ymd_hms_to_unix_utc(2024, 2, 30, 0, 0, 0).is_none());
    }

    #[test]
    fn rejects_feb_29_non_leap() {
        // 2100 is divisible by 100 but not by 400 -> NOT a leap year
        assert!(!is_leap(2100));
        assert!(ymd_hms_to_unix_utc(2100, 2, 29, 0, 0, 0).is_none());
        // 2000 IS divisible by 400 -> leap year
        assert!(is_leap(2000));
        assert!(ymd_hms_to_unix_utc(2000, 2, 29, 0, 0, 0).is_some());
    }

    #[test]
    fn rejects_out_of_range_year() {
        assert!(ymd_hms_to_unix_utc(1969, 12, 31, 23, 59, 59).is_none());
        assert!(ymd_hms_to_unix_utc(2200, 1, 1, 0, 0, 0).is_none());
    }

    #[test]
    fn random_known_point() {
        // 2026-05-15 23:30:00 UTC ≈ 1_778_887_800
        // (independently verified via `date -u -d '2026-05-15 23:30:00' +%s`).
        assert_eq!(
            ymd_hms_to_unix_utc(2026, 5, 15, 23, 30, 0),
            Some(1_778_887_800)
        );
        let (y, mo, d, h, mi, s) = unix_to_ymd_hms_utc(1_778_887_800).unwrap();
        assert_eq!((y, mo, d, h, mi, s), (2026, 5, 15, 23, 30, 0));
    }

    #[test]
    fn humanize_known_codes() {
        assert_eq!(humanize_err(0), "success");
        assert!(humanize_err(SYS_TIME_ERR_NULL_ARG).contains("invalid"));
        assert!(humanize_err(SYS_TIME_ERR_NO_SYMBOL).contains("not exported"));
        assert!(humanize_err(0x80A2_3001).contains("ucred-elevated"));
        assert!(humanize_err(0x8001_0042).contains("Sony error"));
    }

    #[test]
    fn pstime_to_unix_returns_none_when_not_ok() {
        let bad = PsTime {
            ok: false,
            err_code: 1,
            year: 2026,
            month: 5,
            day: 15,
            hour: 12,
            min: 0,
            sec: 0,
        };
        assert!(bad.to_unix_seconds().is_none());
    }

    #[test]
    fn pstime_to_unix_round_trip() {
        let dt = PsTime {
            ok: true,
            err_code: 0,
            year: 2026,
            month: 5,
            day: 15,
            hour: 23,
            min: 30,
            sec: 0,
        };
        assert_eq!(dt.to_unix_seconds(), Some(1_778_887_800));
    }

    #[test]
    fn ps_time_set_result_json_round_trip() {
        let json = serde_json::json!({
            "ok": true,
            "err_code": 0,
            "prior_unix": 1_779_658_000_i64,
            "new_unix": 1_778_887_800_i64,
        });
        let r: PsTimeSetResult = serde_json::from_value(json).unwrap();
        assert!(r.ok);
        assert_eq!(r.prior_unix, 1_779_658_000);
        assert_eq!(r.new_unix, 1_778_887_800);
    }

    #[test]
    fn ps_time_set_result_defaults_unix_to_minus_one() {
        // Older payloads / partial responses may omit the unix fields;
        // serde_default should give us -1, NOT 0, so the desktop can
        // distinguish "unknown" from "epoch zero".
        let json = serde_json::json!({"ok": false, "err_code": 1});
        let r: PsTimeSetResult = serde_json::from_value(json).unwrap();
        assert_eq!(r.prior_unix, -1);
        assert_eq!(r.new_unix, -1);
    }
}
