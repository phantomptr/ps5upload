//! Go live end to end, against local RTMP listeners (ffmpeg `-listen 1`): real H.264 and Opus
//! through the live feed, the engine's ffmpeg, RTMP, and back out of the listener's FLV, checked
//! with ffprobe. Also: reconnecting after the listener goes away, one of two sites failing
//! while the other keeps going, and an unreachable server.
//!
//! They need ffmpeg and ffprobe (libx264, libopus) on PATH, set the global session and run,
//! and use ports 19350-19352, so they are `#[ignore]`d. No sound is played: everything goes to
//! files. Run with
//! `cargo test -p ps5upload-engine --lib rp_golive -- --ignored --nocapture --test-threads 1`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ps5upload_core::rp_stream::StreamConfig;

use super::*;
use crate::rp_stream_api::live_feed::tests::{encode, tmp_dir, Media};
use crate::rp_stream_api::live_feed::TestSession;

const FRAMES: usize = 240; // 4 s at 60 fps, looped
const KEYS: [usize; 2] = [0, 120]; // a keyframe every 2 s

fn ffmpeg() -> FfmpegCmd {
    FfmpegCmd {
        program: PathBuf::from("ffmpeg"),
        lib_dir: None,
    }
}

/// A local RTMP server that takes one stream and writes it to `out` (no sound is played).
fn listener(port: u16, key: &str, out: &Path) -> Child {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-listen", "1", "-i"])
        .arg(format!("rtmp://127.0.0.1:{port}/live/{key}"))
        .args(["-c", "copy", "-f", "flv"])
        .arg(out)
        .stdin(Stdio::null())
        .spawn()
        .expect("ffmpeg on PATH")
}

/// Plays the console in real time, looping `m`, until `stop`.
fn play(s: &TestSession, m: Arc<Media>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let s = s.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut v = 0usize;
        let mut a = 0usize;
        while !stop.load(Ordering::Relaxed) {
            let vt = Duration::from_micros(v as u64 * 16_667);
            let at = Duration::from_micros(a as u64 * 10_000);
            let next = vt.min(at);
            let el = t0.elapsed();
            if next > el {
                std::thread::sleep(next - el);
            }
            let feed = s.feed();
            if vt <= at {
                let n = v % FRAMES;
                feed.video(&m.aus[n], v as u16, KEYS.contains(&n), Instant::now());
                v += 1;
            } else {
                let n = a % m.opus.len().min(FRAMES * 10 / 6);
                let mut fr = (a as u16).to_be_bytes().to_vec();
                fr.extend_from_slice(&m.opus[n]);
                feed.audio(&fr, Instant::now());
                a += 1;
            }
        }
    })
}

/// (video codec, audio codec, seconds of video) of an FLV, by ffprobe. The length is from the
/// packets: a server killed mid-stream leaves a file without its duration.
fn probe(p: &Path) -> (String, String, f64) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(p)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let codec = |t: &str| {
        v["streams"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["codec_type"] == t))
            .and_then(|s| s["codec_name"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let times = packet_times(p);
    let dur = match (times.first(), times.last()) {
        (Some(a), Some(b)) => b - a,
        _ => 0.0,
    };
    (codec("video"), codec("audio"), dur)
}

fn packet_times(p: &Path) -> Vec<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
        .collect()
}

/// Video keyframe times of a file (seconds).
fn keyframes(p: &Path) -> Vec<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains('K'))
        .filter_map(|l| l.split(',').next()?.parse().ok())
        .collect()
}

fn decode_errors(p: &Path) -> usize {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(p)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

fn dest(port: u16, key: &str, name: &str) -> Dest {
    Dest {
        id: format!("{name}-{port}"),
        label: name.into(),
        preset: golive_dest::preset("custom").unwrap(),
        out: Output {
            url: format!("rtmp://127.0.0.1:{port}/live/{key}"),
            format: "flv",
        },
        secrets: vec![key.to_string()],
        bandwidth_test: false,
    }
}

async fn wait_for(r: &Run, secs: u64, what: &str, f: impl Fn(&serde_json::Value) -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let s = r.status();
        if f(&s) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {s:#}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn media(dir: &Path) -> Arc<Media> {
    Arc::new(encode(dir, false, FRAMES, &KEYS))
}

fn session() -> TestSession {
    TestSession::start(
        "10.0.0.9",
        StreamConfig {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_kbps: 6000,
            ..StreamConfig::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs ffmpeg/ffprobe with libx264 and libopus; local RTMP on 19350"]
async fn goes_live_to_a_local_rtmp_server_and_reconnects() {
    let dir = tmp_dir("golive");
    let m = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || media(&dir)
    })
    .await
    .unwrap();
    let s = session();
    s.feed().video_header(&m.header);
    let stop = Arc::new(AtomicBool::new(false));
    let player = play(&s, m.clone(), stop.clone());

    let first = dir.join("first.flv");
    let mut l1 = listener(19350, "testkey", &first);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = start_run(vec![dest(19350, "testkey", "Local")], 160, ffmpeg());
    wait_for(&r, 20, "live", |s| s["phase"] == "live").await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    let st = r.status();
    eprintln!("live: {st:#}");
    assert_eq!(st["destinations"][0]["state"], "live");
    assert!(st["kbps"].as_f64().unwrap() > 50.0, "{st}");
    assert!(st["fps"].as_f64().unwrap() > 30.0, "{st}");
    // The key never shows.
    assert!(!st.to_string().contains("testkey"), "{st}");

    // The server goes away mid-stream (killed by its PID).
    l1.kill().unwrap();
    let _ = l1.wait();
    wait_for(&r, 20, "reconnecting", |s| s["phase"] == "reconnecting").await;
    let st = r.status();
    eprintln!("after the server went: {st:#}");
    assert!(st["reconnects"].as_u64().unwrap() >= 1);
    // It comes back: the stream carries on to it, the session never stopped.
    let second = dir.join("second.flv");
    let mut l2 = listener(19350, "testkey", &second);
    wait_for(&r, 60, "live again", |s| s["phase"] == "live").await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    r.request_stop();
    wait_for(&r, 15, "ended", |s| s["phase"] == "ended").await;
    let _ = tokio::task::spawn_blocking(move || l2.wait()).await;
    stop.store(true, Ordering::Relaxed);
    s.end();
    let _ = player.join();

    for (f, killed) in [(&first, true), (&second, false)] {
        let (vc, ac, dur) = probe(f);
        let keys = keyframes(f);
        let gaps: Vec<f64> = keys.windows(2).map(|w| w[1] - w[0]).collect();
        let errs = decode_errors(f);
        eprintln!(
            "{}: {vc} + {ac}, {dur:.1} s, keyframes every {gaps:.2?} s, {errs} decode errors",
            f.display()
        );
        assert_eq!(vc, "h264");
        assert_eq!(ac, "aac");
        assert!(dur > 4.0, "{dur}");
        assert!(gaps.iter().all(|&g| g <= 2.1), "{gaps:?}");
        // The killed server's file may end in a cut-off frame; nothing else may be broken.
        assert!(errs <= if killed { 4 } else { 0 }, "{errs} decode errors");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs ffmpeg/ffprobe with libx264 and libopus; local RTMP on 19351-19352"]
async fn one_site_failing_leaves_the_other_streaming() {
    let dir = tmp_dir("golive-tee");
    let m = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || media(&dir)
    })
    .await
    .unwrap();
    let s = session();
    s.feed().video_header(&m.header);
    let stop = Arc::new(AtomicBool::new(false));
    let player = play(&s, m.clone(), stop.clone());

    let a = dir.join("a.flv");
    let b = dir.join("b.flv");
    let mut la = listener(19351, "keyA", &a);
    let mut lb = listener(19352, "keyB", &b);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = start_run(
        vec![dest(19351, "keyA", "A"), dest(19352, "keyB", "B")],
        128,
        ffmpeg(),
    );
    wait_for(&r, 20, "live", |s| {
        s["phase"] == "live"
            && s["destinations"][0]["state"] == "live"
            && s["destinations"][1]["state"] == "live"
    })
    .await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    la.kill().unwrap();
    let _ = la.wait();
    wait_for(&r, 20, "A out", |s| s["destinations"][0]["state"] != "live").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let st = r.status();
    eprintln!("A gone: {st:#}");
    assert_eq!(st["phase"], "live");
    assert_eq!(st["destinations"][1]["state"], "live");
    assert!(!st.to_string().contains("keyA"));
    r.request_stop();
    wait_for(&r, 15, "ended", |s| s["phase"] == "ended").await;
    let _ = tokio::task::spawn_blocking(move || lb.wait()).await;
    stop.store(true, Ordering::Relaxed);
    s.end();
    let _ = player.join();
    let (vc, ac, dur_a) = probe(&a);
    assert_eq!((vc.as_str(), ac.as_str()), ("h264", "aac"));
    let (vc, ac, dur_b) = probe(&b);
    assert_eq!((vc.as_str(), ac.as_str()), ("h264", "aac"));
    eprintln!("A {dur_a:.1} s, B {dur_b:.1} s");
    assert!(dur_b > dur_a + 3.0, "B kept going after A stopped");
    assert_eq!(decode_errors(&b), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs ffmpeg with libx264 and libopus"]
async fn an_unreachable_server_is_said_and_retried() {
    let dir = tmp_dir("golive-unreach");
    let m = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || media(&dir)
    })
    .await
    .unwrap();
    let s = session();
    s.feed().video_header(&m.header);
    let stop = Arc::new(AtomicBool::new(false));
    let player = play(&s, m.clone(), stop.clone());
    // Nothing listens on port 9 here.
    let r = start_run(vec![dest(9, "nokey123", "Nowhere")], 160, ffmpeg());
    wait_for(&r, 20, "reconnecting", |s| s["phase"] == "reconnecting").await;
    let st = r.status();
    eprintln!("{st:#}");
    assert_eq!(st["error"]["code"], "unreachable");
    assert!(st["retry_in_s"].as_u64().is_some());
    r.request_stop();
    wait_for(&r, 15, "ended", |s| s["phase"] == "ended").await;
    stop.store(true, Ordering::Relaxed);
    s.end();
    let _ = player.join();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "sets the global session"]
async fn waits_for_play_and_gives_up_with_a_reason() {
    // No session at all: it waits (the user restarting Play in H.264), and says so.
    let r = start_run(vec![dest(9, "nokey123", "X")], 160, ffmpeg());
    wait_for(&r, 5, "waiting", |s| s["phase"] == "waiting_for_play").await;
    r.request_stop();
    wait_for(&r, 5, "ended", |s| s["phase"] == "ended").await;
    // An HEVC session and an H.264-only site: refused with the reason, nothing started.
    let s = TestSession::start(
        "10.0.0.9",
        StreamConfig {
            codec: ps5upload_core::rp_stream::Codec::H265,
            ..StreamConfig::default()
        },
    );
    let mut d = dest(9, "nokey123", "Twitch");
    d.preset = golive_dest::preset("twitch").unwrap();
    let r = start_run(vec![d], 160, ffmpeg());
    wait_for(&r, 5, "failed", |s| s["phase"] == "failed").await;
    let st = r.status();
    assert_eq!(st["error"]["code"], "codec_not_allowed");
    assert!(st["error"]["message"].as_str().unwrap().contains("H.264"));
    s.end();
}
