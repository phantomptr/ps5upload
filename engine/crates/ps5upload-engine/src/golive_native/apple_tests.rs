//! AudioToolbox for real, on this Mac (the iPhone app's code, built for macOS): real Opus from
//! ffmpeg decoded and re-encoded to AAC, and a whole native run published to an ffmpeg RTMP
//! listener and checked with ffprobe. They need ffmpeg and ffprobe (libx264, libx265, libopus)
//! on PATH, set the global session and use ports 19360-19361, so they are `#[ignore]`d. No sound
//! is played: everything goes to files. Run with
//! `cargo test -p ps5upload-engine --lib golive_native -- --ignored --nocapture --test-threads 1`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ps5upload_core::rp_stream::{Codec, StreamConfig};

use super::super::audio::{AudioCodecs, AAC_FRAME, OPUS_FRAME};
use super::AudioToolbox;
use crate::rp_stream_api::live_feed::tests::{encode, tmp_dir, Media};
use crate::rp_stream_api::live_feed::TestSession;

#[test]
#[ignore = "needs ffmpeg for the Opus"]
fn decodes_real_opus_and_encodes_aac() {
    let dir = tmp_dir("golive-at");
    let m = encode(&dir, false, 120, &[0]);
    let (mut dec, mut enc) = AudioToolbox.open(2, 160).expect("AudioToolbox opens");
    let mut pcm = Vec::new();
    for p in &m.opus[..100] {
        dec.decode(p, &mut pcm).expect("decodes");
    }
    // 100 packets of 10 ms, stereo (the decoder may hold a packet back).
    let frames = pcm.len() / 2;
    assert!(
        (98 * OPUS_FRAME..=100 * OPUS_FRAME).contains(&frames),
        "{frames} frames"
    );
    // A 440 Hz sine: loud, and crossing zero about 880 times a second (left channel).
    let left: Vec<i16> = pcm.iter().step_by(2).copied().collect();
    let tail = &left[left.len() / 2..];
    let rms = (tail.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
    assert!(rms > 1000.0, "rms {rms}");
    let crossings = tail.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    let per_s = crossings as f64 / (tail.len() as f64 / 48_000.0);
    assert!(
        (per_s - 880.0).abs() < 60.0,
        "{per_s} zero crossings a second"
    );

    let mut aac = Vec::new();
    for c in pcm.chunks(960) {
        enc.encode(c, &mut aac).expect("encodes");
    }
    let expect = frames / AAC_FRAME;
    assert!(
        aac.len() + 3 >= expect && aac.len() <= expect,
        "{} AAC frames for {frames} samples",
        aac.len()
    );
    assert!(aac.iter().all(|f| !f.is_empty() && f.len() < 2048));
    assert!(enc.delay() > 0, "AudioToolbox reports its priming");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Plays the console in real time, looping `m`, until `stop`.
fn play(s: &TestSession, m: Arc<Media>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let s = s.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let (mut v, mut a) = (0usize, 0usize);
        while !stop.load(Ordering::Relaxed) {
            let vt = Duration::from_micros(v as u64 * 16_667);
            let at = Duration::from_micros(a as u64 * 10_000);
            let next = vt.min(at);
            let el = t0.elapsed();
            if next > el {
                std::thread::sleep(next - el);
            }
            if vt <= at {
                let n = v % m.aus.len();
                s.feed()
                    .video(&m.aus[n], v as u16, n.is_multiple_of(120), Instant::now());
                v += 1;
            } else {
                let n = a % m.opus.len();
                let mut fr = (a as u16).to_be_bytes().to_vec();
                fr.extend_from_slice(&m.opus[n]);
                s.feed().audio(&fr, Instant::now());
                a += 1;
            }
        }
    })
}

/// (video codec, audio codec, seconds) of an FLV, by ffprobe.
fn probe(p: &Path) -> (String, String, f64) {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
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
    let dur = v["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    (codec("video"), codec("audio"), dur)
}

/// Decode errors ffmpeg finds decoding all of `p` (to nothing).
fn decode_errors(p: &Path) -> Vec<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(p)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

async fn through_ffmpeg(hevc: bool, port: u16) {
    let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let dir = tmp_dir(if hevc {
        "golive-at-hevc"
    } else {
        "golive-at-h264"
    });
    let m = Arc::new(encode(&dir, hevc, 240, &[0, 120]));
    let out = dir.join("got.flv");
    let key = "testkey-native";
    let mut listener = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-listen", "1", "-i"])
        .arg(format!("rtmp://127.0.0.1:{port}/live/{key}"))
        .args(["-c", "copy", "-f", "flv"])
        .arg(&out)
        .stdin(Stdio::null())
        .spawn()
        .expect("ffmpeg on PATH");
    std::thread::sleep(Duration::from_millis(800));
    let s = TestSession::start(
        "10.0.88.2",
        StreamConfig {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_kbps: 6000,
            codec: if hevc { Codec::H265 } else { Codec::H264 },
            ..StreamConfig::default()
        },
    );
    s.feed().video_header(&m.header);
    let halt = Arc::new(AtomicBool::new(false));
    let player = play(&s, m.clone(), halt.clone());
    let dest = super::super::super::Dest {
        id: "local".into(),
        label: "Local".into(),
        preset: crate::golive_dest::preset("custom").unwrap(),
        out: crate::golive_ffmpeg::Output {
            url: format!("rtmp://127.0.0.1:{port}/live/{key}"),
            format: "flv",
        },
        secrets: vec![key.into()],
        bandwidth_test: false,
    };
    let r = super::super::super::start_run(
        vec![dest],
        160,
        super::super::super::Pipeline::Native(Arc::new(AudioToolbox)),
    )
    .unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    while r.status()["live_s"].as_u64() < Some(6) {
        assert!(Instant::now() < end, "never live: {:#}", r.status());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    r.request_stop();
    let end = Instant::now() + Duration::from_secs(10);
    while !matches!(r.status()["phase"].as_str(), Some("ended" | "failed")) {
        assert!(Instant::now() < end, "never ended: {:#}", r.status());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    halt.store(true, Ordering::Relaxed);
    player.join().unwrap();
    s.end();
    // The listener ends with the publisher's unpublish.
    let end = Instant::now() + Duration::from_secs(10);
    while listener.try_wait().unwrap().is_none() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = listener.kill();
    let _ = listener.wait();
    let (v, a, dur) = probe(&out);
    assert_eq!(v, if hevc { "hevc" } else { "h264" });
    assert_eq!(a, "aac");
    assert!(dur > 5.0, "{dur} s");
    let errors = decode_errors(&out);
    assert!(errors.is_empty(), "decodes cleanly: {errors:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs ffmpeg/ffprobe and sets the global session"]
async fn goes_live_in_h264_to_an_ffmpeg_server_with_apples_aac() {
    through_ffmpeg(false, 19360).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs ffmpeg/ffprobe (libx265) and sets the global session"]
async fn goes_live_in_hevc_over_enhanced_rtmp_to_an_ffmpeg_server() {
    through_ffmpeg(true, 19361).await;
}
