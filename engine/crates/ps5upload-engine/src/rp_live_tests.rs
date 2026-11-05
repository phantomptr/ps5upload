//! Tests for the live feed (rp_live.rs) and its muxer (rp_live_ts.rs).
//!
//! The `#[ignore]`d ones need ffmpeg and ffprobe (with libx264, libx265 and libopus) on PATH:
//! they encode real H.264/HEVC and Opus, push it through the feed as the console would (jitter,
//! a frame-index wrap, a lost frame, keyframes only when the feed asks for them), and check the
//! result with ffprobe and a full decode. Run them with
//! `cargo test -p ps5upload-engine --lib live_feed -- --ignored --nocapture --test-threads 1`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ps5upload_core::rp_stream::{Codec, StreamConfig, StreamSink};
use tokio::sync::mpsc;

use super::*;

const KEY: &[u8] = &[0, 0, 0, 1, 0x65, 1, 2]; // H.264 IDR slice
const DELTA: &[u8] = &[0, 0, 0, 1, 0x41, 1, 2]; // H.264 non-IDR slice

fn feed() -> LiveFeed {
    let f = LiveFeed::new(false);
    f.set_config(StreamConfig::default());
    f.video_header(&[0, 0, 0, 1, 0x67, 9, 0, 0, 0, 1, 0x68, 9]);
    f
}

fn drain(rx: &mut mpsc::Receiver<FeedEvent>) -> Vec<String> {
    let mut v = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        v.push(match ev {
            FeedEvent::Video {
                index, key, header, ..
            } => format!(
                "v{index}{}{}",
                if key { "K" } else { "" },
                if header.is_some() { "H" } else { "" }
            ),
            FeedEvent::Audio { index, .. } => format!("a{index}"),
        });
    }
    v
}

#[test]
fn a_reader_starts_at_a_keyframe_and_gets_sound_from_then_on() {
    let f = feed();
    let t = Instant::now();
    let (_, mut rx, stats) = f.add("test".into(), false).unwrap();
    assert!(
        f.wants_keyframe(t),
        "a keyframe is asked for when a reader joins"
    );
    f.audio(&[0, 1, 0xfc], t);
    f.video(DELTA, 1, false, t);
    assert!(drain(&mut rx).is_empty());
    f.video(KEY, 2, true, t);
    f.audio(&[0, 2, 0xfc], t);
    f.video(DELTA, 3, false, t);
    assert_eq!(drain(&mut rx), ["v2KH", "a2", "v3"]);
    // Waiting for the first keyframe is not a loss.
    assert_eq!(stats.held_after_loss.load(Ordering::Relaxed), 0);
}

#[test]
fn after_a_loss_frames_wait_for_the_next_keyframe_which_is_asked_for() {
    let f = feed();
    let t = Instant::now();
    let (_, mut rx, stats) = f.add("test".into(), false).unwrap();
    f.video(KEY, 10, true, t);
    f.video(DELTA, 11, false, t);
    assert!(f.wants_keyframe(t)); // the join's ask
    assert!(!f.wants_keyframe(t));
    // The stream thread gave up on 12.
    f.lost();
    f.video(DELTA, 13, false, t);
    f.video(DELTA, 14, false, t);
    assert!(f.wants_keyframe(t), "a keyframe is asked for at once");
    // Sound goes on meanwhile.
    f.audio(&[0, 7, 0xfc], t);
    f.video(KEY, 15, true, t);
    f.video(DELTA, 16, false, t);
    assert_eq!(drain(&mut rx), ["v10KH", "v11", "a7", "v15KH", "v16"]);
    assert_eq!(stats.held_after_loss.load(Ordering::Relaxed), 2);
    assert_eq!(f.status()["frames_held_after_loss"], 2);
}

#[test]
fn a_skipped_index_is_a_loss_but_the_wrap_is_not() {
    let f = feed();
    let t = Instant::now();
    let (_, mut rx, stats) = f.add("test".into(), false).unwrap();
    f.video(KEY, 65534, true, t);
    f.video(DELTA, 65535, false, t);
    f.video(DELTA, 0, false, t);
    f.video(DELTA, 2, false, t); // 1 never came
    f.video(KEY, 3, true, t);
    assert_eq!(drain(&mut rx), ["v65534KH", "v65535", "v0", "v3KH"]);
    assert_eq!(stats.held_after_loss.load(Ordering::Relaxed), 1);
}

#[test]
fn a_slow_reader_skips_to_a_keyframe_and_others_are_not_held_up() {
    let f = feed();
    let t = Instant::now();
    let (_, mut fast, _) = f.add("fast".into(), false).unwrap();
    let (_, mut slow, slow_stats) = f.add("slow".into(), false).unwrap();
    f.video(KEY, 0, true, t);
    for i in 1..=READER_QUEUE as u16 {
        f.video(DELTA, i, false, t);
        if i % 10 == 0 {
            drain(&mut fast);
        }
    }
    drain(&mut fast);
    assert_eq!(slow_stats.dropped_slow.load(Ordering::Relaxed), 1);
    assert!(f.wants_keyframe(t));
    // The slow one catches up: it gets nothing more until the keyframe, the fast one all.
    let n = drain(&mut slow).len();
    assert_eq!(n, READER_QUEUE);
    let next = READER_QUEUE as u16 + 1;
    f.video(DELTA, next, false, t);
    f.video(KEY, next + 1, true, t);
    assert_eq!(drain(&mut slow), [format!("v{}KH", next + 1)]);
    assert_eq!(drain(&mut fast).len(), 2);
}

#[test]
fn a_stuck_reader_asks_for_one_keyframe_not_one_per_frame() {
    let f = feed();
    let t = Instant::now();
    let (_, _stuck, _) = f.add("stuck".into(), false).unwrap(); // never read
    assert!(f.wants_keyframe(t)); // joined
    f.video(KEY, 0, true, t);
    for i in 1..=READER_QUEUE as u16 {
        f.video(DELTA, i, false, t);
    }
    assert!(f.wants_keyframe(t), "fell behind: one keyframe asked");
    // Still stuck: frames and the next keyframes go by without asking again (the periodic
    // keyframe, 2 s apart, is all the console sends).
    let mut i = READER_QUEUE as u16 + 1;
    for k in 0..3 {
        for _ in 0..30 {
            f.video(DELTA, i, false, t);
            i += 1;
        }
        f.video(KEY, i, true, t);
        i += 1;
        assert!(!f.wants_keyframe(t), "asked again on round {k}");
    }
}

#[test]
fn keyframes_every_two_seconds_only_while_someone_reads() {
    let f = feed();
    let t = Instant::now();
    f.video(KEY, 0, true, t);
    assert!(!f.wants_keyframe(t + Duration::from_secs(10)), "no reader");
    let (id, _rx, _) = f.add("test".into(), false).unwrap();
    assert!(f.wants_keyframe(t)); // joined
    f.video(KEY, 1, true, t);
    // Asked for 1 s early: the console takes about a second to answer, so keyframes land
    // every 2 s (see the_periodic_keyframe_is_asked_for_early_and_once).
    assert!(!f.wants_keyframe(t + Duration::from_millis(900)));
    assert!(f.wants_keyframe(t + KEYFRAME_EVERY - Duration::from_millis(1000)));
    f.remove(id);
    assert!(!f.wants_keyframe(t + Duration::from_secs(10)));
}

#[test]
fn the_session_ending_ends_every_readers_stream() {
    let f = feed();
    let (_, mut a, _) = f.add("a".into(), false).unwrap();
    let (_, mut b, _) = f.add("b".into(), false).unwrap();
    assert_eq!(f.status()["consumers"], 2);
    f.close();
    assert_eq!(f.readers(), 0);
    assert!(matches!(
        a.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
    assert!(matches!(
        b.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn the_status_says_what_a_reader_gets() {
    let f = LiveFeed::new(true);
    f.set_config(StreamConfig {
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate_kbps: 6000,
        codec: Codec::H265,
        ..StreamConfig::default()
    });
    let v = f.status();
    assert_eq!(v["codec"], "h265");
    assert_eq!(v["width"], 1920);
    assert_eq!(v["height"], 1080);
    assert_eq!(v["fps"], 60);
    assert_eq!(v["bitrate_kbps"], 6000);
    assert_eq!(v["consumers"], 0);
    assert!(v["since_keyframe_s"].is_null());
}

#[test]
fn readers_are_capped() {
    let f = feed();
    let rx: Vec<_> = (0..MAX_READERS)
        .map(|i| f.add(format!("{i}"), false).unwrap())
        .collect();
    assert!(f.add("one too many".into(), false).is_none());
    // Go live still gets in when outside readers fill the cap.
    assert!(f.add("go-live".into(), true).is_some());
    drop(rx);
}

// ---- With ffmpeg: real codec data through the feed and muxer, checked by ffprobe ----

pub(crate) fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ps5upload-rp-live-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub(crate) fn run(cmd: &mut Command) -> std::process::Output {
    let out = cmd.output().expect("ffmpeg/ffprobe on PATH");
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Annex-B NAL units (without start codes).
fn nals(b: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| {
            let mut e = starts.get(n + 1).map_or(b.len(), |&x| x - 3);
            while e > s && b[e - 1] == 0 {
                e -= 1;
            }
            &b[s..e]
        })
        .collect()
}

pub(crate) struct Media {
    /// Access units as the console sends them: no delimiter, no parameter sets.
    pub aus: Vec<Vec<u8>>,
    /// The parameter sets, as STREAMINFO carries them.
    pub header: Vec<u8>,
    pub opus: Vec<Vec<u8>>,
}

/// Encodes `frames` frames at 60 fps with keyframes exactly at `keys`, and 10 ms Opus.
pub(crate) fn encode(dir: &Path, hevc: bool, frames: usize, keys: &[usize]) -> Media {
    let es = dir.join(if hevc { "v.hevc" } else { "v.h264" });
    let times: Vec<String> = keys
        .iter()
        .map(|&i| format!("{:.4}", (i as f64 / 60.0 - 0.002).max(0.0)))
        .collect();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!(
            "testsrc2=size=1280x720:rate=60:duration={}",
            frames as f64 / 60.0
        ))
        .args(["-frames:v", &frames.to_string(), "-bf", "0", "-g", "100000"])
        .args(["-force_key_frames", &times.join(","), "-forced-idr", "1"]);
    if hevc {
        cmd.args(["-c:v", "libx265", "-preset", "ultrafast"])
            .args(["-x265-params", "aud=1:scenecut=0:bframes=0:log-level=error"]);
    } else {
        cmd.args(["-c:v", "libx264", "-preset", "veryfast"])
            .args(["-x264-params", "aud=1:scenecut=0"]);
    }
    cmd.args(["-f", if hevc { "hevc" } else { "h264" }])
        .arg(&es);
    run(&mut cmd);
    let data = std::fs::read(&es).unwrap();
    let t = |n: &[u8]| {
        if hevc {
            (n[0] >> 1) & 0x3f
        } else {
            n[0] & 0x1f
        }
    };
    let (aud, params): (u8, &[u8]) = if hevc {
        (35, &[32, 33, 34])
    } else {
        (9, &[7, 8])
    };
    let mut aus: Vec<Vec<u8>> = Vec::new();
    let mut header = Vec::new();
    let mut header_done = false;
    for n in nals(&data) {
        if n.is_empty() {
            continue;
        }
        let ty = t(n);
        if ty == aud {
            if !aus.last().is_some_and(|a| a.is_empty()) {
                aus.push(Vec::new());
            }
            if !header.is_empty() {
                header_done = true;
            }
            continue;
        }
        if params.contains(&ty) {
            if !header_done {
                header.extend_from_slice(&[0, 0, 0, 1]);
                header.extend_from_slice(n);
            }
            continue;
        }
        let au = aus.last_mut().unwrap();
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(n);
    }
    aus.retain(|a| !a.is_empty());
    assert_eq!(aus.len(), frames, "access units");

    let ogg = dir.join("a.ogg");
    run(Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!(
            "sine=frequency=440:sample_rate=48000:duration={}",
            frames as f64 / 60.0 + 1.0
        ))
        .args([
            "-ac",
            "2",
            "-c:a",
            "libopus",
            "-b:a",
            "96k",
            "-frame_duration",
            "10",
        ])
        .args(["-f", "ogg"])
        .arg(&ogg));
    Media {
        aus,
        header,
        opus: ogg_packets(&std::fs::read(&ogg).unwrap())
            .into_iter()
            .skip(2) // OpusHead, OpusTags
            .collect(),
    }
}

fn ogg_packets(b: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut i = 0;
    while i + 27 <= b.len() && &b[i..i + 4] == b"OggS" {
        let nseg = b[i + 26] as usize;
        let segs = &b[i + 27..i + 27 + nseg];
        let mut at = i + 27 + nseg;
        for &s in segs {
            cur.extend_from_slice(&b[at..at + s as usize]);
            at += s as usize;
            if s < 255 {
                out.push(std::mem::take(&mut cur));
            }
        }
        i = at;
    }
    out
}

/// A small deterministic jitter in 0..max µs.
fn jitter(n: usize, max: u64) -> u64 {
    let x = (n as u64).wrapping_mul(2_654_435_761) ^ 0x9e37_79b9;
    (x >> 7) % max
}

const FRAMES: usize = 720; // 12 s at 60 fps
const LOSS_AT: usize = 400;
const VIDEO_INDEX0: u16 = 65000; // wraps at frame 536
const AUDIO_INDEX0: u16 = 65300; // wraps at frame 236

/// What a reader got: the TS, which video frames (by number) went out, where keyframes fell.
struct Run {
    ts: Vec<u8>,
    video_sent: Vec<usize>,
    keys: Vec<usize>,
    held_after_loss: u64,
}

/// Plays the console: frames at 60 fps (with jitter, a burst now and then, one lost frame),
/// audio every 10 ms (with jitter and some out of order); a keyframe two frames after the
/// stream asks for one, at most every 500 ms (as run_stream does). Without `media`, it only
/// works out when the keyframes fall.
fn drive(hevc: bool, media: Option<&Media>) -> Run {
    let f = LiveFeed::new(hevc);
    f.set_config(StreamConfig::default());
    if let Some(m) = media {
        f.video_header(&m.header);
    }
    let (id, mut rx, stats) = f.add("test".into(), false).unwrap();
    let mut mux = ReaderMux::new(&f, id, stats.clone());
    let t0 = Instant::now();
    let mut events: Vec<(Duration, bool, usize)> = Vec::new(); // (arrival, video?, n)
    for i in 0..FRAMES {
        let mut at = i as u64 * 16_667 + jitter(i, 8_000);
        if i % 100 >= 96 {
            // A burst: these frames come late, together.
            at = (i - i % 100 + 99) as u64 * 16_667 + 8_000;
        }
        events.push((Duration::from_micros(at), true, i));
    }
    for j in 0..FRAMES * 10 / 6 {
        let mut at = j as u64 * 10_000 + jitter(j + 7, 4_000);
        if j % 50 == 10 {
            at += 15_000; // comes after the next one (FEC)
        }
        events.push((Duration::from_micros(at), false, j));
    }
    events.sort_by_key(|e| (e.0, e.1, e.2));
    let mut pending_key: Option<usize> = Some(0);
    let mut last_req: Option<Duration> = None;
    let mut keys = Vec::new();
    let mut run = Run {
        ts: Vec::new(),
        video_sent: Vec::new(),
        keys: Vec::new(),
        held_after_loss: 0,
    };
    for (at, video, n) in events {
        let now = t0 + at;
        if video {
            let mut ask = f.wants_keyframe(now);
            if n == LOSS_AT {
                f.lost();
                ask = true; // run_stream asks on a loss too
            }
            if ask && last_req.is_none_or(|r| at - r > Duration::from_millis(500)) {
                last_req = Some(at);
                pending_key.get_or_insert(n + 2);
            }
            let key = pending_key == Some(n);
            if key {
                pending_key = None;
                keys.push(n);
            }
            if n == LOSS_AT {
                continue; // never arrives
            }
            let au: &[u8] = match media {
                Some(m) => &m.aus[n],
                None if hevc && key => &[0, 0, 0, 1, 0x26, 1, 2],
                None if hevc => &[0, 0, 0, 1, 0x02, 1, 2],
                None if key => KEY,
                None => DELTA,
            };
            let real_key = super::super::is_keyframe(au, hevc);
            assert_eq!(
                real_key, key,
                "frame {n}: the encoder put keyframes elsewhere"
            );
            f.video(au, VIDEO_INDEX0.wrapping_add(n as u16), key, now);
        } else {
            let mut fr = AUDIO_INDEX0.wrapping_add(n as u16).to_be_bytes().to_vec();
            match media {
                Some(m) => fr.extend_from_slice(&m.opus[n]),
                None => fr.push(0xfc),
            }
            f.audio(&fr, now);
        }
        while let Ok(ev) = rx.try_recv() {
            match &ev {
                FeedEvent::Video { index, .. } => run
                    .video_sent
                    .push(index.wrapping_sub(VIDEO_INDEX0) as usize),
                FeedEvent::Audio { .. } => {}
            }
            let out = mux.push(ev);
            run.ts.extend_from_slice(&out);
        }
    }
    run.keys = keys;
    run.held_after_loss = stats.held_after_loss.load(Ordering::Relaxed);
    run
}

#[derive(Debug)]
struct Pkt {
    video: bool,
    pts: i64,
    key: bool,
}

/// ffprobe's streams and packets of a TS file: (video codec, audio codec, channels, packets).
fn probe(ts: &Path) -> (String, String, i64, Vec<Pkt>) {
    let out = run(Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_packets",
            "-of",
            "json",
        ])
        .arg(ts));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let streams = v["streams"].as_array().unwrap();
    let find = |t: &str| {
        streams
            .iter()
            .find(|s| s["codec_type"] == t)
            .unwrap_or_else(|| panic!("no {t} stream: {streams:?}"))
            .clone()
    };
    let vs = find("video");
    let a = find("audio");
    let pk = v["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| Pkt {
            video: p["codec_type"] == "video",
            pts: p["pts"].as_i64().unwrap(),
            key: p["flags"].as_str().unwrap_or("").starts_with('K'),
        })
        .collect();
    (
        vs["codec_name"].as_str().unwrap().into(),
        a["codec_name"].as_str().unwrap().into(),
        a["channels"].as_i64().unwrap_or(0),
        pk,
    )
}

/// Decodes everything; returns ffmpeg's error lines (none expected).
fn decode_errors(ts: &Path) -> Vec<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(ts)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

fn check_file(ts: &Path, hevc: bool) -> Vec<Pkt> {
    let (vc, ac, ch, pk) = probe(ts);
    assert_eq!(vc, if hevc { "hevc" } else { "h264" });
    assert_eq!(ac, "opus");
    assert_eq!(ch, 2);
    for video in [true, false] {
        let p: Vec<i64> = pk
            .iter()
            .filter(|p| p.video == video)
            .map(|p| p.pts)
            .collect();
        assert!(p.len() > 10);
        assert!(
            p.windows(2).all(|w| w[1] > w[0]),
            "{} PTS not increasing",
            if video { "video" } else { "audio" }
        );
    }
    let errs = decode_errors(ts);
    eprintln!("{}: decode errors: {}", ts.display(), errs.len());
    for e in errs.iter().take(10) {
        eprintln!("  {e}");
    }
    assert!(errs.is_empty(), "decode errors");
    pk
}

fn full_run(hevc: bool) {
    let name = if hevc { "hevc" } else { "h264" };
    let dir = tmp_dir(name);
    // First where the keyframes fall (they depend only on the feed's asks), then the real data
    // encoded with keyframes just there, then the real run.
    let plan = drive(hevc, None);
    let media = encode(&dir, hevc, FRAMES, &plan.keys);
    let r = drive(hevc, Some(&media));
    assert_eq!(r.keys, plan.keys);
    let ts = dir.join("out.ts");
    std::fs::write(&ts, &r.ts).unwrap();
    let pk = check_file(&ts, hevc);

    let vids: Vec<&Pkt> = pk.iter().filter(|p| p.video).collect();
    let auds: Vec<&Pkt> = pk.iter().filter(|p| !p.video).collect();
    assert_eq!(vids.len(), r.video_sent.len());
    // Keyframes: where the feed asked, every ~2 s plus the one after the loss.
    let key_s: Vec<f64> = vids
        .iter()
        .filter(|p| p.key)
        .map(|p| (p.pts - vids[0].pts) as f64 / 90_000.0)
        .collect();
    let gaps: Vec<f64> = key_s.windows(2).map(|w| w[1] - w[0]).collect();
    eprintln!("{name}: keyframes at {key_s:.3?} s, gaps {gaps:.3?}");
    assert!(gaps.iter().all(|&g| g <= 2.2), "keyframe gap over 2.2 s");
    assert!(key_s.len() >= 6);
    // Frames held after the loss: none of LOSS_AT+1 .. the next keyframe went out.
    let next_key = *r.keys.iter().find(|&&k| k > LOSS_AT).unwrap();
    assert!(!r.video_sent.iter().any(|&n| n > LOSS_AT && n < next_key));
    assert_eq!(r.held_after_loss as usize, next_key - LOSS_AT - 1);
    eprintln!(
        "{name}: frame {LOSS_AT} lost, {} held until keyframe {next_key}",
        r.held_after_loss
    );
    // Start: audio within one frame of video.
    let start = auds[0].pts - vids[0].pts;
    eprintln!("{name}: first audio PTS - first video PTS = {start} ticks");
    assert!(start.abs() <= 1500, "audio starts {start} ticks off");
    // Sync all along: each stream's PTS minus its content time stays within a frame of the
    // other's.
    let off_v: Vec<i64> = vids
        .iter()
        .zip(&r.video_sent)
        .map(|(p, &n)| p.pts - (n as i64 * 1500))
        .collect();
    let mut sv = off_v.clone();
    sv.sort();
    let mv = sv[sv.len() / 2];
    // Audio frame numbers: the audio packets are in order with no gaps after the first, so
    // number them from where their first PTS puts them.
    let a0 = ((auds[0].pts - (mv)) as f64 / 900.0).round() as i64;
    let off_a: Vec<i64> = auds
        .iter()
        .enumerate()
        .map(|(k, p)| p.pts - (a0 + k as i64) * 900)
        .collect();
    let mut sa = off_a.clone();
    sa.sort();
    let ma = sa[sa.len() / 2];
    let worst_v = off_v.iter().map(|o| (o - mv).abs()).max().unwrap();
    let worst_a = off_a.iter().map(|o| (o - ma).abs()).max().unwrap();
    eprintln!(
        "{name}: {} video / {} audio packets; video PTS vs frame index: median {mv}, worst {worst_v} ticks; audio worst {worst_a} ticks; A/V offset {} ticks",
        vids.len(),
        auds.len(),
        ma - mv
    );
    assert!((ma - mv).abs() <= 1500, "A/V apart by {} ticks", ma - mv);
    assert!(worst_a <= 1500, "audio wanders {worst_a} ticks");
}

#[test]
#[ignore = "needs ffmpeg/ffprobe with libx264 and libopus"]
fn ffmpeg_reads_h264_and_opus_from_the_feed() {
    full_run(false);
}

#[test]
#[ignore = "needs ffmpeg/ffprobe with libx265 and libopus"]
fn ffmpeg_reads_hevc_and_opus_from_the_feed() {
    full_run(true);
}

/// The real endpoint, against a fake session fed in real time through the stream's own sink:
/// two ffmpeg readers at once over HTTP, the status endpoint, and 503 when nothing runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs ffmpeg/ffprobe with libx264 and libopus; sets the global session"]
async fn two_players_read_the_endpoint_over_http() {
    let dir = tmp_dir("http");
    let frames = 360; // 6 s
    let keys: Vec<usize> = (0..frames).step_by(120).collect();
    let media = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || encode(&dir, false, frames, &keys)
    })
    .await
    .unwrap();

    let app = axum::Router::new()
        .route("/api/ps5/rp/live.ts", axum::routing::get(live_ts))
        .route("/api/ps5/rp/live/status", axum::routing::get(status));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://127.0.0.1:{port}/api/ps5/rp");
    let get = |url: String| async move {
        tokio::task::spawn_blocking(move || match ureq::get(&url).call() {
            Ok(mut r) => (200u16, r.body_mut().read_to_string().unwrap_or_default()),
            Err(ureq::Error::StatusCode(c)) => (c, String::new()),
            Err(e) => panic!("{e}"),
        })
        .await
        .unwrap()
    };

    // Nothing runs yet: 503 at once.
    assert_eq!(get(format!("{base}/live.ts")).await.0, 503);
    let (_, s) = get(format!("{base}/live/status")).await;
    assert!(s.contains("\"running\":false"), "{s}");

    // A session (no console: frames come from the test, through the real Sink).
    let live = Arc::new(super::super::Live::new(
        "10.0.0.9".into(),
        false,
        Duration::from_secs(60),
    ));
    live.feed.set_config(StreamConfig {
        width: 1280,
        height: 720,
        ..StreamConfig::default()
    });
    *super::super::LIVE.lock().unwrap() = Some(live.clone());
    // Another console named: 503.
    assert_eq!(get(format!("{base}/live.ts?addr=10.0.0.8")).await.0, 503);

    let readers: Vec<_> = (0..2)
        .map(|k| {
            let out = dir.join(format!("http{k}.ts"));
            let url = format!("{base}/live.ts?addr=10.0.0.9");
            std::process::Command::new("ffmpeg")
                .args([
                    "-v", "error", "-y", "-i", &url, "-c", "copy", "-f", "mpegts",
                ])
                .arg(&out)
                .spawn()
                .map(|c| (c, out))
                .unwrap()
        })
        .collect();
    // Wait for both to connect.
    let t = Instant::now();
    while live.feed.readers() < 2 {
        assert!(t.elapsed() < Duration::from_secs(10), "readers never came");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let l = live.clone();
    let feeder = tokio::task::spawn_blocking(move || {
        let mut sink = super::super::Sink {
            live: l.clone(),
            t0: Instant::now(),
        };
        sink.video(&media.header, true);
        sink.audio(&[2, 16, 0, 0, 0xbb, 0x80, 0, 0, 1, 0xe0, 0, 0, 0, 0], true);
        let start = Instant::now();
        let mut asked = 0;
        let mut j = 0;
        for (i, au) in media.aus.iter().enumerate() {
            let due = start + Duration::from_micros(i as u64 * 16_667);
            while j < media.opus.len() && Duration::from_micros(j as u64 * 10_000) <= due - start {
                let mut fr = (j as u16).to_be_bytes().to_vec();
                fr.extend_from_slice(&media.opus[j]);
                sink.audio(&fr, false);
                j += 1;
            }
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            if sink.wants_keyframe() {
                asked += 1;
            }
            sink.video_frame(au, i as u16);
        }
        asked
    });
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (_, s) = get(format!("{base}/live/status")).await;
    eprintln!("status mid-stream: {s}");
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["consumers"], 2);
    assert_eq!(v["codec"], "h264");
    assert_eq!(v["width"], 1280);
    assert!(v["since_keyframe_s"].as_f64().unwrap() < 2.1);
    let asked = feeder.await.unwrap();
    eprintln!("keyframe asks seen by the stream thread: {asked}");
    // The session ends: the readers' streams end, and ffmpeg finishes its files.
    live.feed.close();
    *super::super::LIVE.lock().unwrap() = None;
    for (mut c, out) in readers {
        let st = tokio::task::spawn_blocking(move || c.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(st.success());
        let pk = check_file(&out, false);
        let keys = pk.iter().filter(|p| p.video && p.key).count();
        eprintln!(
            "{}: {} video packets, {} keyframes, {} audio packets",
            out.display(),
            pk.iter().filter(|p| p.video).count(),
            keys,
            pk.iter().filter(|p| !p.video).count()
        );
        assert!(pk[0].video || pk.iter().find(|p| p.video).unwrap().key);
        assert!(
            pk.iter().find(|p| p.video).unwrap().key,
            "starts at a keyframe"
        );
    }
}

/// The periodic request goes out a second early (the console takes about that long to answer)
/// and is not repeated while one is in flight, so keyframes land about 2 s apart instead of 3
/// with an extra one in between (measured on a PS5 Pro before the change: gaps 3.05, 1.0, 3.08).
#[test]
fn the_periodic_keyframe_is_asked_for_early_and_once() {
    let f = feed();
    let t = Instant::now();
    let (_, _rx, _stats) = f.add("test".into(), false).unwrap();
    assert!(f.wants_keyframe(t), "the join's ask");
    f.video(KEY, 1, true, t);
    assert!(!f.wants_keyframe(t + Duration::from_millis(900)));
    assert!(
        f.wants_keyframe(t + Duration::from_millis(1000)),
        "asked 1 s early"
    );
    assert!(
        !f.wants_keyframe(t + Duration::from_millis(1500)),
        "not again while in flight"
    );
    assert!(!f.wants_keyframe(t + Duration::from_millis(2400)));
    assert!(
        f.wants_keyframe(t + Duration::from_millis(2600)),
        "asked again if none came"
    );
    f.video(KEY, 2, true, t + Duration::from_millis(2700));
    assert!(!f.wants_keyframe(t + Duration::from_millis(3000)));
}
