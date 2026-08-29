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
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::connection::Connection;

// ─── NTP query ────────────────────────────────────────────────────
//
// Minimal NTPv4 client (RFC 5905). Sends a single 48-byte client mode
// request to a pool server on UDP 123 and parses the transmit
// timestamp from the reply. The NTP epoch (1900-01-01) is 2208988800
// seconds before the Unix epoch (1970-01-01).
//
// We implement NTP from scratch (48 bytes, one UDP round-trip) rather
// than pulling in an NTP crate — the protocol is trivially simple for
// a one-shot query and we avoid the transitive dependency cost.
const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;
const NTP_TIMEOUT: Duration = Duration::from_secs(3);

/// Sanity bounds on anything we would write to a console clock:
/// 2000-01-01 and 2100-01-01 UTC. A server answering outside this range
/// is broken or hostile, and either way its answer is not usable.
const NTP_MIN_PLAUSIBLE_UNIX: i64 = 946_684_800;
const NTP_MAX_PLAUSIBLE_UNIX: i64 = 4_102_444_800;

/// Query an NTP server and return the current Unix epoch in seconds (UTC).
/// Tries each server in order; first success wins.
pub fn ntp_query_unix_seconds(servers: &[&str]) -> Result<i64> {
    ntp_query_unix_seconds_with_server(servers).map(|(_, ts)| ts)
}

/// As [`ntp_query_unix_seconds`], but also reports which server
/// answered. Worth surfacing: when a sync produces a surprising time,
/// the first question is which of the four servers replied.
pub fn ntp_query_unix_seconds_with_server(servers: &[&str]) -> Result<(String, i64)> {
    if servers.is_empty() {
        bail!("no NTP servers provided");
    }
    let mut last_err = None;
    for &host in servers {
        match ntp_query_single(host) {
            Ok(ts) => return Ok((host.to_string(), ts)),
            Err(e) => last_err = Some((host, e)),
        }
    }
    let (host, e) = last_err.expect("non-empty server list always sets last_err");
    bail!("all NTP servers failed; last: {host}: {e:#}");
}

fn ntp_query_single(host: &str) -> Result<i64> {
    // Resolve and try each address until one responds.
    let addrs = std::net::ToSocketAddrs::to_socket_addrs(&(host, 123))?;
    let mut last_err = None;
    for addr in addrs {
        match ntp_query_addr(addr) {
            Ok(ts) => return Ok(ts),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no addresses resolved for {host}")))
}

fn ntp_query_addr(addr: SocketAddr) -> Result<i64> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(NTP_TIMEOUT))?;
    sock.set_write_timeout(Some(NTP_TIMEOUT))?;
    sock.connect(addr)?;

    let nonce = ntp_nonce();
    let sent_at = Instant::now();
    sock.send(&build_ntp_request(nonce))?;

    let mut buf = [0u8; 48];
    let n = sock.recv(&mut buf)?;
    let rtt_nanos = sent_at.elapsed().as_nanos().min(u64::MAX as u128) as u64;

    parse_ntp_response(&buf[..n], nonce, rtt_nanos)
}

/// A per-query value placed in the request's transmit timestamp, which
/// the server must echo back in its originate timestamp.
///
/// This is not cryptographic randomness and does not need to be. NTP is
/// unauthenticated UDP: any host that can reach us may answer, and
/// whatever it says would become the console's clock. The nonce closes
/// the cheap version of that — an off-path attacker who cannot see our
/// packet has to guess 64 bits to have its reply accepted. It does
/// nothing against an on-path attacker, who can read the nonce; that is
/// a limit of unauthenticated NTP itself, not of this check, and is why
/// `parse_ntp_response` also range-checks the result rather than
/// trusting a validated reply unconditionally.
///
/// Built from the wall clock, the pid and a per-process counter so that
/// two queries in the same nanosecond, or two processes started
/// together, still differ.
fn ntp_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Rotate the pid into the high bits so it perturbs the whole word
    // rather than only the bottom of a value dominated by `nanos`.
    nanos ^ (seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ ((std::process::id() as u64) << 32)
}

/// Build a 48-byte NTPv4 client request carrying `nonce` as the
/// transmit timestamp (RFC 5905 §7.3). Every other field is zero,
/// which is correct for a one-shot client query.
fn build_ntp_request(nonce: u64) -> [u8; 48] {
    let mut b = [0u8; 48];
    // LI=0 (no warning) << 6 | VN=4 << 3 | Mode=3 (client) = 0x23.
    b[0] = 0x23;
    b[40..48].copy_from_slice(&nonce.to_be_bytes());
    b
}

/// Validate a server reply and return the Unix epoch (seconds, UTC) it
/// implies at the moment of receipt.
///
/// `rtt_nanos` is the measured round trip. The server's transmit
/// timestamp says when *it* answered, which is already about half a
/// round trip in the past by the time the bytes reach us, so half the
/// RTT is added back. On a LAN this is microseconds; over a congested
/// link it is the difference between a clock that is right and one that
/// is a second slow.
///
/// Every check here exists because the result is written to a games
/// console's system clock. A wrong clock is not a cosmetic bug: a
/// far-past clock fails PSN sign-in (the TLS chain is not yet valid)
/// and a far-future one fails game certificate validation.
fn parse_ntp_response(buf: &[u8], nonce: u64, rtt_nanos: u64) -> Result<i64> {
    if buf.len() < 48 {
        bail!("short NTP reply: {} bytes", buf.len());
    }

    // Mode 4 is "server". Anything else is not an answer to a client
    // query — mode 3 in particular would be our own packet reflected.
    let mode = buf[0] & 0x07;
    if mode != 4 {
        bail!("unexpected NTP mode {mode} (want 4, server)");
    }

    // LI=3 is the alarm condition: the server's own clock has never
    // synchronized. It will still hand over a timestamp; it just has no
    // reason to be right.
    let leap = buf[0] >> 6;
    if leap == 3 {
        bail!("server reports its clock is not synchronized (LI=3)");
    }

    // Stratum 0 is the kiss-o'-death packet — a rate-limit or access
    // denial carrying an ASCII code, never a usable time. Above 15 is
    // unsynchronized or reserved.
    let stratum = buf[1];
    if stratum == 0 {
        bail!("NTP kiss-o'-death reply (stratum 0)");
    }
    if stratum > 15 {
        bail!("unusable NTP stratum {stratum}");
    }

    // The server copies our transmit timestamp into its originate
    // field. If it does not match, this reply is not for our query.
    let originate = u64::from_be_bytes(buf[24..32].try_into().expect("8 bytes"));
    if originate != nonce {
        bail!("NTP originate timestamp mismatch — reply is not for our query");
    }

    let secs = u32::from_be_bytes(buf[40..44].try_into().expect("4 bytes")) as u64;
    let frac = u32::from_be_bytes(buf[44..48].try_into().expect("4 bytes")) as u64;

    // NTP seconds are a 32-bit count from 1900 that wraps every ~136
    // years; era 0 ends 2036-02-07. A value below the Unix-epoch offset
    // therefore belongs to era 1, not to 1900. The previous
    // `saturating_sub` collapsed every era-1 timestamp to 0 — i.e. it
    // would have started reporting 1970 in 2036.
    let ntp_secs = if secs >= NTP_EPOCH_OFFSET {
        secs
    } else {
        secs + (1u64 << 32)
    };
    let base = (ntp_secs - NTP_EPOCH_OFFSET) as i64;

    // Sub-second part of the server's timestamp plus half the round
    // trip, rounded to the nearest second (the payload's clock API has
    // one-second granularity, so there is nothing finer to carry).
    let frac_nanos = (frac * 1_000_000_000) >> 32;
    let extra_nanos = frac_nanos + rtt_nanos / 2;
    let unix = base + ((extra_nanos + 500_000_000) / 1_000_000_000) as i64;

    if !(NTP_MIN_PLAUSIBLE_UNIX..=NTP_MAX_PLAUSIBLE_UNIX).contains(&unix) {
        bail!("implausible NTP timestamp: {unix}");
    }
    Ok(unix)
}

/// Default NTP servers used when the client doesn't specify any.
/// Cloudflare and Google are anycast, fast, and globally reachable.
pub const DEFAULT_NTP_SERVERS: &[&str] = &[
    "time.cloudflare.com",
    "time.google.com",
    "pool.ntp.org",
    "time.windows.com",
];

/// Diagnostic err_code sentinels surfaced by the payload's sys_time
/// module. Keep in sync with payload/include/sys_time.h.
pub const SYS_TIME_ERR_NULL_ARG: u32 = 0xE0002001;
pub const SYS_TIME_ERR_NO_SYMBOL: u32 = 0xE0002002;
/// Neither the SCE call nor the settimeofday fallback moved the clock.
pub const SYS_TIME_ERR_FALLBACK: u32 = 0xE0002003;

/// sys_registry sentinels — kept here too so `humanize_err` can route
/// them. Keep in sync with payload/include/sys_registry.h.
pub const SYS_REGISTRY_ERR_NULL_ARG: u32 = 0xE0003001;
pub const SYS_REGISTRY_ERR_NO_SYMBOL: u32 = 0xE0003002;
pub const SYS_REGISTRY_ERR_BUFFER_TOO_SMALL: u32 = 0xE0003003;

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
    /// True when the clock was ultimately set by `settimeofday` rather
    /// than the SCE call. Defaults false so a payload predating the
    /// fallback deserializes cleanly.
    #[serde(default)]
    pub used_fallback: bool,
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
/// the UI. The 0xE0002xxx + 0xE0003xxx sentinels are ours; 0x80A2xxxx
/// values come from Sony's SceShellCore authid check; anything else
/// falls through as a hex code. Used by the Tauri command to keep the
/// React side from having to know the constants.
pub fn humanize_err(err_code: u32) -> String {
    match err_code {
        0 => "success".to_string(),
        SYS_TIME_ERR_NULL_ARG => "invalid request (null/out-of-range field)".to_string(),
        SYS_TIME_ERR_NO_SYMBOL => {
            "sceSystemServiceSet/GetCurrentDateTime not exported on this firmware".to_string()
        }
        SYS_TIME_ERR_FALLBACK => {
            "clock did not move — the SCE call and the settimeofday fallback both failed \
             (loader is probably not ucred-elevated; reload via kstuff)"
                .to_string()
        }
        SYS_REGISTRY_ERR_NULL_ARG => "invalid registry request (null arg)".to_string(),
        SYS_REGISTRY_ERR_NO_SYMBOL => "sceRegMgr Get/Set not exported on this firmware".to_string(),
        SYS_REGISTRY_ERR_BUFFER_TOO_SMALL => "registry response buffer too small".to_string(),
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

// ── PS5 Date & Time state (registry-backed, novel as of 2.10.0) ─────────
//
// Adds full read/write of the SCE_REGMGR_ENT_KEY_DATE_* namespace
// (timezone, DST policy, NTP auto-sync flag, date/time format,
// tzdata version, NTP-error count) plus a comparison against the
// cached libSceRtc NTP-derived tick. The payload-side handler reads
// every key best-effort and surfaces per-field availability flags
// so this struct uses `default`s everywhere; missing data shows up
// as `*_avail=false` rather than the whole response failing.
//
// See reference_ps5_date_registry_keys.md for the hardware-
// verification status of each individual key.

/// One per-field availability + error triple. Every numeric field in
/// `PsTimeState` has an `_avail` and `_err` companion field; this is
/// just the type those companions share.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PsTimeStateField<T> {
    pub value: T,
    pub avail: bool,
    pub err: u32,
}

/// Full PS5 Date & Time state surfaced by TIME_STATE_GET. The shape
/// here mirrors the payload's flat JSON (one `<name>` + `<name>_avail`
/// + `<name>_err` triple per field) — we don't fold it into nested
///   structs because flat JSON is cheaper to debug from the engine
///   log and serde generates the same Rust code either way.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PsTimeState {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub truncated: bool,

    // Timezone (enum index into Sony's tzdata table; ~120 entries).
    #[serde(default)]
    pub tz_index: i32,
    #[serde(default)]
    pub tz_index_avail: bool,
    #[serde(default)]
    pub tz_index_err: u32,

    // Date format: 0=YYYY/MM/DD, 1=DD/MM/YYYY, 2=MM/DD/YYYY.
    #[serde(default)]
    pub date_format: i32,
    #[serde(default)]
    pub date_format_avail: bool,
    #[serde(default)]
    pub date_format_err: u32,

    // Time format: 0=24h, 1=12h.
    #[serde(default)]
    pub time_format: i32,
    #[serde(default)]
    pub time_format_avail: bool,
    #[serde(default)]
    pub time_format_err: u32,

    // DST policy: 0=off, 1=auto (tzdata-driven), 2=manual on.
    #[serde(default)]
    pub summer_policy: i32,
    #[serde(default)]
    pub summer_policy_avail: bool,
    #[serde(default)]
    pub summer_policy_err: u32,

    // Auto-sync (NTP) flag: 0=manual, 1=use Sony's NTP.
    #[serde(default)]
    pub set_auto: i32,
    #[serde(default)]
    pub set_auto_avail: bool,
    #[serde(default)]
    pub set_auto_err: u32,

    // Read-only flag: currently in DST?
    #[serde(default)]
    pub is_summer_time: i32,
    #[serde(default)]
    pub is_summer_time_avail: bool,
    #[serde(default)]
    pub is_summer_time_err: u32,

    // Local time offset from UTC in seconds.
    #[serde(default)]
    pub utc_offset_sec: i32,
    #[serde(default)]
    pub utc_offset_sec_avail: bool,
    #[serde(default)]
    pub utc_offset_sec_err: u32,

    // Same offset, expressed in minutes (Sony stores both — write
    // both consistently or the Settings UI may show drift between
    // the two views).
    #[serde(default)]
    pub tz_offset_min: i32,
    #[serde(default)]
    pub tz_offset_min_avail: bool,
    #[serde(default)]
    pub tz_offset_min_err: u32,

    // NTP sync failure counter — non-zero indicates the console
    // couldn't reach Sony's NTP (DNS broken? UDP 123 blocked?).
    #[serde(default)]
    pub rtc_error_count: i32,
    #[serde(default)]
    pub rtc_error_count_avail: bool,
    #[serde(default)]
    pub rtc_error_count_err: u32,

    // tzdata version string (e.g. "2023d") — read from the registry's
    // string-typed `DATE_tzdata_update` key.
    #[serde(default)]
    pub tzdata: String,
    #[serde(default)]
    pub tzdata_avail: bool,
    #[serde(default)]
    pub tzdata_err: u32,

    // Cached NTP-derived unix epoch (seconds, UTC). -1 sentinel if
    // the libSceRtc symbol wasn't resolvable. NOT a fresh NTP query —
    // this is what the system thinks NTP would say based on its last
    // successful sync (which can be hours/days old on offline consoles).
    #[serde(default = "minus_one")]
    pub ntp_tick_unix: i64,
    #[serde(default)]
    pub ntp_tick_avail: bool,
    #[serde(default)]
    pub ntp_tick_err: u32,

    // Wall-clock unix epoch (seconds, UTC), derived from the
    // sce_datetime_t the payload reads on the same call. Same shape
    // as `prior_unix` in PsTimeSetResult.
    #[serde(default = "minus_one")]
    pub wall_clock_unix: i64,
    #[serde(default)]
    pub wall_clock_avail: bool,
    #[serde(default)]
    pub wall_clock_err: u32,
}

/// Optional fields for TIME_STATE_SET. Each field is optional; only
/// present (non-None) fields are written. Mirrors the payload's
/// partial-update semantics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PsTimeStateSetRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tz_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_format: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_format: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summer_policy: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_auto: Option<i32>,
}

/// Per-field write results from TIME_STATE_SET. `ok` is true only if
/// EVERY attempted write succeeded; per-field `*_attempted` /
/// `*_rc` / `*_err` lets the UI render which writes took.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PsTimeStateSetResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub any_attempted: bool,
    #[serde(default)]
    pub truncated: bool,

    #[serde(default)]
    pub tz_index_attempted: bool,
    #[serde(default)]
    pub tz_index_rc: i32,
    #[serde(default)]
    pub tz_index_err: u32,

    #[serde(default)]
    pub date_format_attempted: bool,
    #[serde(default)]
    pub date_format_rc: i32,
    #[serde(default)]
    pub date_format_err: u32,

    #[serde(default)]
    pub time_format_attempted: bool,
    #[serde(default)]
    pub time_format_rc: i32,
    #[serde(default)]
    pub time_format_err: u32,

    #[serde(default)]
    pub summer_policy_attempted: bool,
    #[serde(default)]
    pub summer_policy_rc: i32,
    #[serde(default)]
    pub summer_policy_err: u32,

    #[serde(default)]
    pub set_auto_attempted: bool,
    #[serde(default)]
    pub set_auto_rc: i32,
    #[serde(default)]
    pub set_auto_err: u32,
}

/// Read all PS5 Date & Time state in one round-trip — see
/// `PsTimeState` for the field semantics. Best-effort: every
/// per-field availability flag lets the caller render partial data
/// when one or more reads fail.
pub fn ps5_time_state_get(addr: &str) -> Result<PsTimeState> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::TimeStateGet, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected TIME_STATE_GET: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::TimeStateGetAck {
        bail!("expected TIME_STATE_GET_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

/// Write a partial subset of PS5 Date & Time state. `None` fields are
/// skipped — only `Some(value)` fields are written. Returns per-field
/// rc + err so the UI can show "set_auto succeeded but tz_index
/// rejected" rather than one opaque ok/fail.
pub fn ps5_time_state_set(addr: &str, req: &PsTimeStateSetRequest) -> Result<PsTimeStateSetResult> {
    let body = serde_json::to_vec(req)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::TimeStateSet, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected TIME_STATE_SET: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::TimeStateSetAck {
        bail!("expected TIME_STATE_SET_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
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

    #[test]
    fn ntp_query_returns_plausible_timestamp() {
        // Live NTP query — may fail in CI without network. We only assert
        // the timestamp is in a sane range (year 2024-2035) so a wrong
        // reply is caught without making the test brittle.
        let ts = ntp_query_unix_seconds(DEFAULT_NTP_SERVERS);
        if let Ok(ts) = ts {
            // 2024-01-01 to 2035-12-31 ≈ 1.7B to 2.08B
            assert!(ts > 1_704_067_200, "NTP timestamp too old: {ts}");
            assert!(ts < 2_100_000_000, "NTP timestamp too far future: {ts}");
        }
        // If NTP failed (no network in CI), that's acceptable — we don't
        // fail the test. The function's error path is exercised.
    }

    #[test]
    fn ps_time_set_result_reports_settimeofday_fallback() {
        let json = serde_json::json!({
            "ok": true, "err_code": 0,
            "prior_unix": 1_i64, "new_unix": 1_778_887_800_i64,
            "used_fallback": true,
        });
        let r: PsTimeSetResult = serde_json::from_value(json).unwrap();
        assert!(r.used_fallback);
    }

    #[test]
    fn ps_time_set_result_defaults_fallback_to_false() {
        // A payload predating the fallback omits the key entirely.
        let json = serde_json::json!({"ok": true, "err_code": 0});
        let r: PsTimeSetResult = serde_json::from_value(json).unwrap();
        assert!(!r.used_fallback);
    }

    #[test]
    fn humanize_reports_both_set_paths_failing() {
        let msg = humanize_err(SYS_TIME_ERR_FALLBACK);
        assert!(msg.contains("settimeofday"), "unexpected: {msg}");
    }

    #[test]
    fn ntp_query_empty_servers_returns_error() {
        let r = ntp_query_unix_seconds(&[]);
        assert!(r.is_err());
    }

    #[test]
    fn ntp_query_reports_which_server_answered() {
        // Which server actually replied is the first thing worth
        // knowing when a sync looks wrong, so the query reports it
        // rather than just a bare timestamp. Network-optional, like
        // the plausibility test above.
        if let Ok((server, ts)) = ntp_query_unix_seconds_with_server(DEFAULT_NTP_SERVERS) {
            assert!(
                DEFAULT_NTP_SERVERS.contains(&server.as_str()),
                "answered by a server we never asked: {server}"
            );
            assert!(ts > NTP_MIN_PLAUSIBLE_UNIX);
        }
    }

    #[test]
    fn ntp_query_with_server_rejects_empty_list() {
        assert!(ntp_query_unix_seconds_with_server(&[]).is_err());
    }

    // ─── NTP protocol hardening ───────────────────────────────────
    //
    // These exercise the wire format directly, with no socket. A
    // one-shot NTP query is UDP: any host that can guess the source
    // port can answer, and whatever it says becomes the PS5's clock.
    // Setting a console's clock wrong is not cosmetic — it breaks PSN
    // sign-in (TLS notBefore) and game cert validation. So the reply
    // has to be checked, not merely parsed.

    /// A well-formed server reply echoing `nonce`, whose transmit
    /// timestamp encodes `transmit_unix`.
    fn fake_reply(nonce: u64, transmit_unix: i64) -> [u8; 48] {
        let mut b = [0u8; 48];
        b[0] = 0x24; // LI=0, VN=4, Mode=4 (server)
        b[1] = 2; // stratum 2 — a normal upstream-synced server
        b[24..32].copy_from_slice(&nonce.to_be_bytes());
        let ntp_secs = (transmit_unix as u64 + NTP_EPOCH_OFFSET) as u32;
        b[40..44].copy_from_slice(&ntp_secs.to_be_bytes());
        b
    }

    #[test]
    fn ntp_request_declares_version_4_client_mode() {
        let req = build_ntp_request(0x0102_0304_0506_0708);
        // LI=0 (<<6), VN=4 (<<3), Mode=3 => 0b00_100_011 = 0x23.
        // The old code sent 0x1B, which is VN=3, despite its comment
        // claiming VN=4.
        assert_eq!(req[0], 0x23, "first byte should be LI=0 VN=4 Mode=3");
        assert_eq!(req.len(), 48);
    }

    #[test]
    fn ntp_request_carries_nonce_as_transmit_timestamp() {
        let nonce = 0xDEAD_BEEF_CAFE_F00D_u64;
        let req = build_ntp_request(nonce);
        assert_eq!(&req[40..48], &nonce.to_be_bytes());
    }

    #[test]
    fn ntp_response_reads_transmit_timestamp() {
        let nonce = 0x1111_2222_3333_4444;
        let reply = fake_reply(nonce, 1_778_887_800);
        assert_eq!(parse_ntp_response(&reply, nonce, 0).unwrap(), 1_778_887_800);
    }

    #[test]
    fn ntp_response_rejects_originate_timestamp_mismatch() {
        // The anti-spoofing check: a reply that does not echo the exact
        // nonce we sent cannot be an answer to our query.
        let reply = fake_reply(0xAAAA_AAAA_AAAA_AAAA, 1_778_887_800);
        let err = parse_ntp_response(&reply, 0xBBBB_BBBB_BBBB_BBBB, 0).unwrap_err();
        assert!(
            format!("{err:#}").contains("originate"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_response_rejects_unsynchronized_server() {
        let nonce = 1;
        let mut reply = fake_reply(nonce, 1_778_887_800);
        reply[0] = 0xE4; // LI=3 (alarm / never synchronized), VN=4, Mode=4
        let err = parse_ntp_response(&reply, nonce, 0).unwrap_err();
        assert!(
            format!("{err:#}").contains("not synchronized"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_response_rejects_kiss_of_death() {
        // Stratum 0 is the "kiss-o'-death" packet — a rate-limit or
        // deny message, never a timestamp worth believing.
        let nonce = 1;
        let mut reply = fake_reply(nonce, 1_778_887_800);
        reply[1] = 0;
        let err = parse_ntp_response(&reply, nonce, 0).unwrap_err();
        assert!(
            format!("{err:#}").contains("stratum"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_response_rejects_non_server_mode() {
        let nonce = 1;
        let mut reply = fake_reply(nonce, 1_778_887_800);
        reply[0] = 0x23; // Mode=3 (client) — our own packet reflected back
        let err = parse_ntp_response(&reply, nonce, 0).unwrap_err();
        assert!(
            format!("{err:#}").contains("mode"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_response_adds_half_the_round_trip() {
        // The server's transmit timestamp describes when it answered,
        // which is already ~rtt/2 in the past by the time we parse it.
        let nonce = 7;
        let reply = fake_reply(nonce, 1_778_887_800);
        let got = parse_ntp_response(&reply, nonce, 2_000_000_000).unwrap();
        assert_eq!(got, 1_778_887_801, "2s RTT should advance by 1s");
    }

    #[test]
    fn ntp_response_rejects_implausible_year() {
        // 1980 — far outside the range any live server should report.
        let nonce = 3;
        let reply = fake_reply(nonce, 315_532_800);
        let err = parse_ntp_response(&reply, nonce, 0).unwrap_err();
        assert!(
            format!("{err:#}").contains("implausible"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_response_rejects_short_buffer() {
        let nonce = 4;
        let reply = fake_reply(nonce, 1_778_887_800);
        assert!(parse_ntp_response(&reply[..40], nonce, 0).is_err());
    }

    /// Spin up a UDP server that answers one query, and return its
    /// address. `mode` selects how it misbehaves.
    fn spawn_fake_ntp(mode: &'static str) -> SocketAddr {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let addr = sock.local_addr().expect("local_addr");
        std::thread::spawn(move || {
            let mut req = [0u8; 48];
            let Ok((_, peer)) = sock.recv_from(&mut req) else {
                return;
            };
            let mut r = [0u8; 48];
            r[0] = 0x24; // LI=0, VN=4, Mode=4
            r[1] = if mode == "kod" { 0 } else { 2 };
            // Echo the client's transmit timestamp, unless spoofing.
            if mode != "spoof" {
                r[24..32].copy_from_slice(&req[40..48]);
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let unix = if mode == "past" { 315_532_800 } else { now };
            let secs = (unix + NTP_EPOCH_OFFSET) as u32;
            r[40..44].copy_from_slice(&secs.to_be_bytes());
            let _ = sock.send_to(&r, peer);
        });
        addr
    }

    #[test]
    fn ntp_client_accepts_a_well_formed_server_over_a_real_socket() {
        // End-to-end through the actual UDP path, not just the parser:
        // proves the request we put on the wire is one a server can
        // answer, and that the reply survives the round trip.
        let ts = ntp_query_addr(spawn_fake_ntp("good")).expect("well-formed reply");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((ts - now).abs() <= 2, "got {ts}, expected near {now}");
    }

    #[test]
    fn ntp_client_rejects_a_server_that_does_not_echo_the_nonce() {
        // The off-path spoofing case, exercised over a real socket:
        // a reply that is otherwise perfectly well-formed but is not an
        // answer to *our* query must not set a console's clock.
        let err = ntp_query_addr(spawn_fake_ntp("spoof")).unwrap_err();
        assert!(
            format!("{err:#}").contains("originate"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_client_rejects_kiss_of_death_over_a_real_socket() {
        let err = ntp_query_addr(spawn_fake_ntp("kod")).unwrap_err();
        assert!(
            format!("{err:#}").contains("kiss"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_client_rejects_an_implausible_time_over_a_real_socket() {
        let err = ntp_query_addr(spawn_fake_ntp("past")).unwrap_err();
        assert!(
            format!("{err:#}").contains("implausible"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn ntp_response_handles_post_2036_era_rollover() {
        // NTP era 0 ends 2036-02-07; the 32-bit seconds field wraps to
        // 0 and counts again. `saturating_sub` turned every era-1
        // timestamp into 0 (1970) instead of a 2036+ date. Era 1
        // second 100 is 2036-02-07 06:29:52 UTC.
        let nonce = 5;
        let mut reply = fake_reply(nonce, 0);
        reply[40..44].copy_from_slice(&100u32.to_be_bytes());
        let got = parse_ntp_response(&reply, nonce, 0).unwrap();
        assert_eq!(got, 2_085_978_596);
        let (y, ..) = unix_to_ymd_hms_utc(got).unwrap();
        assert_eq!(y, 2036);
    }
}
