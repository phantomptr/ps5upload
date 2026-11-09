//! The running game's sampled frame rate (#774).
//!
//! The helper's `hw.fps` method (`payload/src/game_fps.c`) answers with the display controller's
//! frame count (the pictures it has flipped to the screen) and the helper's monotonic clock. Two
//! reads a second apart give the frames shown in that second: [`FpsMeter`] turns consecutive reads
//! into frames per second. Both numbers come from the console, so the network's delay does not
//! enter the result.
//!
//! What it is and is not:
//! * It is an average over each sample's interval (about one second), not a per-frame time. A
//!   10 ms hitch inside a second that still shows 59 frames reads as 59. So the "1% low" here is
//!   the average of the slowest 1% of seconds, not of frames.
//! * The count is the frames the display flipped: what you see on the TV, not frames the game
//!   rendered and dropped. A game capped at 30 reads 30 on a 60 Hz output.
//! * Each sample is off by at most one frame (the count is whole frames), so ±1 FPS at 1 s.
//! * Without a game the driver has nothing to count (`no_game`).
//!
//! The Remote Play stream's own frame rate is a different thing (the encoder's output) and is not
//! used here.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::console_files::is_unsupported;
use crate::mgmt::{self, m};

/// One read must come back well within the sampler's one-second step.
pub const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// Two reads closer than this give a noisy rate (a whole frame is a large share of a short
/// interval): the meter waits for a longer one.
pub const MIN_INTERVAL_US: u64 = 500_000;
/// Two reads further apart than this are not one sample: the meter starts over.
pub const MAX_INTERVAL_US: u64 = 10_000_000;
/// A rate above this is a counter jump (seen while a game loads), not frames.
pub const FPS_MAX: f64 = 240.0;

/// What `hw.fps` answers. The keys are the ones `game_fps_json` writes (`payload/src/game_fps.c`);
/// `payload/tests/game_fps_selftest.c` checks the same literals the tests below parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlipRead {
    /// `ok`, `no_game`, `no_device`, `failed`; and, from this side, `unsupported` (a helper older
    /// than the method).
    pub state: String,
    #[serde(default)]
    pub flips: u64,
    #[serde(default)]
    pub mono_us: u64,
    /// How many bytes the driver wrote (diagnostics).
    #[serde(default)]
    pub bytes: u32,
    #[serde(default)]
    pub errno: i32,
}

impl FlipRead {
    pub fn is_ok(&self) -> bool {
        self.state == "ok"
    }
}

pub fn parse_flip_read(body: &[u8]) -> Result<FlipRead> {
    let r: FlipRead = serde_json::from_slice(body).context("hw.fps reply is not JSON")?;
    if r.state.is_empty() {
        anyhow::bail!("hw.fps reply has no state");
    }
    Ok(r)
}

/// One read of the helper. An older helper is `state: "unsupported"`, not an error.
pub fn read_flips(addr: &str) -> Result<FlipRead> {
    match mgmt::call_with(addr, m::HW_FPS, m::HW_FPS.label, b"", Some(READ_TIMEOUT)) {
        Ok(body) => parse_flip_read(&body),
        Err(e) if is_unsupported(&e) => Ok(FlipRead {
            state: "unsupported".into(),
            ..FlipRead::default()
        }),
        Err(e) => Err(e),
    }
}

/// A rate from two reads: the frames between them over the time between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FpsSample {
    pub fps: f64,
    /// Frames counted in the interval.
    pub frames: u64,
    /// The interval on the console's clock, in microseconds.
    pub interval_us: u64,
}

/// Turns consecutive reads into rates. Keeps only the previous good read.
#[derive(Debug, Default, Clone)]
pub struct FpsMeter {
    last: Option<(u64, u64)>, // (flips, mono_us)
}

impl FpsMeter {
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// The rate since the previous read, or `None`: no game, the first read, a read too soon
    /// after the last (kept for later), a counter that went back (a new game) or a gap.
    pub fn push(&mut self, r: &FlipRead) -> Option<FpsSample> {
        if !r.is_ok() {
            self.last = None;
            return None;
        }
        let Some((flips0, at0)) = self.last else {
            self.last = Some((r.flips, r.mono_us));
            return None;
        };
        if r.mono_us <= at0 || r.flips < flips0 {
            // the clock or the count went back: a restarted helper or a new game
            self.last = Some((r.flips, r.mono_us));
            return None;
        }
        let dt = r.mono_us - at0;
        if dt < MIN_INTERVAL_US {
            return None;
        }
        self.last = Some((r.flips, r.mono_us));
        if dt > MAX_INTERVAL_US {
            return None;
        }
        let frames = r.flips - flips0;
        let fps = frames as f64 * 1_000_000.0 / dt as f64;
        if fps > FPS_MAX {
            return None;
        }
        Some(FpsSample {
            fps,
            frames,
            interval_us: dt,
        })
    }
}

/// One row of a capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureSample {
    /// Wall-clock time of the sample on this computer, Unix milliseconds.
    pub t_ms: i64,
    /// Milliseconds since the capture started.
    pub elapsed_ms: i64,
    pub fps: f64,
    pub frames: u64,
    pub interval_ms: f64,
    /// The console's temperatures, read every few seconds during a capture; `None` = not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_temp_c: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soc_temp_c: Option<i32>,
}

/// A capture's summary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CaptureStats {
    pub samples: usize,
    /// Seconds the samples cover (the sum of their intervals).
    pub seconds: f64,
    pub min_fps: f64,
    pub max_fps: f64,
    /// All frames over all the time sampled: a long slow stretch weighs as long as it lasted.
    pub avg_fps: f64,
    /// The average of the slowest 1% of samples (at least one): seconds, not frames.
    pub low_1pct_fps: f64,
}

pub fn capture_stats(samples: &[CaptureSample]) -> CaptureStats {
    if samples.is_empty() {
        return CaptureStats::default();
    }
    let frames: u64 = samples.iter().map(|s| s.frames).sum();
    let ms: f64 = samples.iter().map(|s| s.interval_ms).sum();
    let mut sorted: Vec<f64> = samples.iter().map(|s| s.fps).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n_low = (sorted.len() / 100).max(1);
    let low = sorted[..n_low].iter().sum::<f64>() / n_low as f64;
    CaptureStats {
        samples: samples.len(),
        seconds: ms / 1000.0,
        min_fps: sorted[0],
        max_fps: sorted[sorted.len() - 1],
        avg_fps: if ms > 0.0 {
            frames as f64 * 1000.0 / ms
        } else {
            0.0
        },
        low_1pct_fps: low,
    }
}

/// `2026-11-09T12:34:56.789Z` from Unix milliseconds (UTC; no date library needed).
pub fn iso_utc_ms(t_ms: i64) -> String {
    let secs = t_ms.div_euclid(1000);
    let ms = t_ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// The capture as CSV: one row per sample, a header first. Temperatures are empty when not read.
pub fn capture_csv(samples: &[CaptureSample]) -> String {
    let mut out = String::from(
        "timestamp_utc,elapsed_s,fps_sampled,frames,interval_ms,cpu_temp_c,soc_temp_c\n",
    );
    let opt = |v: Option<i32>| v.map(|v| v.to_string()).unwrap_or_default();
    for s in samples {
        out.push_str(&format!(
            "{},{:.3},{:.2},{},{:.1},{},{}\n",
            iso_utc_ms(s.t_ms),
            s.elapsed_ms as f64 / 1000.0,
            s.fps,
            s.frames,
            s.interval_ms,
            opt(s.cpu_temp_c),
            opt(s.soc_temp_c),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The literals payload/tests/game_fps_selftest.c checks game_fps_json() against.
    const OK_BODY: &str =
        "{\"ok\":true,\"state\":\"ok\",\"flips\":123456,\"mono_us\":5000000,\"bytes\":128}";
    const NO_GAME_BODY: &str =
        "{\"ok\":true,\"state\":\"no_game\",\"mono_us\":7000000,\"errno\":2}";

    fn ok(flips: u64, mono_us: u64) -> FlipRead {
        FlipRead {
            state: "ok".into(),
            flips,
            mono_us,
            ..FlipRead::default()
        }
    }

    #[test]
    fn the_helpers_reply_parses() {
        let r = parse_flip_read(OK_BODY.as_bytes()).unwrap();
        assert_eq!(
            r,
            FlipRead {
                state: "ok".into(),
                flips: 123_456,
                mono_us: 5_000_000,
                bytes: 128,
                errno: 0
            }
        );
        let r = parse_flip_read(NO_GAME_BODY.as_bytes()).unwrap();
        assert_eq!((r.state.as_str(), r.errno, r.flips), ("no_game", 2, 0));
        assert!(parse_flip_read(b"{}").is_err());
        assert!(parse_flip_read(b"nope").is_err());
    }

    #[test]
    fn two_reads_a_second_apart_are_the_frames_in_that_second() {
        let mut m = FpsMeter::default();
        assert_eq!(m.push(&ok(1000, 1_000_000)), None);
        let s = m.push(&ok(1060, 2_000_000)).unwrap();
        assert_eq!((s.frames, s.interval_us), (60, 1_000_000));
        assert!((s.fps - 60.0).abs() < 1e-9);
        // 1.25 s with 50 frames is 40 FPS
        let s = m.push(&ok(1110, 3_250_000)).unwrap();
        assert!((s.fps - 40.0).abs() < 1e-9);
    }

    #[test]
    fn a_read_too_soon_waits_for_a_longer_interval() {
        let mut m = FpsMeter::default();
        m.push(&ok(0, 1_000_000));
        assert_eq!(m.push(&ok(10, 1_200_000)), None);
        // measured from the first read, not the early one
        let s = m.push(&ok(60, 2_000_000)).unwrap();
        assert_eq!(s.frames, 60);
    }

    #[test]
    fn no_game_a_new_game_a_gap_and_a_jump_start_over() {
        let mut m = FpsMeter::default();
        m.push(&ok(500, 1_000_000));
        m.push(&FlipRead {
            state: "no_game".into(),
            ..FlipRead::default()
        });
        assert_eq!(m.push(&ok(560, 2_000_000)), None, "no game resets");
        assert_eq!(m.push(&ok(10, 3_000_000)), None, "count went back");
        assert!(m.push(&ok(70, 4_000_000)).is_some());
        assert_eq!(m.push(&ok(700, 20_000_000)), None, "a 16 s gap");
        assert_eq!(m.push(&ok(1700, 21_000_000)), None, "1000 FPS is a jump");
        assert!(m.push(&ok(1760, 22_000_000)).is_some());
    }

    fn sample(fps: f64, secs: f64) -> CaptureSample {
        CaptureSample {
            t_ms: 0,
            elapsed_ms: 0,
            fps,
            frames: (fps * secs).round() as u64,
            interval_ms: secs * 1000.0,
            cpu_temp_c: None,
            soc_temp_c: None,
        }
    }

    #[test]
    fn stats_weigh_time_and_the_low_is_the_slowest_one_percent() {
        assert_eq!(capture_stats(&[]), CaptureStats::default());
        let mut v: Vec<CaptureSample> = (0..198).map(|_| sample(60.0, 1.0)).collect();
        v.push(sample(30.0, 1.0));
        v.push(sample(20.0, 1.0));
        let s = capture_stats(&v);
        assert_eq!(s.samples, 200);
        assert_eq!((s.min_fps, s.max_fps), (20.0, 60.0));
        assert!((s.low_1pct_fps - 25.0).abs() < 1e-9, "{}", s.low_1pct_fps);
        assert!((s.seconds - 200.0).abs() < 1e-9);
        // a 2-second sample at 30 counts twice as long as a 1-second one at 60
        let s = capture_stats(&[sample(60.0, 1.0), sample(30.0, 2.0)]);
        assert!((s.avg_fps - 40.0).abs() < 1e-9);
        assert_eq!(s.low_1pct_fps, 30.0);
    }

    #[test]
    fn utc_timestamps() {
        assert_eq!(iso_utc_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_utc_ms(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(iso_utc_ms(1_793_000_000_000), "2026-10-26T07:33:20.000Z");
    }

    #[test]
    fn csv_has_a_header_and_empty_temperatures_when_not_read() {
        let mut a = sample(59.94, 1.0);
        a.t_ms = 1_793_000_000_000;
        a.elapsed_ms = 1500;
        a.frames = 60;
        a.interval_ms = 1001.0;
        let mut b = a.clone();
        b.cpu_temp_c = Some(61);
        b.soc_temp_c = Some(55);
        let csv = capture_csv(&[a, b]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "timestamp_utc,elapsed_s,fps_sampled,frames,interval_ms,cpu_temp_c,soc_temp_c"
        );
        assert_eq!(lines[1], "2026-10-26T07:33:20.000Z,1.500,59.94,60,1001.0,,");
        assert_eq!(
            lines[2],
            "2026-10-26T07:33:20.000Z,1.500,59.94,60,1001.0,61,55"
        );
        assert_eq!(lines.len(), 3);
    }
}
