//! The native run end to end inside the engine: a pretend Remote Play session (frames put into
//! the live feed as the stream thread puts them), the pretend sound codecs, and in-process RTMP
//! servers (test_server.rs). Nothing leaves this computer and no sound is played.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ps5upload_core::rp_stream::StreamConfig;

use super::super::{start_run_with, Dest, Pipeline, Run, Timing};
use super::audio::fake::FakeCodecs;
use super::test_server::{self, Got, Opts, Server};
use crate::golive_dest;
use crate::golive_ffmpeg::Output;
use crate::rp_stream_api::live_feed::TestSession;

/// Stand-ins for keys (never real ones).
const KEY_A: &str = "test-name-aaaa";
const KEY_B: &str = "test-name-bbbb";

const SPS: &[u8] = &[
    0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50, 0x05, 0xbb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
    0x10, 0x00, 0x00, 0x03, 0x03, 0xc0, 0xf1, 0x83, 0x19, 0x60,
];
const PPS: &[u8] = &[0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];

fn annexb(nals: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in nals {
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(n);
    }
    v
}

fn quick() -> Timing {
    Timing {
        stop_grace: Duration::from_millis(1500),
        rejoin_after: Duration::from_millis(600),
        connect_stall_after: Duration::from_secs(5),
        ..Timing::default()
    }
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

fn session() -> TestSession {
    let s = TestSession::start(
        "10.0.88.1",
        StreamConfig {
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_kbps: 6000,
            ..StreamConfig::default()
        },
    );
    s.feed().video_header(&annexb(&[SPS, PPS]));
    s
}

/// Plays a pretend console in real time: 60 fps (a keyframe every 2 s, no parameter sets in
/// the frames: they come from the header) and 10 ms Opus packets, until `stop`.
fn play(s: &TestSession, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let s = s.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let (mut v, mut a) = (0u32, 0u32);
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            let vt = Duration::from_micros(v as u64 * 16_667);
            let at = Duration::from_micros(a as u64 * 10_000);
            let next = vt.min(at);
            let el = t0.elapsed();
            if next > el {
                std::thread::sleep(next - el);
            }
            if vt <= at {
                let key = v.is_multiple_of(120);
                let slice: &[u8] = if key {
                    &[0x65, 0x88, 1, 2, 3]
                } else {
                    &[0x41, 0x9a, 4, 5]
                };
                s.feed()
                    .video(&annexb(&[slice]), v as u16, key, Instant::now());
                v += 1;
            } else {
                let mut fr = (a as u16).to_be_bytes().to_vec();
                fr.extend_from_slice(&[(a % 200) as u8 + 1, 0x42]);
                s.feed().audio(&fr, Instant::now());
                a += 1;
            }
        }
    })
}

async fn wait_for(r: &Run, secs: u64, what: &str, f: impl Fn(&serde_json::Value) -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        let s = r.status();
        if f(&s) {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {s:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn native() -> Pipeline {
    Pipeline::Native(Arc::new(FakeCodecs { fail_open: false }))
}

/// What one publisher's media says: (type, first two body bytes, timestamp).
fn shape(g: &Got) -> Vec<(u8, [u8; 2], u32)> {
    g.media
        .iter()
        .map(|m| (m.typ, [m.body[0], *m.body.get(1).unwrap_or(&0)], m.ts))
        .collect()
}

fn check_flv_order(g: &Got) {
    let s = shape(g);
    assert!(s.len() > 20, "{} messages", s.len());
    assert_eq!(s[0].0, 18, "onMetaData first");
    assert_eq!(
        (s[1].0, s[1].1),
        (9, [0x17, 0x00]),
        "the AVC sequence header"
    );
    assert_eq!(
        (s[2].0, s[2].1),
        (8, [0xaf, 0x00]),
        "the AAC sequence header"
    );
    assert_eq!(
        (s[3].0, s[3].1, s[3].2),
        (9, [0x17, 0x01], 0),
        "a keyframe at 0"
    );
    // The AVC record carries the session's SPS.
    let cfg = &g.media[1].body;
    assert_eq!(&cfg[5..9], &[1, 0x64, 0x00, 0x1f]);
    assert_eq!(&g.media[2].body[2..], &[0x11, 0x90], "AAC-LC 48 kHz stereo");
    // Frames: AVCC (length-prefixed) slices.
    let k = &g.media[3].body;
    assert_eq!(&k[5..9], &[0, 0, 0, 5]);
    assert_eq!(k[9], 0x65);
    // Sound and picture both, each in time order.
    let audio: Vec<u32> = s
        .iter()
        .filter(|m| m.0 == 8 && m.1[1] == 1)
        .map(|m| m.2)
        .collect();
    let video: Vec<u32> = s
        .iter()
        .filter(|m| m.0 == 9 && m.1[1] == 1)
        .map(|m| m.2)
        .collect();
    assert!(
        audio.len() > 5 && video.len() > 10,
        "{} audio, {} video",
        audio.len(),
        video.len()
    );
    assert!(
        audio.windows(2).all(|w| w[1] >= w[0]),
        "audio times go forward"
    );
    assert!(
        video.windows(2).all(|w| w[1] > w[0]),
        "video times go forward"
    );
    // ~16.7 ms apart at 60 fps.
    let step = (video[video.len() - 1] - video[0]) as f64 / (video.len() - 1) as f64;
    assert!((step - 16.7).abs() < 2.0, "video step {step} ms");
}

async fn stop(r: &Run) {
    r.request_stop();
    wait_for(r, 10, "ended", |v| {
        v["phase"] == "ended" || v["phase"] == "failed"
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn goes_live_without_ffmpeg_and_unpublishes_on_stop() {
    let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let srv = test_server::start(Opts::default(), None).await;
    let s = session();
    let halt = Arc::new(AtomicBool::new(false));
    let player = play(&s, halt.clone());
    let r = start_run_with(vec![dest(srv.port, KEY_A, "A")], 160, native(), quick()).unwrap();
    wait_for(&r, 10, "live", |v| {
        v["phase"] == "live" && v["destinations"][0]["state"] == "live"
    })
    .await;
    assert!(
        srv.wait(Duration::from_secs(5), |g| g
            .first()
            .is_some_and(|g| g.media.len() > 120))
            .await
    );
    wait_for(&r, 5, "progress", |v| {
        v["frames"].as_u64() > Some(30) && v["kbps"].as_f64() > Some(0.0)
    })
    .await;
    let st = r.status();
    assert!(st["fps"].as_f64().unwrap() > 30.0, "{st:#}");
    stop(&r).await;
    assert_eq!(r.status()["phase"], "ended");
    halt.store(true, std::sync::atomic::Ordering::Relaxed);
    player.join().unwrap();
    s.end();
    let g = &srv.got()[0];
    assert_eq!((g.app.as_str(), g.stream.as_str()), ("live", KEY_A));
    assert!(g.unpublished, "a stop unpublishes");
    check_flv_order(g);
    // The key is never in what the run shows.
    assert!(!r.status().to_string().contains(KEY_A));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_key_gets_one_more_try_then_ends_with_the_reason() {
    let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let srv = test_server::start(
        Opts {
            reject_publish: true,
            ..Opts::default()
        },
        None,
    )
    .await;
    let s = session();
    let halt = Arc::new(AtomicBool::new(false));
    let player = play(&s, halt.clone());
    let r = start_run_with(vec![dest(srv.port, KEY_A, "A")], 160, native(), quick()).unwrap();
    wait_for(&r, 15, "failed", |v| v["phase"] == "failed").await;
    let st = r.status();
    assert_eq!(st["error"]["code"], "rejected", "{st:#}");
    assert_eq!(st["reconnects"], 1);
    assert_eq!(srv.got().len(), 2, "two tries");
    assert!(!st.to_string().contains(KEY_A));
    halt.store(true, std::sync::atomic::Ordering::Relaxed);
    player.join().unwrap();
    s.end();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_site_refusing_leaves_the_other_streaming() {
    let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let good = test_server::start(Opts::default(), None).await;
    let bad = test_server::start(
        Opts {
            reject_publish: true,
            ..Opts::default()
        },
        None,
    )
    .await;
    let s = session();
    let halt = Arc::new(AtomicBool::new(false));
    let player = play(&s, halt.clone());
    let r = start_run_with(
        vec![dest(good.port, KEY_A, "A"), dest(bad.port, KEY_B, "B")],
        160,
        native(),
        quick(),
    )
    .unwrap();
    wait_for(&r, 10, "A live, B out", |v| {
        v["destinations"][0]["state"] == "live" && v["destinations"][1]["state"] == "failed"
    })
    .await;
    assert_eq!(r.status()["destinations"][1]["error"]["code"], "rejected");
    // A keeps going, and was never restarted for B.
    let n = good.got()[0].media.len();
    assert!(
        good.wait(Duration::from_secs(5), |g| g[0].media.len() > n + 60)
            .await
    );
    assert_eq!(good.got().len(), 1);
    assert_eq!(r.status()["phase"], "live");
    stop(&r).await;
    halt.store(true, std::sync::atomic::Ordering::Relaxed);
    player.join().unwrap();
    s.end();
    check_flv_order(&good.got()[0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_site_that_drops_mid_stream_is_let_back_in_alone() {
    let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let steady = test_server::start(Opts::default(), None).await;
    // Hangs up after a few seconds of media, every time.
    let flaky = test_server::start(
        Opts {
            close_after: Some(400),
            ..Opts::default()
        },
        None,
    )
    .await;
    let s = session();
    let halt = Arc::new(AtomicBool::new(false));
    let player = play(&s, halt.clone());
    let r = start_run_with(
        vec![dest(steady.port, KEY_A, "A"), dest(flaky.port, KEY_B, "B")],
        160,
        native(),
        quick(),
    )
    .unwrap();
    // B drops after ~4 s of media and is let back in on its own.
    assert!(
        flaky
            .wait(Duration::from_secs(20), |g| g.len() >= 2 && g[1].published)
            .await
    );
    assert_eq!(steady.got().len(), 1, "A was never restarted");
    // B's second connection starts with its own configuration and a keyframe at 0.
    assert!(
        flaky
            .wait(Duration::from_secs(10), |g| g[1].media.len() > 30)
            .await
    );
    let g = &flaky.got()[1];
    let sh = shape(g);
    assert_eq!((sh[1].0, sh[1].1), (9, [0x17, 0x00]));
    assert_eq!((sh[3].1, sh[3].2), ([0x17, 0x01], 0));
    stop(&r).await;
    halt.store(true, std::sync::atomic::Ordering::Relaxed);
    player.join().unwrap();
    s.end();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_sound_converter_ends_the_run_with_its_reason() {
    let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let srv: Server = test_server::start(Opts::default(), None).await;
    let s = session();
    let r = start_run_with(
        vec![dest(srv.port, KEY_A, "A")],
        160,
        Pipeline::Native(Arc::new(FakeCodecs { fail_open: true })),
        quick(),
    )
    .unwrap();
    wait_for(&r, 10, "failed", |v| v["phase"] == "failed").await;
    assert_eq!(r.status()["error"]["code"], "audio_failed");
    s.end();
}
