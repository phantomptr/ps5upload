//! Hardware monitoring over FTX2.
//!
//! Three RPCs — HW_INFO / GET_TEMPS / GET_POWER_INFO:
//!   - [`hw_info`] returns static info (model, serial, OS, RAM, CPU count).
//!     Cached forever on the payload side since it never changes.
//!   - [`hw_temps`] returns live CPU/SoC temperature, CPU frequency, and
//!     SoC power draw. 1-second payload-side cache, 5-second throttle on
//!     the power API specifically.
//!   - [`hw_power`] returns uptime derived from `kern.boottime`.
//!
//! Wire format is newline-separated `key=value` text (not JSON) to keep
//! the payload parser trivial. This module turns that into typed Rust
//! structs so the engine HTTP surface can return clean JSON to clients.
//!
//! Browser launch ([`app_launch_browser`]) fits here because it shares
//! the Sony-API theme even if it's not strictly monitoring.
use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Static hardware info. `physmem` is in bytes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwInfo {
    pub model: String,
    pub serial: String,
    pub has_wlan_bt: bool,
    pub has_optical_out: bool,
    pub hw_model: String,
    pub hw_machine: String,
    pub os: String,
    pub ncpu: u32,
    pub physmem: u64,
}

/// Live sensor readings. Values of 0 mean "API not available on this
/// firmware" rather than "actually zero" -- there's no thermal state
/// where any of these are legitimately 0 during normal operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwTemps {
    pub cpu_temp: i32,
    pub soc_temp: i32,
    pub cpu_freq_mhz: i64,
    pub soc_clock_mhz: i64,
    pub soc_power_mw: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwPower {
    pub operating_time_sec: u64,
    pub operating_time_hours: u64,
    pub operating_time_minutes: u64,
    pub boot_count: u64,
    pub power_consumption_mw: u32,
}

/// Parse a newline-separated "key=value" body into a lookup closure.
/// Unknown keys are ignored; missing keys default per field. The
/// payload's text format has no escape rules (values are simple
/// identifiers or integers), so a naive split is sufficient.
fn parse_kv<'a>(body: &'a [u8]) -> impl Fn(&str) -> Option<&'a str> {
    let text = std::str::from_utf8(body).unwrap_or("");
    move |key: &str| -> Option<&'a str> {
        for line in text.lines() {
            if let Some(eq) = line.find('=') {
                let (k, v) = line.split_at(eq);
                if k == key {
                    return Some(&v[1..]); // skip the '='
                }
            }
        }
        None
    }
}

fn round_trip(addr: &str, req: FrameType, ack: FrameType, label: &str) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(req, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {label}: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != ack {
        bail!("expected {ack:?}, got {ft:?}");
    }
    Ok(resp)
}

/// Static hardware info. Cached on the payload side, so cheap to call
/// repeatedly -- subsequent calls just return the already-formatted
/// text. The client Hardware tab calls this once at mount time.
pub fn hw_info(addr: &str) -> Result<HwInfo> {
    let body = round_trip(addr, FrameType::HwInfo, FrameType::HwInfoAck, "HW_INFO")?;
    let get = parse_kv(&body);
    Ok(HwInfo {
        model: get("model").unwrap_or("PlayStation 5").to_string(),
        serial: get("serial").unwrap_or("").to_string(),
        has_wlan_bt: get("has_wlan_bt") != Some("0"),
        has_optical_out: get("has_optical_out").is_some_and(|v| v != "0"),
        hw_model: get("hw_model").unwrap_or("").to_string(),
        hw_machine: get("hw_machine").unwrap_or("").to_string(),
        os: get("os").unwrap_or("").to_string(),
        ncpu: get("ncpu").and_then(|v| v.parse().ok()).unwrap_or(0),
        physmem: get("physmem").and_then(|v| v.parse().ok()).unwrap_or(0),
    })
}

/// Live sensor snapshot. Called every 5s from the Hardware tab's
/// auto-refresh timer. The payload's 1s cache + 5s power throttle
/// keeps this safe to poll even at higher rates.
pub fn hw_temps(addr: &str) -> Result<HwTemps> {
    let body = round_trip(addr, FrameType::HwTemps, FrameType::HwTempsAck, "HW_TEMPS")?;
    let get = parse_kv(&body);
    Ok(HwTemps {
        cpu_temp: get("cpu_temp").and_then(|v| v.parse().ok()).unwrap_or(0),
        soc_temp: get("soc_temp").and_then(|v| v.parse().ok()).unwrap_or(0),
        cpu_freq_mhz: get("cpu_freq_mhz")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        soc_clock_mhz: get("soc_clock_mhz")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        soc_power_mw: get("soc_power_mw")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    })
}

pub fn hw_power(addr: &str) -> Result<HwPower> {
    let body = round_trip(addr, FrameType::HwPower, FrameType::HwPowerAck, "HW_POWER")?;
    let get = parse_kv(&body);
    Ok(HwPower {
        operating_time_sec: get("operating_time_sec")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        operating_time_hours: get("operating_time_hours")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        operating_time_minutes: get("operating_time_minutes")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        boot_count: get("boot_count").and_then(|v| v.parse().ok()).unwrap_or(0),
        power_consumption_mw: get("power_consumption_mw")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    })
}

/// Safe bounds for fan-turbo threshold. Mirrored in the payload
/// (payload/include/hw_info.h) — the payload clamps identically, so
/// callers that bypass this function still get a safe outcome. We
/// enforce here mostly for clearer error surfacing.
pub const FAN_THRESHOLD_MIN_C: u8 = 45;
pub const FAN_THRESHOLD_MAX_C: u8 = 80;

/// Set the PS5's fan-turbo threshold in °C. Rings through to the
/// payload's `/dev/icc_fan` ioctl — the canonical fan-threshold
/// mechanism on PS5, confirmed working on firmwares 9.x–11.x.
/// Persists until the PS5 reboots.
///
/// Out-of-range inputs are rejected with a clear error here rather
/// than silently clamped, so the UI can surface "you asked for 30,
/// we'll use 45" explicitly rather than hide the behavior.
pub fn hw_set_fan_threshold(addr: &str, threshold_c: u8) -> Result<()> {
    if !(FAN_THRESHOLD_MIN_C..=FAN_THRESHOLD_MAX_C).contains(&threshold_c) {
        bail!(
            "threshold {threshold_c}°C is outside the safe range \
             {FAN_THRESHOLD_MIN_C}–{FAN_THRESHOLD_MAX_C}°C"
        );
    }
    let body = threshold_c.to_string();
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::HwSetFanThreshold, body.as_bytes())?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected HW_SET_FAN_THRESHOLD: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::HwSetFanThresholdAck {
        bail!("expected HW_SET_FAN_THRESHOLD_ACK, got {ft:?}");
    }
    Ok(())
}

/// Open the PS5's built-in web browser. Handy for self-hosted
/// payload-loader pages or custom browsing on the console.
pub fn app_launch_browser(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::AppLaunchBrowser, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected APP_LAUNCH_BROWSER: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::AppLaunchBrowserAck {
        bail!("expected APP_LAUNCH_BROWSER_ACK, got {:?}", ft);
    }
    Ok(())
}

// ─── Process listing ─────────────────────────────────────────────────────

/// One running process on the PS5. Kept minimal — PID and command name
/// is all the payload can surface cheaply from the allproc walk. Fields
/// are `String` rather than `&str` since this crosses the HTTP boundary
/// and needs to own its data.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcEntry {
    pub pid: i32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcList {
    /// `false` when the payload can tell the walk didn't succeed
    /// (typically `kernel_rw_unavailable`). `procs` may still be
    /// partially populated if the failure happened mid-walk.
    pub ok: bool,
    /// Observed entries. Empty when `ok == false`.
    pub procs: Vec<ProcEntry>,
    /// Payload-reported truncation signal. `true` if the walk was cut
    /// short (cap hit or a follow-pointer failure).
    #[serde(default)]
    pub truncated: bool,
    /// Optional short error code when `ok == false`. UI can map this
    /// to a human string; raw codes match the `err_out` values in
    /// `payload/src/proc_list.c`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Snapshot of running PS5 processes. The payload returns JSON directly
/// so we pass it straight through after a minimal parse. Designed to be
/// cheap to call on a ~2–5 second refresh cadence — no UI surface uses
/// it today (latent capability for a future "processes" view), but the
/// endpoint exists because the injector path wants to pick targets by
/// name, which needs the same walk.
pub fn proc_list(addr: &str) -> Result<ProcList> {
    let body = round_trip(
        addr,
        FrameType::ProcList,
        FrameType::ProcListAck,
        "PROC_LIST",
    )?;
    // The payload emits a very small, flat shape — parse straight into
    // a serde_json::Value first so we can handle both the normal case
    // (with a trailing `{"truncated":true}` sentinel object in the
    // array) and the degenerate empty/error case without a bespoke
    // parser.
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| anyhow::anyhow!("proc_list returned non-JSON: {e}"))?;

    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut truncated = false;
    let mut procs = Vec::new();
    if let Some(arr) = v.get("procs").and_then(|x| x.as_array()) {
        for item in arr {
            // The truncation sentinel is the only entry with a
            // `truncated` key. Everything else is `{pid, name}`.
            if item
                .get("truncated")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
            {
                truncated = true;
                continue;
            }
            let pid = item.get("pid").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                procs.push(ProcEntry { pid, name });
            }
        }
    }
    let error = v
        .get("error")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    Ok(ProcList {
        ok,
        procs,
        truncated,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hw_info_body() {
        let body = b"model=CFI-1215A\nserial=ABC123\nhas_wlan_bt=1\nhas_optical_out=0\nhw_model=CFI-1215A\nhw_machine=amd64\nos=FreeBSD 11.0-RELEASE\nncpu=8\nphysmem=13421772800\n";
        let get = parse_kv(body);
        assert_eq!(get("model"), Some("CFI-1215A"));
        assert_eq!(get("ncpu"), Some("8"));
        assert_eq!(get("physmem"), Some("13421772800"));
    }

    #[test]
    fn parse_temps_with_zero_fallbacks() {
        let body =
            b"cpu_temp=65\nsoc_temp=72\ncpu_freq_mhz=3500\nsoc_clock_mhz=0\nsoc_power_mw=85000\n";
        let get = parse_kv(body);
        assert_eq!(get("cpu_temp"), Some("65"));
        assert_eq!(get("soc_power_mw"), Some("85000"));
    }

    #[test]
    fn parse_kv_ignores_missing_keys() {
        let body = b"cpu_temp=50\n";
        let get = parse_kv(body);
        assert_eq!(get("cpu_temp"), Some("50"));
        assert_eq!(get("unknown_key"), None);
    }

    #[test]
    fn fan_threshold_rejects_out_of_range() {
        // Below the safe floor: running fan at turbo during idle.
        let low = hw_set_fan_threshold("127.0.0.1:0", 30);
        assert!(low.is_err());
        let msg = format!("{}", low.unwrap_err());
        assert!(msg.contains("safe range"), "unexpected: {msg}");

        // Above the safe ceiling: too close to thermal throttle.
        let high = hw_set_fan_threshold("127.0.0.1:0", 95);
        assert!(high.is_err());
    }
}
