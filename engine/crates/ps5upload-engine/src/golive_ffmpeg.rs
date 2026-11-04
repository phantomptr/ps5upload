//! Go live's ffmpeg (#635, rp_golive.rs): its command line, what it reports, and when to start
//! it again. Plain functions, so each is tested without ffmpeg.
//!
//! One ffmpeg reads the live feed's MPEG-TS on stdin and sends it on: video copied as the
//! console made it, Opus decoded and encoded to AAC (no site takes Opus). One destination is a
//! plain FLV output; several go through the tee muxer with `onfail=ignore`, so one site
//! failing leaves the others streaming. `-progress pipe:1` reports frames, fps, bitrate and
//! speed once a second.

use std::time::{Duration, Instant};

/// One output: its URL (with the key in it) and its muxer ("flv", or "mpegts" for SRT).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Output {
    pub url: String,
    pub format: &'static str,
}

/// The arguments for ffmpeg (the program itself excluded). `hevc`: the session's video is
/// HEVC (else H.264).
pub(crate) fn args(outputs: &[Output], audio_kbps: u32, hevc: bool) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "level+warning",
        "-nostats",
        "-stats_period",
        "1",
        "-progress",
        "pipe:1",
        // The feed starts at a keyframe with its parameter sets: a short look is enough.
        "-analyzeduration",
        "1500000",
        "-probesize",
        "8000000",
        "-f",
        "mpegts",
        "-i",
        "pipe:0",
        "-map",
        "0:v:0",
        "-map",
        "0:a:0?",
        "-c:v",
        "copy",
        "-c:a",
        "aac",
        "-ar",
        "48000",
        "-ac",
        "2",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push("-b:a".into());
    a.push(format!("{}k", audio_kbps.clamp(64, 320)));
    a.extend(
        ["-max_muxing_queue_size", "1024"]
            .iter()
            .map(|s| s.to_string()),
    );
    match outputs {
        [one] => {
            a.extend(["-f".to_string(), one.format.to_string()]);
            if one.format == "flv" {
                a.extend(["-flvflags".to_string(), "no_duration_filesize".to_string()]);
            }
            a.push(one.url.clone());
        }
        many => {
            // The tee muxer opens its outputs itself: the AAC encoder must put its config in
            // the stream header for FLV.
            a.extend(["-flags:a".to_string(), "+global_header".to_string()]);
            // …and it hands each output the input's codec tag, MPEG-TS's stream type (0x1B),
            // which FLV refuses ("Tag [27][0][0][0] incompatible", ffmpeg 9). FLV's own tag is
            // 7 for H.264 and 'hvc1' (big-endian) for HEVC; MPEG-TS outputs ignore the tag.
            a.extend([
                "-tag:v".to_string(),
                if hevc { "0x68766331" } else { "7" }.to_string(),
            ]);
            a.extend(["-f".to_string(), "tee".to_string()]);
            a.push(tee_spec(many));
        }
    }
    a
}

/// `[f=flv:onfail=ignore:flvflags=no_duration_filesize]url|…`. URLs never hold `|`, `[`, `]`,
/// `\` or `'` (golive_dest.rs refuses them), so nothing needs escaping.
pub(crate) fn tee_spec(outputs: &[Output]) -> String {
    outputs
        .iter()
        .map(|o| {
            let extra = if o.format == "flv" {
                ":flvflags=no_duration_filesize"
            } else {
                ""
            };
            format!("[f={}:onfail=ignore{extra}]{}", o.format, o.url)
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// `text` with every secret replaced by "••••" (longest first, so a key inside a URL goes too).
pub(crate) fn redact(text: &str, secrets: &[String]) -> String {
    let mut s: Vec<&String> = secrets.iter().filter(|x| x.len() >= 4).collect();
    s.sort_by_key(|x| std::cmp::Reverse(x.len()));
    let mut out = text.to_string();
    for k in s {
        if out.contains(k.as_str()) {
            out = out.replace(k.as_str(), "••••");
        }
    }
    out
}

/// One `-progress` report.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub(crate) struct Progress {
    pub frame: u64,
    pub fps: f64,
    /// ffmpeg's output bitrate (absent for tee), kbit/s.
    pub bitrate_kbps: Option<f64>,
    /// Bytes ffmpeg has written out (absent for tee).
    pub total_size: u64,
    pub out_time_s: f64,
    pub drop_frames: u64,
    pub dup_frames: u64,
    pub speed: Option<f64>,
    /// "continue" or "end".
    pub ended: bool,
}

impl Progress {
    /// The stream is going out. Frames say so on the build we ship, but ffmpeg 6.1 reports `frame=0`
    /// for `-c copy` while it sends, and `PS5UPLOAD_FFMPEG` can name any ffmpeg: bytes and bitrate
    /// count too. Tee reports neither, and frames there.
    pub(crate) fn flowing(&self) -> bool {
        self.out_time_s > 0.0
            && (self.frame > 0 || self.total_size > 0 || self.bitrate_kbps.is_some_and(|k| k > 0.0))
    }
}

/// Collects `key=value` lines into reports: `push` gives one at each `progress=` line.
#[derive(Default, Debug)]
pub(crate) struct ProgressParser {
    cur: Progress,
}

impl ProgressParser {
    pub(crate) fn push(&mut self, line: &str) -> Option<Progress> {
        let (k, v) = line.trim().split_once('=')?;
        let v = v.trim();
        match k {
            "frame" => self.cur.frame = v.parse().unwrap_or(self.cur.frame),
            "fps" => self.cur.fps = v.parse().unwrap_or(0.0),
            "bitrate" => {
                self.cur.bitrate_kbps = v.trim_end_matches("kbits/s").trim().parse().ok();
            }
            "total_size" => self.cur.total_size = v.parse().unwrap_or(0),
            "out_time_us" | "out_time_ms" => {
                // Both are µs (out_time_ms is misnamed in ffmpeg).
                if let Ok(us) = v.parse::<i64>() {
                    self.cur.out_time_s = us.max(0) as f64 / 1e6;
                }
            }
            "drop_frames" => self.cur.drop_frames = v.parse().unwrap_or(0),
            "dup_frames" => self.cur.dup_frames = v.parse().unwrap_or(0),
            "speed" => self.cur.speed = v.trim_end_matches('x').trim().parse().ok(),
            "progress" => {
                self.cur.ended = v == "end";
                return Some(self.cur.clone());
            }
            _ => {}
        }
        None
    }
}

/// Why ffmpeg (or one of its outputs) stopped, from what it printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Failure {
    /// The server could not be reached (name, refused, timed out, no route).
    Unreachable,
    /// The secure connection failed: mostly ffmpeg finding no trusted roots to check the site's
    /// certificate against, which no amount of "check your internet" fixes.
    Tls,
    /// The site took the connection and then refused the stream: almost always the key.
    Rejected,
    /// The site does not take this video codec.
    Codec,
    /// The connection dropped while streaming.
    Dropped,
    /// Anything else.
    Other,
}

impl Failure {
    pub(crate) fn message(self, site: &str) -> String {
        match self {
            Failure::Unreachable => format!(
                "Can't reach {site}'s server. Check this computer's internet connection (and the server address)."
            ),
            Failure::Rejected => format!(
                "{site} refused the stream. That is almost always the stream key: copy it again from {site} (keys change when you reset them)."
            ),
            Failure::Codec => format!(
                "{site} doesn't take this video format. Restart Play in H.264 and go live again."
            ),
            Failure::Dropped => format!("The connection to {site} dropped."),
            Failure::Other => format!("Streaming to {site} stopped."),
            Failure::Tls => format!(
                "Can't make a secure connection to {site}: its certificate could not be checked. Set SSL_CERT_FILE to this computer's certificate bundle, or check the server address."
            ),
        }
    }

    /// The same when several sites stopped together.
    pub(crate) fn message_many(self) -> String {
        match self {
            Failure::Unreachable => "Can't reach the streaming sites' servers. Check this computer's internet connection (and the server addresses).".into(),
            Failure::Rejected => "The streaming sites refused the stream. That is almost always the stream key: copy each one again from its site (keys change when you reset them).".into(),
            Failure::Codec => "The streaming sites don't take this video format. Restart Play in H.264 and go live again.".into(),
            Failure::Dropped => "The connection to the streaming sites dropped.".into(),
            Failure::Other => "Streaming to the sites stopped.".into(),
            Failure::Tls => "Can't make a secure connection to the streaming sites: their certificates could not be checked. Set SSL_CERT_FILE to this computer's certificate bundle.".into(),
        }
    }
}

/// What one line of ffmpeg's log says went wrong, if anything.
pub(crate) fn classify(line: &str) -> Option<Failure> {
    let l = line.to_ascii_lowercase();
    let any = |pats: &[&str]| pats.iter().any(|p| l.contains(p));
    if any(&[
        "codec not currently supported in container",
        "not compatible with flv",
        "unsupported codec",
        "could not find tag for codec",
        "video codec hevc not compatible",
    ]) {
        return Some(Failure::Codec);
    }
    if any(&["certificate", "tls handshake"]) {
        return Some(Failure::Tls);
    }
    if any(&[
        "connection refused",
        "connection timed out",
        "operation timed out",
        "network is unreachable",
        "no route to host",
        "failed to resolve",
        "name or service not known",
        "nodename nor servname",
        "temporary failure in name resolution",
        "host is unreachable",
        "cannot open connection",
        "error in the pull function",
        "connection to tcp",
    ]) {
        return Some(Failure::Unreachable);
    }
    if any(&[
        "authentication",
        "unauthorized",
        "forbidden",
        "badname",
        "publish.badname",
        "connect.rejected",
        "netstream.publish.denied",
        "server error",
        "invalid stream key",
    ]) {
        return Some(Failure::Rejected);
    }
    if any(&[
        "broken pipe",
        "connection reset",
        "end of file",
        "error in the push function",
        "error writing trailer",
        "input/output error",
        "error number -10053",
        "error number -10054",
    ]) {
        return Some(Failure::Dropped);
    }
    None
}

/// The tee muxer's word that output `n` (0-based) failed: "Slave muxer #n failed: …".
pub(crate) fn failed_output(line: &str) -> Option<usize> {
    let i = line.find("Slave muxer #")?;
    let rest = &line[i + "Slave muxer #".len()..];
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !rest[n.len()..].trim_start().starts_with("failed") {
        return None;
    }
    n.parse().ok()
}

/// The tee muxer's word that output `url…` could not be opened: "Slave '…': error opening: …".
/// Returns the slave spec's text (redact it before showing).
pub(crate) fn failed_open(line: &str) -> Option<&str> {
    let i = line.find("Slave '")?;
    let rest = &line[i + "Slave '".len()..];
    let end = rest.find("': error")?;
    Some(&rest[..end])
}

/// "Right after a start": less than this much sent…
pub(crate) const EARLY_OUT_TIME_S: f64 = 8.0;
/// …in less than this long.
pub(crate) const EARLY_RUN: Duration = Duration::from_secs(15);

/// What a failure that came this soon after the start most likely was. A site that refuses a
/// key mostly just closes the connection (a "broken pipe" a few seconds in), which reads like
/// a dropped connection; one that drops within the first seconds, before any output time,
/// was refused.
pub(crate) fn settle(f: Failure, out_time_s: f64, ran: Duration) -> Failure {
    match f {
        Failure::Dropped | Failure::Other if out_time_s < EARLY_OUT_TIME_S && ran < EARLY_RUN => {
            Failure::Rejected
        }
        f => f,
    }
}

/// What to do after ffmpeg stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Next {
    Retry(Duration),
    GiveUp(Failure),
}

/// Reconnect backoff: 2, 4, 8, 15, 30 s, then every 30 s; reset by a run of a minute or more.
/// A codec the site does not take is final; a key refused twice in a row is final; anything
/// else gives up after failing for 10 minutes straight.
#[derive(Debug, Default)]
pub(crate) struct Backoff {
    failures: u32,
    rejected: u32,
    failing_since: Option<Instant>,
}

pub(crate) const DELAYS: [u64; 5] = [2, 4, 8, 15, 30];
const GOOD_RUN: Duration = Duration::from_secs(60);
const GIVE_UP_AFTER: Duration = Duration::from_secs(600);

impl Backoff {
    pub(crate) fn after(&mut self, f: Failure, ran: Duration, now: Instant) -> Next {
        if ran >= GOOD_RUN {
            self.failures = 0;
            self.rejected = 0;
            self.failing_since = None;
        }
        let since = *self.failing_since.get_or_insert(now);
        if f == Failure::Codec {
            return Next::GiveUp(f);
        }
        if f == Failure::Rejected {
            self.rejected += 1;
            if self.rejected >= 2 {
                return Next::GiveUp(f);
            }
        } else {
            self.rejected = 0;
        }
        if now.saturating_duration_since(since) >= GIVE_UP_AFTER {
            return Next::GiveUp(f);
        }
        let d = DELAYS[(self.failures as usize).min(DELAYS.len() - 1)];
        self.failures += 1;
        Next::Retry(Duration::from_secs(d))
    }

    #[cfg(test)]
    pub(crate) fn attempts(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
#[path = "golive_ffmpeg_tests.rs"]
mod tests;
