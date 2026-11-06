//! Hardware monitoring over AVA1 management.
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
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m, Method};

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
    /// Sony's packed firmware version word, read from kernel data
    /// memory via the SDK's `kernel_get_fw_version()` helper. 0 when
    /// kernel R/W is unavailable on the running payload (no kstuff
    /// loaded, or payload doesn't have the required perms) — in that
    /// case fall back to the kern.version string parse on the
    /// desktop side. Precise enough to distinguish FW points within
    /// a family (e.g. 9.60 vs 9.6010).
    #[serde(default)]
    pub kernel_fw_version: u32,
}

/// Live sensor readings. Values of 0 mean "API not available on this
/// firmware" rather than "actually zero" -- there's no thermal state
/// where any of these are legitimately 0 during normal operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwTemps {
    /// Set when only the basic temperatures could be read: why the full read failed. Everything
    /// beyond the temperatures then reads as unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_error: Option<String>,
    pub cpu_temp: i32,
    pub soc_temp: i32,
    /// M.2 NVMe expansion slot temperature (°C). Channel 2 of the SoC
    /// sensor array. `0` = no M.2 drive installed or sensor unavailable.
    /// Requires >= 20 to be treated as valid (empty slot returns 0).
    #[serde(default)]
    pub m2_temp: i32,
    /// A fixed REFERENCE clock (`sceKernelGetCpuFrequency`, 800 MHz on every console measured),
    /// not what the cores run at; see `cpu_core_mhz` for those.
    pub cpu_freq_mhz: i64,
    /// The GPU (GFX) clock in MHz from the SoC's power controller (`/dev/mp1`), read only when
    /// the clocks were asked for (`hw_temps_with_clocks`). `0` = not read, unsupported, or
    /// outside 100–2500 MHz.
    pub soc_clock_mhz: i64,
    pub soc_power_mw: u32,
    /// Average CPU usage across cores, 0–100 %. `-1` = unavailable (the
    /// API isn't exported on this firmware, or an older payload that
    /// doesn't send the field). Read on demand only.
    #[serde(default = "default_sensor_unavailable")]
    pub cpu_usage_pct: i32,
    /// Current fan duty, 0–100 % (approximate — the raw scale is
    /// firmware-dependent). `-1` = unavailable. Read on demand only.
    #[serde(default = "default_sensor_unavailable")]
    pub fan_duty_pct: i32,
    /// Raw Sony "basic product shape" code (distinguishes hardware
    /// families). `-1` = unavailable. The desktop maps known codes to a
    /// label and otherwise shows the number; the model string remains the
    /// primary identifier. Read on demand only.
    #[serde(default = "default_sensor_unavailable")]
    pub product_shape: i32,
    /// User-pinned fan threshold (°C). `0` = no custom threshold set
    /// (fan runs at firmware default). A positive value means the user
    /// has set a permanent fan speed that the helper puts back within
    /// about 2 s whenever the system resets it (a game starting or closing).
    /// Always available (even on basic read) since it's an in-process
    /// value, not a sensor syscall.
    #[serde(default)]
    pub fan_pinned_c: i32,
    /// The temperature the fan controller is working to right now, read back from the
    /// console (°C). `-1` = not reported (an older helper) or unreadable.
    #[serde(default = "default_sensor_unavailable")]
    pub fan_target_c: i32,
    /// The console's own value: what the controller held before ps5upload first changed
    /// it (°C). `0` = never seen. 91 on FW 13.60, not the 60 this app long assumed.
    #[serde(default)]
    pub fan_stock_c: i32,
    /// How many times this helper session put the pinned target back after the system reset
    /// it. `0` also for an older helper.
    #[serde(default)]
    pub fan_restores: u32,
    /// Seconds since the last of those restores; `-1` = never (or an older helper).
    #[serde(default = "default_sensor_unavailable")]
    pub fan_last_restore_s: i32,
    /// The guard that keeps the pinned target: `"off"` (nothing pinned), `"ok"`, or
    /// `"failing"` (its writes kept failing, so it stopped and the console's own control is in
    /// charge until the next set). Empty from an older helper.
    #[serde(default)]
    pub fan_guard: String,
    /// The fan controller's own filtered temperature (°C, one decimal). `None` when unreadable,
    /// out of 20–110 °C, or from an older helper.
    #[serde(default)]
    pub fan_filtered_c: Option<f32>,
    /// Where the clocks came from: `"mp1"` (read; while the driver is busy, the last values or
    /// zeros), `"unsupported"` (the driver refused; the helper does not try again until it
    /// restarts), or `""` (not asked for, or an older helper).
    #[serde(default)]
    pub gpu_clock_src: String,
    /// The power controller's read has not come back (seen for as long as a game runs), so
    /// `gpu_clock_src` and the core clocks say nothing. `false` from an older helper.
    #[serde(default)]
    pub mp1_busy: bool,
    /// The eight CPU cores' clocks in MHz: the power controller's figure for each core's
    /// P-state, not a cycle count. `0` = unknown or outside 400–4000 MHz. Empty when the clocks
    /// were not read.
    #[serde(default)]
    pub cpu_core_mhz: Vec<u32>,
    /// Lowest and highest of the known `cpu_core_mhz` values; `0` when none is known.
    #[serde(default)]
    pub cpu_core_mhz_min: u32,
    #[serde(default)]
    pub cpu_core_mhz_max: u32,
    /// How the measured clocks came: `"add_chain"` (timed on the console, this read or an
    /// earlier one), `"unsupported"` (no CPU took the measuring thread and nothing was measured
    /// earlier; the next read tries again), or `""` (not asked for, or an older helper).
    #[serde(default)]
    pub cpu_measure_src: String,
    /// The measuring read has not come back, so `cpu_measure_src` and the measured clocks say
    /// nothing. `false` from an older helper.
    #[serde(default)]
    pub measure_busy: bool,
    /// The clock each of the eight cores really ran at while busy, in MHz, measured with a
    /// timed chain of adds; indexed like `cpu_core_mhz`. `0` = not measured (the game cores
    /// are not open to the helper) or outside 400–5000 MHz.
    #[serde(default)]
    pub cpu_core_measured_mhz: Vec<u32>,
    /// How the last read of the CPU temperature went: Sony's return code (`"0x00000000"` is
    /// success), `"fault"` (the call crashed and was recovered), `"missing"` (no such call on
    /// this firmware), `"range"` (it answered with an impossible value) or `"none"` (no read has
    /// finished yet). `None` from an older helper. Says why `cpu_temp` is 0.
    #[serde(default)]
    pub cpu_temp_rc: Option<String>,
    /// The same for the SoC temperature.
    #[serde(default)]
    pub soc_temp_rc: Option<String>,
    /// How long the last read of the basic sensors took, in ms; with `stale`, how long the read
    /// still running has taken so far. `None` from an older helper.
    #[serde(default)]
    pub read_ms: Option<u32>,
    /// Another request's read was still running, so these are the last values it had (0 if it
    /// had none): the helper answers at once rather than queueing behind a stalled sensor call.
    #[serde(default)]
    pub stale: bool,
}

fn default_sensor_unavailable() -> i32 {
    -1
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwPower {
    pub operating_time_sec: u64,
    pub operating_time_hours: u64,
    pub operating_time_minutes: u64,
    /// When the console booted, in seconds since the epoch on the console's own clock (to
    /// compare with file times there); 0 from a helper that doesn't say.
    #[serde(default)]
    pub boot_time_sec: u64,
    pub boot_count: u64,
    pub power_consumption_mw: u32,
    /// System load average (1, 5, 15 minute windows) via the SDK's
    /// `getloadavg()` — added to prospero-libc in the 2026-05
    /// ps5-payload-dev/sdk update. Stored as fractional values
    /// (e.g. 0.42 = 42% of one core average); the payload sends
    /// integer centi-units on the wire and the engine divides by
    /// 100 here so consumers get a normal `f32`. Negative value
    /// means the getloadavg call failed on this firmware (the
    /// payload encodes that as -1.00).
    #[serde(default = "default_loadavg_unavailable")]
    pub load_avg_1m: f32,
    #[serde(default = "default_loadavg_unavailable")]
    pub load_avg_5m: f32,
    #[serde(default = "default_loadavg_unavailable")]
    pub load_avg_15m: f32,
}

fn default_loadavg_unavailable() -> f32 {
    -1.0
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

fn round_trip(addr: &str, method: Method) -> Result<Vec<u8>> {
    mgmt::call(addr, method, &[])
}

/// Like [`round_trip`] but sends a request body. Used by HW_TEMPS to pass
/// the `extended` selector ("1" = read the on-demand-only telemetry).
fn round_trip_body(addr: &str, method: Method, body: &[u8]) -> Result<Vec<u8>> {
    mgmt::call(addr, method, body)
}

/// Static hardware info. Cached on the payload side, so cheap to call
/// repeatedly -- subsequent calls just return the already-formatted
/// text. The client Hardware tab calls this once at mount time.
pub fn hw_info(addr: &str) -> Result<HwInfo> {
    let body = round_trip(addr, m::HW_INFO)?;
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
        kernel_fw_version: get("kernel_fw_version")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    })
}

/// Physical sanity bounds for live sensor values, mirrored from the
/// payload (`payload/src/hw_info.c`: `HW_TEMP_MIN_C` / `HW_TEMP_MAX_C` /
/// `HW_CPU_FREQ_MAX_MHZ`, plus the `< 500000` power clamp).
///
/// The current payload already drops out-of-range reads, but we
/// re-validate here as defense-in-depth: a console still running an
/// OLDER payload can emit garbage — e.g. `cpu_temp=26747` (a raw FreeBSD
/// thermal-zone value the old sysctl fallback mis-converted) or a
/// `cpu_freq_mhz` near 5.2e13 (an unbounded direct read on FW 5.10) —
/// and the UI must never render those. Out-of-range ⇒ 0 = "unavailable",
/// matching the payload's contract that 0 means "no usable reading".
pub const SENSOR_TEMP_MIN_C: i32 = 1;
pub const SENSOR_TEMP_MAX_C: i32 = 150;
pub const SENSOR_CPU_FREQ_MAX_MHZ: i64 = 6000;
pub const SENSOR_POWER_MAX_MW: u32 = 500_000;
/// GPU clock bounds (MHz), mirrored from `payload/include/mp1_clock.h`.
pub const SENSOR_GPU_MIN_MHZ: i64 = 100;
pub const SENSOR_GPU_MAX_MHZ: i64 = 2500;
/// Per-core CPU clock bounds (MHz), mirrored from `payload/include/mp1_clock.h`.
pub const SENSOR_CORE_MIN_MHZ: u32 = 400;
pub const SENSOR_CORE_MAX_MHZ: u32 = 4000;
/// Upper bound of a measured core clock (the helper keeps 400–5000 MHz too).
pub const SENSOR_MEASURED_MAX_MHZ: u32 = 5000;
/// The PS5 has eight CPU cores; the helper reports exactly that many.
pub const CPU_CORES: usize = 8;

/// Clamp each sensor field to its physical range, zeroing anything
/// implausible. Pure — unit-tested below.
fn sanitize_temps(mut t: HwTemps) -> HwTemps {
    if !(SENSOR_TEMP_MIN_C..=SENSOR_TEMP_MAX_C).contains(&t.cpu_temp) {
        t.cpu_temp = 0;
    }
    if !(SENSOR_TEMP_MIN_C..=SENSOR_TEMP_MAX_C).contains(&t.soc_temp) {
        t.soc_temp = 0;
    }
    // M.2 temp uses same physical range; 0 means "no drive installed" and
    // is a valid value (don't treat it as implausible).
    if !(0..=SENSOR_TEMP_MAX_C).contains(&t.m2_temp) {
        t.m2_temp = 0;
    }
    if !(0..=SENSOR_CPU_FREQ_MAX_MHZ).contains(&t.cpu_freq_mhz) {
        t.cpu_freq_mhz = 0;
    }
    if !(SENSOR_GPU_MIN_MHZ..=SENSOR_GPU_MAX_MHZ).contains(&t.soc_clock_mhz) {
        t.soc_clock_mhz = 0;
    }
    // Per-core clocks: at most eight, each in range or 0; min/max come from what survives, so
    // they can never disagree with the list.
    t.cpu_core_mhz.truncate(CPU_CORES);
    for v in t.cpu_core_mhz.iter_mut() {
        if !(SENSOR_CORE_MIN_MHZ..=SENSOR_CORE_MAX_MHZ).contains(v) {
            *v = 0;
        }
    }
    t.cpu_core_measured_mhz.truncate(CPU_CORES);
    for v in t.cpu_core_measured_mhz.iter_mut() {
        if !(SENSOR_CORE_MIN_MHZ..=SENSOR_MEASURED_MAX_MHZ).contains(v) {
            *v = 0;
        }
    }
    let known = t.cpu_core_mhz.iter().copied().filter(|v| *v != 0);
    t.cpu_core_mhz_min = known.clone().min().unwrap_or(0);
    t.cpu_core_mhz_max = known.max().unwrap_or(0);
    if t.soc_power_mw > SENSOR_POWER_MAX_MW {
        t.soc_power_mw = 0;
    }
    // 0–100 % are the only valid percentages; anything else (incl. the -1
    // "unavailable" sentinel) is normalised to -1 so the UI shows a dash
    // rather than a bogus bar. product_shape is a raw code with no range to
    // clamp — pass it through untouched (-1 already means unavailable).
    if !(0..=100).contains(&t.cpu_usage_pct) {
        t.cpu_usage_pct = -1;
    }
    if !(0..=100).contains(&t.fan_duty_pct) {
        t.fan_duty_pct = -1;
    }
    // -1 is "never"; anything else below 0 is not a time.
    if t.fan_last_restore_s < -1 {
        t.fan_last_restore_s = -1;
    }
    t
}

/// Live sensor snapshot. Auto-polled every 5s by the Dashboard (and read
/// on demand from the Hardware tab). The payload reads CPU/SoC temp + CPU
/// clock via direct Sony APIs only (no ptrace, no auto-fired power read —
/// `sceKernelGetSocPowerConsumption` hangs the payload on FW 9.60, so
/// `soc_power_mw` is intentionally 0); see `payload/src/hw_info.c`. Every
/// value is re-validated through [`sanitize_temps`] so a stale payload
/// can't surface a nonsense reading.
/// `extended` selects which sensors the payload reads:
///   - `false` (BASIC): CPU/SoC temp + CPU clock only — the auto-poll-safe
///     calls the Dashboard's 5 s tick uses.
///   - `true` (EXTENDED): additionally read CPU usage, fan duty and product
///     shape (`"ufs"`). Requested only behind an explicit user action
///     ("Read sensors"), never on a timer.
///
/// SoC power is DELIBERATELY EXCLUDED. Hardware probe (2026-06): of the
/// four extended getters, `sceKernelGetSocPowerConsumption` is the ONLY one
/// that wedges the helper on FW 9.60 (Pro) — and it does so even with the
/// corrected ABI, so the hang is the API's, not ours; on the phat (5.10) it
/// returns a useless 0. Dangerous and worthless, so we never request it
/// ("p" is omitted from the selector). The payload still supports the `p`
/// flag for a future firmware probe, but no shipping path sends it.
pub fn hw_temps(addr: &str, extended: bool) -> Result<HwTemps> {
    hw_temps_with_clocks(addr, extended, false)
}

/// [`hw_temps`], plus — with `clocks` — the GPU clock and the eight CPU core clocks from the
/// SoC's power controller (`/dev/mp1`, the `c` selector). The helper reads that driver
/// read-only, caches it 2 s and stops for good after one refusal, but it is new and verified
/// only on FW 13.20/13.60 by other projects, so `clocks` is for the Hardware screen's explicit
/// "Read sensors" click only: never a timer, never the bug-report snapshot.
pub fn hw_temps_with_clocks(addr: &str, extended: bool, clocks: bool) -> Result<HwTemps> {
    let body = round_trip_body(addr, m::HW_TEMPS, hw_temps_selector(extended, clocks))?;
    Ok(sanitize_temps(parse_hw_temps(&body)))
}

/// The basic temperatures alone, with its own deadline: what is left when the full read fails.
pub fn hw_temps_basic_within(addr: &str, timeout: std::time::Duration) -> Result<HwTemps> {
    let body = mgmt::call_with(
        addr,
        m::HW_TEMPS,
        m::HW_TEMPS.label,
        hw_temps_selector(false, false),
        Some(timeout),
    )?;
    Ok(sanitize_temps(parse_hw_temps(&body)))
}

/// The HW_TEMPS request body. "ufs" = usage + fan + shape (NOT power — see [`hw_temps`]); each
/// is independently gated payload-side so this set is the verified-safe one. "c" adds the
/// clocks; an older helper ignores the letter it does not know.
fn hw_temps_selector(extended: bool, clocks: bool) -> &'static [u8] {
    match (extended, clocks) {
        (false, false) => b"",
        (true, false) => b"ufs",
        (false, true) => b"c",
        (true, true) => b"ufsc",
    }
}

/// `cpu_core_mhz=a,b,...`: a token that is not a number reads as 0 (unknown), never as a
/// reason to drop the others. Bounds are applied by [`sanitize_temps`].
fn parse_core_list(v: &str) -> Vec<u32> {
    v.split(',')
        .take(CPU_CORES)
        .map(|t| t.trim().parse::<u32>().unwrap_or(0))
        .collect()
}

/// Pure parse of the `key=value` HW_TEMPS body into the typed struct,
/// split out from [`hw_temps`] so the wire-format decode is unit-testable
/// without a live payload. Bounds are applied separately by
/// [`sanitize_temps`].
fn parse_hw_temps(body: &[u8]) -> HwTemps {
    let get = parse_kv(body);
    HwTemps {
        extended_error: None,
        cpu_temp: get("cpu_temp").and_then(|v| v.parse().ok()).unwrap_or(0),
        soc_temp: get("soc_temp").and_then(|v| v.parse().ok()).unwrap_or(0),
        m2_temp: get("m2_temp").and_then(|v| v.parse().ok()).unwrap_or(0),
        cpu_freq_mhz: get("cpu_freq_mhz")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        soc_clock_mhz: get("soc_clock_mhz")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        soc_power_mw: get("soc_power_mw")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        // Missing (older payload) ⇒ -1 = "unavailable", NOT 0 — 0% usage /
        // 0% duty are legitimate live readings and must stay distinct from
        // "this payload doesn't report it".
        cpu_usage_pct: get("cpu_usage_pct")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        fan_duty_pct: get("fan_duty_pct")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        product_shape: get("product_shape")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        fan_pinned_c: get("fan_pinned_c")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        fan_target_c: get("fan_target_c")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        fan_stock_c: get("fan_stock_c").and_then(|v| v.parse().ok()).unwrap_or(0),
        fan_restores: get("fan_restores")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        fan_last_restore_s: get("fan_last_restore_s")
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1),
        fan_guard: get("fan_guard")
            .filter(|v| matches!(*v, "off" | "ok" | "failing"))
            .unwrap_or("")
            .to_string(),
        fan_filtered_c: get("fan_filtered_c")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|c| (20.0..=110.0).contains(c)), // the helper's own sanity range
        gpu_clock_src: get("gpu_clock_src")
            .filter(|v| matches!(*v, "mp1" | "unsupported"))
            .unwrap_or("")
            .to_string(),
        mp1_busy: get("mp1_busy") == Some("1"),
        cpu_core_mhz: get("cpu_core_mhz").map(parse_core_list).unwrap_or_default(),
        cpu_core_mhz_min: get("cpu_core_mhz_min")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        cpu_core_mhz_max: get("cpu_core_mhz_max")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        cpu_measure_src: get("cpu_measure_src")
            .filter(|v| matches!(*v, "add_chain" | "unsupported"))
            .unwrap_or("")
            .to_string(),
        measure_busy: get("measure_busy") == Some("1"),
        cpu_core_measured_mhz: get("cpu_core_measured_mhz")
            .map(parse_core_list)
            .unwrap_or_default(),
        cpu_temp_rc: get("cpu_temp_rc").and_then(sensor_rc_token),
        soc_temp_rc: get("soc_temp_rc").and_then(sensor_rc_token),
        read_ms: get("read_ms").and_then(|v| v.parse().ok()),
        stale: get("stale") == Some("1"),
    }
}

/// A `*_temp_rc` value: a short token (`0x…`, `fault`, `missing`, `range`, `none`). A helper this
/// engine has not met may add others; anything that does not look like one reads as absent.
fn sensor_rc_token(v: &str) -> Option<String> {
    (!v.is_empty() && v.len() <= 16 && v.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| v.to_string())
}

pub fn hw_power(addr: &str) -> Result<HwPower> {
    let body = round_trip(addr, m::HW_POWER)?;
    let get = parse_kv(&body);
    /* Wire format is integer centi-units (× 100). Divide here so
     * consumers downstream see a normal fractional value. Missing
     * key OR parse failure → -1.0 (the "unavailable" sentinel that
     * matches what the payload emits when getloadavg itself fails);
     * keeps "old payload doesn't send this field" and "new payload
     * couldn't read it" indistinguishable to the UI, which is the
     * right behavior — both render the same "unavailable" chip. */
    let la = |k: &str| -> f32 {
        get(k)
            .and_then(|v| v.parse::<i32>().ok())
            .map(|c| c as f32 / 100.0)
            .unwrap_or(-1.0)
    };
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
        boot_time_sec: get("boot_time_sec")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        boot_count: get("boot_count").and_then(|v| v.parse().ok()).unwrap_or(0),
        power_consumption_mw: get("power_consumption_mw")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        load_avg_1m: la("load_avg_1m_centi"),
        load_avg_5m: la("load_avg_5m_centi"),
        load_avg_15m: la("load_avg_15m_centi"),
    })
}

/// "Console Storage" aggregate that matches what PS5 Settings shows.
/// Sums the three partitions Sony's UI counts (`/user effective +
/// /system_data + /system_ex`) and accounts for the reserved-pool slice
/// the FS keeps for root. Per-partition fields are also surfaced for
/// the diagnostics card. Distinct from `fs_list_volumes`: that returns
/// per-volume detail; this is the single-line summary.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HwStorage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub user_total_bytes: u64,
    pub user_free_bytes: u64,
    pub user_reserved_bytes: u64,
    pub system_data_total_bytes: u64,
    pub system_data_free_bytes: u64,
    pub system_ex_total_bytes: u64,
    pub system_ex_free_bytes: u64,
}

pub fn hw_storage(addr: &str) -> Result<HwStorage> {
    let body = round_trip(addr, m::HW_STORAGE)?;
    let get = parse_kv(&body);
    let n = |k: &str| get(k).and_then(|v| v.parse().ok()).unwrap_or(0u64);
    Ok(HwStorage {
        total_bytes: n("total_bytes"),
        free_bytes: n("free_bytes"),
        used_bytes: n("used_bytes"),
        reserved_bytes: n("reserved_bytes"),
        user_total_bytes: n("user_total_bytes"),
        user_free_bytes: n("user_free_bytes"),
        user_reserved_bytes: n("user_reserved_bytes"),
        system_data_total_bytes: n("system_data_total_bytes"),
        system_data_free_bytes: n("system_data_free_bytes"),
        system_ex_total_bytes: n("system_ex_total_bytes"),
        system_ex_free_bytes: n("system_ex_free_bytes"),
    })
}

/// Safe bounds for fan-turbo threshold. Mirrored in the payload
/// (payload/include/hw_info.h) — the payload clamps identically, so
/// callers that bypass this function still get a safe outcome. We
/// enforce here mostly for clearer error surfacing.
pub const FAN_THRESHOLD_MIN_C: u8 = 45;
pub const FAN_THRESHOLD_MAX_C: u8 = 85;
/// The "threshold" that means "restore the console's own fan setting".
pub const FAN_RESTORE_CONSOLE: u8 = 0;

/// Set the PS5's fan-turbo threshold in °C. Rings through to the
/// payload's `/dev/icc_fan` ioctl — the canonical fan-threshold
/// mechanism on PS5, confirmed working on firmwares 9.x–11.x.
/// Persists until the PS5 reboots.
///
/// Out-of-range inputs are rejected with a clear error here rather
/// than silently clamped, so the UI can surface "you asked for 30,
/// we'll use 45" explicitly rather than hide the behavior.
pub fn hw_set_fan_threshold(addr: &str, threshold_c: u8) -> Result<()> {
    hw_set_fan_threshold_ex(addr, threshold_c, None)
}

/// Extended variant that optionally sets the reapply interval alongside
/// the threshold. When `reapply_sec` is `Some(n)`, the body is
/// `"threshold reapply_sec"` — the payload parses both and updates the
/// watcher's interval atomically. `None` sends just `"threshold"`,
/// preserving backward compatibility with older payloads.
pub fn hw_set_fan_threshold_ex(
    addr: &str,
    threshold_c: u8,
    reapply_sec: Option<u32>,
) -> Result<()> {
    // 0 is not a temperature: it asks the helper to give the fan back to the console (its own
    // value restored, ours no longer re-applied).
    if threshold_c != FAN_RESTORE_CONSOLE
        && !(FAN_THRESHOLD_MIN_C..=FAN_THRESHOLD_MAX_C).contains(&threshold_c)
    {
        bail!(
            "threshold {threshold_c}°C is outside the safe range \
             {FAN_THRESHOLD_MIN_C}–{FAN_THRESHOLD_MAX_C}°C"
        );
    }
    let body = match reapply_sec {
        Some(interval) => format!("{threshold_c} {interval}"),
        None => threshold_c.to_string(),
    };
    mgmt::call(addr, m::HW_FAN_THRESHOLD, body.as_bytes())?;
    Ok(())
}

/// Open the PS5's built-in web browser. Handy for self-hosted
/// payload-loader pages or custom browsing on the console.
pub fn app_launch_browser(addr: &str) -> Result<()> {
    mgmt::call(addr, m::APP_LAUNCH_BROWSER, &[])?;
    Ok(())
}

// ─── Drive SMART / temperature sensors ─────────────────────────────────────

/// One physical disk discovered on `/dev/daN`. Temp and usage fields are
/// optional because not all drives support LOG SENSE and not all are
/// mounted. `access_denied` is surfaced when the device node exists but
/// the payload can't open it (rare, only seen on very locked-down FW).
// The `alias` attributes accept the camelCase spellings that
// payload/src/drive_sensors.c emitted before v5.1.0. Every field except
// `device` and `ident` was named differently there, so serde silently
// defaulted them: real hardware reported a 2 TB SSD as 0 bytes with no
// temperature and no filesystem usage, and nothing failed loudly. The
// payload now emits snake_case like every other frame; these aliases keep
// a console still running an older payload working instead of showing
// zeros. Serialization is unaffected — the client keeps receiving
// snake_case.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DriveSensor {
    pub device: String,
    #[serde(default, alias = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ident: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "tempC")]
    pub temp_c: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "tempErr")]
    pub temp_err: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "fsTotalBytes")]
    pub fs_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "fsUsedBytes")]
    pub fs_used_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "fsFreeBytes")]
    pub fs_free_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "mountPoint")]
    pub mount_point: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        alias = "accessDenied"
    )]
    pub access_denied: bool,
}

/// A fixed-storage partition entry (internal SSD, M.2) returned alongside
/// the hot-pluggable drives. No temperature here — the internal SSD temp
/// is already surfaced via the SoC sensor channels.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FixedStorageEntry {
    pub label: String,
    // See DriveSensor above for why these aliases exist.
    #[serde(default, alias = "fsTotalBytes")]
    pub fs_total_bytes: u64,
    #[serde(default, alias = "fsUsedBytes")]
    pub fs_used_bytes: u64,
    #[serde(default, alias = "fsFreeBytes")]
    pub fs_free_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DriveSensorList {
    pub drives: Vec<DriveSensor>,
    pub storage: Vec<FixedStorageEntry>,
}

/// Enumerate all `/dev/daN` disks and read SMART temp, capacity, ident,
/// and filesystem usage. Also returns fixed-storage summaries (internal
/// SSD + M.2) for a complete storage-health picture.
///
/// Read-only — no elevation needed. The payload opens each device node
/// with `O_RDONLY | O_NONBLOCK`, confirms it's a real block device, then
/// issues a SCSI LOG SENSE (page 0x0D) via CAM pass-through.
pub fn drive_sensors(addr: &str) -> Result<DriveSensorList> {
    let body = round_trip(addr, m::HW_DRIVE_SENSORS)?;
    let parsed: DriveSensorList = serde_json::from_slice(&body)
        .map_err(|e| anyhow::anyhow!("drive_sensors returned non-JSON: {e}"))?;
    Ok(parsed)
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
    let body = round_trip(addr, m::PROC_LIST)?;
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

/// Recent PS5 kernel log lines (`dmesg`-style output). Returned verbatim
/// from `sysctl kern.msgbuf` on the PS5 — the kernel's in-memory printk/
/// printf history. Used by the desktop's "System log" panel to diagnose
/// "why didn't the payload load" / "what error happened silently" without
/// requiring the user to FTP/ssh into the console.
///
/// Up to ~1 MiB returned (payload-side HARD_CAP); can be empty if the
/// kernel rebuilt with a smaller buffer or the sysctl reset between
/// calls. Most firmwares surface 128 KiB – 256 KiB of recent kernel
/// output.
pub fn syslog_tail(addr: &str) -> Result<String> {
    let body = round_trip(addr, m::LOG_SYSLOG)?;
    // Kernel printf output is plain ASCII; lossy decode covers any stray
    // non-UTF-8 byte (driver scribbles, raw register dumps) without
    // failing the whole panel render.
    Ok(String::from_utf8_lossy(&body).into_owned())
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
    fn parse_hw_temps_decodes_plausible_reading() {
        // A normal reading from a fixed payload (PS5 Pro 9.60): power is 0
        // by design (the direct power API hangs that firmware), everything
        // else is in range and must pass through untouched.
        let body = b"cpu_temp=36\nsoc_temp=34\ncpu_freq_mhz=800\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 36);
        assert_eq!(t.soc_temp, 34);
        assert_eq!(t.cpu_freq_mhz, 800);
        assert_eq!(t.soc_power_mw, 0);
    }

    #[test]
    fn sanitize_zeros_old_payload_garbage_temps() {
        // The exact garbage an OLD payload emitted on FW 5.10: the sysctl
        // thermal-zone fallback's raw value rendered as "26747 °C". Must be
        // dropped to 0 = "unavailable", never shown to the user.
        let body =
            b"cpu_temp=26747\nsoc_temp=44000\ncpu_freq_mhz=800\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 0, "26747 °C must be rejected");
        assert_eq!(t.soc_temp, 0, "44000 °C must be rejected");
        assert_eq!(t.cpu_freq_mhz, 800, "an in-range clock survives");
    }

    #[test]
    fn sanitize_zeros_garbage_cpu_freq() {
        // The unbounded direct cpu-freq read on FW 5.10 surfaced ~5.2e13 MHz.
        let body = b"cpu_temp=45\nsoc_temp=40\ncpu_freq_mhz=51874615006914\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 45);
        assert_eq!(t.cpu_freq_mhz, 0, "5.2e13 MHz must be rejected");
    }

    #[test]
    fn sanitize_clamps_boundary_values() {
        // Exactly-at-bound values are valid; one past is not.
        let lo = sanitize_temps(HwTemps {
            cpu_temp: SENSOR_TEMP_MIN_C,
            soc_temp: SENSOR_TEMP_MAX_C,
            m2_temp: 45,
            cpu_freq_mhz: SENSOR_CPU_FREQ_MAX_MHZ,
            soc_clock_mhz: 0,
            soc_power_mw: SENSOR_POWER_MAX_MW,
            cpu_usage_pct: 0,
            fan_duty_pct: 100,
            product_shape: 3,
            fan_pinned_c: 0,
            fan_target_c: -1,
            fan_stock_c: 0,
            ..Default::default()
        });
        assert_eq!(lo.cpu_temp, SENSOR_TEMP_MIN_C);
        assert_eq!(lo.soc_temp, SENSOR_TEMP_MAX_C);
        assert_eq!(lo.m2_temp, 45, "in-range M.2 temp passes through");
        assert_eq!(lo.cpu_freq_mhz, SENSOR_CPU_FREQ_MAX_MHZ);
        assert_eq!(lo.soc_power_mw, SENSOR_POWER_MAX_MW);
        assert_eq!(lo.cpu_usage_pct, 0, "0% usage is valid");
        assert_eq!(lo.fan_duty_pct, 100, "100% duty is valid");
        assert_eq!(lo.product_shape, 3, "raw product shape passes through");

        let hi = sanitize_temps(HwTemps {
            cpu_temp: SENSOR_TEMP_MAX_C + 1,
            soc_temp: 0, // 0 = "no reading"; stays 0 (it's below MIN, already unavailable)
            m2_temp: SENSOR_TEMP_MAX_C + 1,
            cpu_freq_mhz: SENSOR_CPU_FREQ_MAX_MHZ + 1,
            soc_clock_mhz: -1,
            soc_power_mw: SENSOR_POWER_MAX_MW + 1,
            cpu_usage_pct: 101,
            fan_duty_pct: -5,
            product_shape: -1,
            fan_pinned_c: 0,
            fan_target_c: -1,
            fan_stock_c: 0,
            ..Default::default()
        });
        assert_eq!(hi.cpu_temp, 0, "one above max temp is rejected");
        assert_eq!(hi.soc_temp, 0);
        assert_eq!(hi.m2_temp, 0, "out-of-range M.2 temp is rejected");
        assert_eq!(hi.cpu_freq_mhz, 0, "one above max freq is rejected");
        assert_eq!(hi.soc_clock_mhz, 0, "negative clock is rejected");
        assert_eq!(hi.soc_power_mw, 0, "one above max power is rejected");
        assert_eq!(hi.cpu_usage_pct, -1, "101% usage → unavailable");
        assert_eq!(hi.fan_duty_pct, -1, "negative duty → unavailable");
    }

    #[test]
    fn parse_hw_temps_extended_fields() {
        // A new-payload reading that carries the extra telemetry. Power is
        // now a real value, usage/duty are valid percentages, and product
        // shape is a raw code that passes through untouched.
        let body = b"cpu_temp=42\nsoc_temp=40\ncpu_freq_mhz=2560\nsoc_clock_mhz=0\nsoc_power_mw=18000\ncpu_usage_pct=37\nfan_duty_pct=55\nproduct_shape=2\nfan_pinned_c=65\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.soc_power_mw, 18000);
        assert_eq!(t.cpu_usage_pct, 37);
        assert_eq!(t.fan_duty_pct, 55);
        assert_eq!(t.product_shape, 2);
        assert_eq!(t.fan_pinned_c, 65, "pinned fan threshold passes through");
    }

    #[test]
    fn parse_hw_temps_fan_guard_fields() {
        // The fan lines exactly as payload/src/hw_info.c hw_fan_status_text writes them.
        let body = b"cpu_temp=52\nfan_duty_pct=19\nfan_pinned_c=75\nfan_target_c=75\nfan_stock_c=91\nfan_min_c=45\nfan_max_c=85\nfan_restores=3\nfan_last_restore_s=12\nfan_guard=ok\nfan_filtered_c=47.3\nfan_servo=00000000005b\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.fan_restores, 3);
        assert_eq!(t.fan_last_restore_s, 12);
        assert_eq!(t.fan_guard, "ok");
        assert_eq!(t.fan_filtered_c, Some(47.3));
        // Never restored, unreadable filtered temperature, an unknown guard word.
        let body = b"fan_restores=-2\nfan_last_restore_s=-7\nfan_guard=maybe\nfan_filtered_c=-1\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.fan_restores, 0, "a negative count is not a count");
        assert_eq!(t.fan_last_restore_s, -1);
        assert_eq!(t.fan_guard, "");
        assert_eq!(t.fan_filtered_c, None);
        // An older helper sends none of them.
        let t = parse_hw_temps(b"cpu_temp=36\n");
        assert_eq!((t.fan_restores, t.fan_last_restore_s), (0, -1));
        assert_eq!(t.fan_guard, "");
        assert_eq!(t.fan_filtered_c, None);
    }

    #[test]
    fn parse_hw_temps_old_payload_omits_extended_fields() {
        // An OLDER payload sends only the original five keys. The new fields
        // must read back as -1 = "unavailable" (NOT 0, which would render as
        // a real 0% reading), so the UI shows a dash.
        let body = b"cpu_temp=36\nsoc_temp=34\ncpu_freq_mhz=800\nsoc_clock_mhz=0\nsoc_power_mw=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 36);
        assert_eq!(t.cpu_usage_pct, -1, "missing usage ⇒ unavailable");
        assert_eq!(t.fan_duty_pct, -1, "missing duty ⇒ unavailable");
        assert_eq!(t.product_shape, -1, "missing shape ⇒ unavailable");
        assert_eq!(t.fan_pinned_c, 0, "missing fan_pinned_c ⇒ 0 (no pin)");
    }

    #[test]
    fn parse_hw_temps_says_how_the_read_went() {
        let body = b"cpu_temp=0\nsoc_temp=52\ncpu_freq_mhz=800\nsoc_clock_mhz=0\nsoc_power_mw=0\n\
            cpu_temp_rc=0x80020016\nsoc_temp_rc=0x00000000\nread_ms=1234\nstale=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_temp, 0);
        assert_eq!(t.soc_temp, 52);
        assert_eq!(t.cpu_temp_rc.as_deref(), Some("0x80020016"));
        assert_eq!(t.soc_temp_rc.as_deref(), Some("0x00000000"));
        assert_eq!(t.read_ms, Some(1234));
        assert!(!t.stale);

        // The answer a caller gets while another request's read is stuck.
        let body = b"cpu_temp=50\ncpu_temp_rc=fault\nsoc_temp_rc=none\nread_ms=30000\nstale=1\n";
        let t = parse_hw_temps(body);
        assert_eq!(t.cpu_temp_rc.as_deref(), Some("fault"));
        assert_eq!(t.soc_temp_rc.as_deref(), Some("none"));
        assert_eq!(t.read_ms, Some(30000));
        assert!(t.stale);

        // Garbage reads as absent, not as a value and not as a failed parse.
        let t = parse_hw_temps(b"cpu_temp_rc=a b\nsoc_temp_rc=\nread_ms=-1\nstale=yes\n");
        assert_eq!(t.cpu_temp_rc, None);
        assert_eq!(t.soc_temp_rc, None);
        assert_eq!(t.read_ms, None);
        assert!(!t.stale);
    }

    #[test]
    fn parse_hw_temps_older_helper_sends_no_read_report() {
        let t = parse_hw_temps(b"cpu_temp=36\nsoc_temp=34\ncpu_freq_mhz=800\n");
        assert_eq!(t.cpu_temp, 36);
        assert_eq!(t.cpu_temp_rc, None);
        assert_eq!(t.soc_temp_rc, None);
        assert_eq!(t.read_ms, None);
        assert!(!t.stale);
        // And the JSON the app reads keeps its keys, snake_case, either way.
        let v = serde_json::to_value(&t).unwrap();
        assert!(v.get("cpu_temp_rc").is_some() && v.get("read_ms").is_some());
    }

    #[test]
    fn parse_hw_temps_m2_temp() {
        // New payload with an M.2 NVMe drive installed: channel 2 reports
        // a valid temp alongside soc_temp.
        let body = b"cpu_temp=42\nsoc_temp=40\nm2_temp=48\ncpu_freq_mhz=2560\nsoc_clock_mhz=0\nsoc_power_mw=18000\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.m2_temp, 48, "M.2 temp parsed from channel 2");
    }

    #[test]
    fn parse_hw_temps_m2_temp_absent() {
        // No M.2 drive installed (empty slot): payload sends m2_temp=0,
        // which is a valid "no drive" reading and must pass through.
        let body = b"cpu_temp=42\nsoc_temp=40\nm2_temp=0\ncpu_freq_mhz=2560\nsoc_clock_mhz=0\nsoc_power_mw=18000\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.m2_temp, 0, "0 = no M.2 drive installed");
    }

    #[test]
    fn parse_hw_temps_m2_temp_omitted() {
        // Older payload that doesn't send m2_temp at all: default to 0
        // (no drive), same as absent.
        let body =
            b"cpu_temp=42\nsoc_temp=40\ncpu_freq_mhz=2560\nsoc_clock_mhz=0\nsoc_power_mw=18000\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.m2_temp, 0, "missing m2_temp ⇒ 0 (no drive)");
    }

    #[test]
    fn sanitize_passes_unavailable_zero_through() {
        // All-zero (every sensor unavailable) is a valid state and must
        // round-trip as zeros, not be mistaken for garbage.
        let t = sanitize_temps(HwTemps::default());
        assert_eq!(t.cpu_temp, 0);
        assert_eq!(t.soc_temp, 0);
        assert_eq!(t.cpu_freq_mhz, 0);
        assert_eq!(t.soc_power_mw, 0);
    }

    #[test]
    fn parse_kv_ignores_missing_keys() {
        let body = b"cpu_temp=50\n";
        let get = parse_kv(body);
        assert_eq!(get("cpu_temp"), Some("50"));
        assert_eq!(get("unknown_key"), None);
    }

    /// Parse the post-2.2.26 sysctl-based proc_list shape into the
    /// engine struct. Mirrors the live-decoder body used by
    /// `proc_list()` so a payload-shape regression here lights up
    /// the engine immediately.
    fn parse_proc_list_body(body: &[u8]) -> ProcList {
        let v: serde_json::Value = serde_json::from_slice(body).expect("valid JSON");
        let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
        let mut truncated = false;
        let mut procs = Vec::new();
        if let Some(arr) = v.get("procs").and_then(|x| x.as_array()) {
            for item in arr {
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
        ProcList {
            ok,
            procs,
            truncated,
            error,
        }
    }

    #[test]
    fn parse_proc_list_normal() {
        let body = br#"{"ok":true,"procs":[{"pid":97,"name":"SceShellUI"},{"pid":113,"name":"payload.elf"}]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert!(!pl.truncated);
        assert_eq!(pl.procs.len(), 2);
        assert_eq!(pl.procs[0].pid, 97);
        assert_eq!(pl.procs[0].name, "SceShellUI");
        assert_eq!(pl.procs[1].name, "payload.elf");
        assert!(pl.error.is_none());
    }

    #[test]
    fn parse_proc_list_truncated() {
        // Payload emits a sentinel object with truncated:true when the
        // walk hit the buffer cap.
        let body = br#"{"ok":true,"procs":[{"pid":1,"name":"init"},{"truncated":true}]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert!(pl.truncated);
        // The sentinel object isn't counted as a process — only real
        // {pid,name} entries should land in `procs`.
        assert_eq!(pl.procs.len(), 1);
    }

    #[test]
    fn parse_proc_list_empty_list() {
        // Edge case: payload emits ok:true but no entries.
        let body = br#"{"ok":true,"procs":[]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert!(!pl.truncated);
        assert!(pl.procs.is_empty());
    }

    #[test]
    fn parse_proc_list_error_shape() {
        // sysctl probe failed shape; engine surfaces the error code so
        // UI can map "proc_list_sysctl_size_failed" → human string.
        let body = br#"{"ok":false,"error":"proc_list_sysctl_read_failed"}"#;
        let pl = parse_proc_list_body(body);
        assert!(!pl.ok);
        assert!(pl.procs.is_empty());
        assert_eq!(pl.error.as_deref(), Some("proc_list_sysctl_read_failed"),);
    }

    #[test]
    fn parse_proc_list_skips_unnamed_entry() {
        // A kinfo_proc with a NUL-only tdname json-escapes to "".
        // Defensive: we shouldn't surface those as zero-name rows.
        let body = br#"{"ok":true,"procs":[{"pid":42,"name":""},{"pid":43,"name":"valid"}]}"#;
        let pl = parse_proc_list_body(body);
        assert!(pl.ok);
        assert_eq!(pl.procs.len(), 1);
        assert_eq!(pl.procs[0].pid, 43);
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

        // 0 is "give the fan back to the console": it passes the range check and fails only
        // because nothing is listening.
        let restore = hw_set_fan_threshold("127.0.0.1:0", FAN_RESTORE_CONSOLE);
        assert!(!format!("{}", restore.unwrap_err()).contains("safe range"));

        // 85 is the mildest target we set (still under the console's own 91); 86 is not.
        let mild = hw_set_fan_threshold("127.0.0.1:0", 85);
        assert!(!format!("{}", mild.unwrap_err()).contains("safe range"));
        assert!(
            format!("{}", hw_set_fan_threshold("127.0.0.1:0", 86).unwrap_err())
                .contains("safe range")
        );
    }

    #[test]
    fn parse_hw_temps_clock_lines() {
        // The clock lines exactly as payload/src/mp1_clock.c writes them for "cd".
        let body = b"cpu_temp=52\ncpu_freq_mhz=800\nsoc_clock_mhz=2230\ngpu_clock_src=mp1\ncpu_core_mhz=3500,2844,2844,2844,2844,2844,2844,2844\ncpu_core_mhz_min=2844\ncpu_core_mhz_max=3500\nmp1_clk_raw=00001000,00001001\nmp1_core_raw=0:3500,2:2844\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.soc_clock_mhz, 2230);
        assert_eq!(t.cpu_freq_mhz, 800, "the reference clock is kept");
        assert_eq!(t.gpu_clock_src, "mp1");
        assert_eq!(
            t.cpu_core_mhz,
            vec![3500, 2844, 2844, 2844, 2844, 2844, 2844, 2844]
        );
        assert_eq!((t.cpu_core_mhz_min, t.cpu_core_mhz_max), (2844, 3500));
        assert_eq!(t.cpu_measure_src, "", "an older helper says nothing");
        assert!(
            !t.mp1_busy && !t.measure_busy,
            "an older helper says nothing"
        );
        assert!(t.cpu_core_measured_mhz.is_empty());
    }

    #[test]
    fn parse_hw_temps_measured_clocks() {
        // As measured on a PS5 (FW 13.60): cores 0-3 timed, game cores 0; garbage reads 0 and
        // the list never grows past eight.
        let body =
            b"cpu_measure_src=add_chain\ncpu_core_measured_mhz=3100,3170,2940,5001,0,x,0,0,1234\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.cpu_measure_src, "add_chain");
        assert_eq!(
            t.cpu_core_measured_mhz,
            vec![3100, 3170, 2940, 0, 0, 0, 0, 0]
        );
        let t = sanitize_temps(parse_hw_temps(b"cpu_measure_src=bogus\n"));
        assert_eq!(t.cpu_measure_src, "");
    }

    #[test]
    fn parse_hw_temps_clock_garbage_and_out_of_range() {
        // Out-of-range GPU, garbage and out-of-range cores, too many cores, and min/max that
        // disagree with the list: the list wins, bad entries read 0, never more than eight.
        let body = b"soc_clock_mhz=2501\ngpu_clock_src=mp1\ncpu_core_mhz=3500,abc,399,4001,,-5,99999999999,3200,3000,3100\ncpu_core_mhz_min=1\ncpu_core_mhz_max=99999\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.soc_clock_mhz, 0, "2501 MHz GPU is rejected");
        assert_eq!(t.cpu_core_mhz, vec![3500, 0, 0, 0, 0, 0, 0, 3200]);
        assert_eq!((t.cpu_core_mhz_min, t.cpu_core_mhz_max), (3200, 3500));

        let t = sanitize_temps(parse_hw_temps(b"soc_clock_mhz=99\n"));
        assert_eq!(t.soc_clock_mhz, 0, "below 100 MHz is rejected");
        let t = sanitize_temps(parse_hw_temps(b"soc_clock_mhz=100\n"));
        assert_eq!(t.soc_clock_mhz, 100);
        let t = sanitize_temps(parse_hw_temps(b"soc_clock_mhz=2500\n"));
        assert_eq!(t.soc_clock_mhz, 2500);
        let t = sanitize_temps(parse_hw_temps(b"soc_clock_mhz=-2230\n"));
        assert_eq!(t.soc_clock_mhz, 0);

        // The read has not come back: the helper says which, and the rest says nothing.
        let t = parse_hw_temps(b"mp1_busy=1\nmeasure_busy=0\n");
        assert!(t.mp1_busy && !t.measure_busy);

        // Unsupported: all zero, no min/max, the source word kept so the UI can say so.
        let body = b"soc_clock_mhz=0\ngpu_clock_src=unsupported\ncpu_core_mhz=0,0,0,0,0,0,0,0\ncpu_core_mhz_min=0\ncpu_core_mhz_max=0\n";
        let t = sanitize_temps(parse_hw_temps(body));
        assert_eq!(t.gpu_clock_src, "unsupported");
        assert_eq!(t.cpu_core_mhz, vec![0; 8]);
        assert_eq!((t.cpu_core_mhz_min, t.cpu_core_mhz_max), (0, 0));

        // An unknown source word, and min/max with no list.
        let t = sanitize_temps(parse_hw_temps(
            b"gpu_clock_src=smu<script>\ncpu_core_mhz_min=3000\ncpu_core_mhz_max=3500\n",
        ));
        assert_eq!(t.gpu_clock_src, "");
        assert!(t.cpu_core_mhz.is_empty());
        assert_eq!((t.cpu_core_mhz_min, t.cpu_core_mhz_max), (0, 0));
    }

    #[test]
    fn parse_hw_temps_without_clock_lines() {
        // A basic/extended read, or an older helper: nothing claims to have been read.
        let t = sanitize_temps(parse_hw_temps(
            b"cpu_temp=36\ncpu_freq_mhz=800\nsoc_clock_mhz=0\n",
        ));
        assert_eq!(t.gpu_clock_src, "");
        assert!(t.cpu_core_mhz.is_empty());
        assert_eq!(t.soc_clock_mhz, 0);
    }

    #[test]
    fn hw_temps_selector_sends_c_only_when_clocks_are_asked_for() {
        assert_eq!(hw_temps_selector(false, false), b"");
        assert_eq!(hw_temps_selector(true, false), b"ufs");
        assert_eq!(hw_temps_selector(false, true), b"c");
        assert_eq!(hw_temps_selector(true, true), b"ufsc");
        // Never the raw-words debug flag, never power.
        for (e, c) in [(false, false), (true, false), (false, true), (true, true)] {
            let s = hw_temps_selector(e, c);
            assert!(!s.contains(&b'd') && !s.contains(&b'p') && !s.contains(&b'1'));
        }
    }
}
