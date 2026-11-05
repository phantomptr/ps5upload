//! Go live end to end, against local RTMP listeners (ffmpeg `-listen 1`): real H.264 and Opus
//! through the live feed, the engine's ffmpeg, RTMP, and back out of the listener's FLV, checked
//! with ffprobe. Also: reconnecting after the listener goes away, one of two sites failing
//! while the other keeps going, and an unreachable server.
//!
//! They need ffmpeg and ffprobe (libx264, libopus) on PATH, set the global session and run,
//! and use ports 19350-19352, so they are `#[ignore]`d. No sound is played: everything goes to
//! files. Run with
//! `cargo test -p ps5upload-engine --lib rp_golive -- --ignored --nocapture --test-threads 1`.
//!
//! The rest run everywhere: the HTTP contract of `/api/stream/*`, and the run loop with a pretend
//! ffmpeg (a shell script that prints what ffmpeg prints) instead of the real one.

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

#[test]
#[ignore = "sets the global session"]
fn a_test_that_panics_leaves_no_session_behind() {
    let panicked = std::panic::catch_unwind(|| {
        let _s = session();
        assert!(crate::rp_stream_api::live_feed::session_running());
        panic!("a test failing part way");
    });
    assert!(panicked.is_err());
    assert!(!crate::rp_stream_api::live_feed::session_running());
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
    let r = start_run(vec![dest(19350, "testkey", "Local")], 160, ffmpeg()).unwrap();
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
        // A server killed mid-stream cuts off what it was writing: a few frames at the end, more
        // on a slow machine (6 seen on arm64 in Docker). The other file may have none.
        assert!(errs <= if killed { 16 } else { 0 }, "{errs} decode errors");
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
    )
    .unwrap();
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
    let r = start_run(vec![dest(9, "nokey123", "Nowhere")], 160, ffmpeg()).unwrap();
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
    let r = start_run(vec![dest(9, "nokey123", "X")], 160, ffmpeg()).unwrap();
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
    let r = start_run(vec![d], 160, ffmpeg()).unwrap();
    wait_for(&r, 5, "failed", |s| s["phase"] == "failed").await;
    let st = r.status();
    assert_eq!(st["error"]["code"], "codec_not_allowed");
    assert!(st["error"]["message"].as_str().unwrap().contains("H.264"));
    s.end();
}

// ---- The HTTP contract ----

/// The routes lib.rs registers.
fn router() -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/stream/status", get(status))
        .route("/api/stream/start", post(start))
        .route("/api/stream/stop", post(stop))
        .route("/api/stream/destinations", post(add_destination))
        .route(
            "/api/stream/destinations/{id}",
            post(update_destination).delete(remove_destination),
        )
}

async fn serve() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            l,
            router().into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

/// A request as a client makes it: (status, JSON body).
async fn call(method: &'static str, url: String, body: Option<&str>) -> (u16, serde_json::Value) {
    let body = body.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .build()
            .into();
        let mut r = match (method, body) {
            ("GET", _) => agent.get(&url).call(),
            ("DELETE", _) => agent.delete(&url).call(),
            (_, Some(b)) => agent
                .post(&url)
                .header("content-type", "application/json")
                .send(b.as_str()),
            (_, None) => agent.post(&url).send_empty(),
        }
        .unwrap();
        let text = r.body_mut().read_to_string().unwrap_or_default();
        (
            r.status().as_u16(),
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
        )
    })
    .await
    .unwrap()
}

/// The panel sends only what changes: this failed with 422 ("missing field `preset`"), so the
/// tick and Twitch's test switch never worked.
#[tokio::test]
async fn changing_a_saved_site_over_http_names_only_what_changes() {
    let base = serve().await;
    let d = format!("{base}/api/stream/destinations");
    let (st, v) = call(
        "POST",
        d.clone(),
        Some(r#"{"preset":"twitch","key":"live_12345678_abcd","bandwidth_test":true}"#), // gitleaks:allow (a made-up key)
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["key_hint"], "…abcd");
    assert_eq!(v["key_lost"], false);
    assert_eq!(v["enabled"], true);
    assert_eq!(v["bandwidth_test"], true);
    assert!(!v.to_string().contains("live_12345678"), "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let one = format!("{d}/{id}");

    let (st, v) = call("POST", one.clone(), Some(r#"{"enabled":false}"#)).await;
    assert_eq!((st, &v["enabled"]), (200, &serde_json::json!(false)), "{v}");
    assert_eq!(v["preset"], "twitch");
    assert_eq!(v["key_hint"], "…abcd");
    let (st, v) = call("POST", one.clone(), Some(r#"{"bandwidth_test":false}"#)).await;
    assert_eq!(
        (st, &v["bandwidth_test"]),
        (200, &serde_json::json!(false)),
        "{v}"
    );
    assert_eq!(v["enabled"], false, "the other field stays");
    let (st, v) = call("POST", one.clone(), Some(r#"{"key":"x"}"#)).await;
    assert_eq!((st, v["error"].as_str()), (400, Some("key_missing")), "{v}");

    // The status lists it, without its key.
    let (st, v) = call("GET", format!("{base}/api/stream/status"), None).await;
    assert_eq!(st, 200);
    for field in ["ffmpeg", "session", "presets", "destinations", "live"] {
        assert!(v.get(field).is_some(), "{field}: {v}");
    }
    assert!(v["store_error"].is_null(), "{v}");
    assert!(v["ffmpeg"]["needs_own"].is_boolean(), "{v}");
    let row = v["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id.as_str())
        .unwrap();
    assert_eq!(row["enabled"], false);
    assert!(!v.to_string().contains("live_12345678"), "{v}");

    let (st, v) = call("DELETE", one.clone(), None).await;
    assert_eq!((st, &v["ok"]), (200, &serde_json::json!(true)), "{v}");
    let (st, v) = call("DELETE", one.clone(), None).await;
    assert_eq!((st, v["error"].as_str()), (404, Some("not_found")), "{v}");
    let (st, v) = call("POST", one, Some(r#"{"enabled":true}"#)).await;
    assert_eq!((st, v["error"].as_str()), (404, Some("not_found")), "{v}");
}

#[tokio::test]
async fn refusals_have_a_code_and_a_sentence() {
    // No session and no run of another test in the way.
    let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let base = serve().await;
    let d = format!("{base}/api/stream/destinations");
    let start_url = format!("{base}/api/stream/start");
    for (url, body, status, code) in [
        (d.clone(), r#"{"key":"abcd1234"}"#, 400, "preset_unknown"),
        (
            d.clone(),
            r#"{"preset":"nowhere","key":"abcd1234"}"#,
            400,
            "preset_unknown",
        ),
        (d.clone(), r#"{"preset":"twitch"}"#, 400, "key_missing"),
        (
            d.clone(),
            r#"{"preset":"twitch","key":"has space"}"#,
            400,
            "key_invalid",
        ),
        (
            d.clone(),
            r#"{"preset":"kick","key":"sk_us_abcd"}"#,
            400,
            "server_missing",
        ),
        (
            d.clone(),
            r#"{"preset":"custom","url":"http://x"}"#,
            400,
            "url_invalid",
        ),
        (start_url.clone(), r#"{}"#, 400, "no_destinations"),
        (
            start_url.clone(),
            r#"{"destinations":[]}"#,
            400,
            "no_destinations",
        ),
        (
            start_url.clone(),
            r#"{"destinations":[{"key_id":"gone-000000"}]}"#,
            404,
            "destination_missing",
        ),
    ] {
        let (st, v) = call("POST", url, Some(body)).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (status, Some(code)),
            "{body}: {v}"
        );
        assert!(
            v["message"].as_str().is_some_and(|m| !m.is_empty()),
            "{body}: {v}"
        );
    }
    // A saved site: then it depends on this computer (ffmpeg there or not), and no session runs.
    let (_, v) = call(
        "POST",
        d,
        Some(r#"{"preset":"youtube","key":"aaaa-bbbb-cccc"}"#),
    )
    .await;
    let id = v["id"].as_str().unwrap().to_string();
    let body = serde_json::json!({"destinations": [{"key_id": id}]}).to_string();
    let (st, v) = call("POST", start_url, Some(&body)).await;
    match v["error"].as_str() {
        Some("ffmpeg_missing") => {
            assert_eq!(st, 412, "{v}");
            assert!(v["ffmpeg"]["supported"].is_boolean(), "{v}");
        }
        Some("no_session") => assert_eq!(st, 409, "{v}"),
        other => panic!("{other:?}: {v}"),
    }
    assert!(golive_dest::store().unwrap().remove(&id));
    // Stop with nothing running is fine.
    let (st, v) = call("POST", format!("{base}/api/stream/stop"), None).await;
    assert_eq!((st, &v["ok"]), (200, &serde_json::json!(true)), "{v}");
}

/// A request from this computer that does not address it by a loopback name (DNS rebinding).
#[tokio::test]
async fn a_page_under_another_name_is_refused() {
    let peer = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 50000)));
    let mut h = HeaderMap::new();
    h.insert("host", "evil.example:19113".parse().unwrap());
    let r = stop(peer, h).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

#[test]
fn what_fails_on_this_computer_is_not_a_bad_request() {
    assert_eq!(error_status("not_found"), StatusCode::NOT_FOUND);
    assert_eq!(error_status("disk"), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error_status("store"), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error_status("unsupported"), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(error_status("key_invalid"), StatusCode::BAD_REQUEST);
}

// ---- One site of several failing ----

#[test]
fn a_site_that_fails_right_after_a_start_gets_one_more_try() {
    let early = Duration::from_secs(3);
    let mut fails = 0;
    // A closed connection right after the start: a refused key, most likely. One more try.
    assert_eq!(
        slave_failed(Some(Failure::Dropped), 1.0, early, &mut fails, 0),
        (Failure::Rejected, false)
    );
    assert_eq!(fails, 1);
    assert_eq!(
        slave_failed(Some(Failure::Dropped), 1.0, early, &mut fails, 1),
        (Failure::Rejected, true)
    );
    // Said outright: out at once.
    let mut fails = 0;
    assert_eq!(
        slave_failed(Some(Failure::Rejected), 90.0, early, &mut fails, 0),
        (Failure::Rejected, true)
    );
    assert_eq!(
        slave_failed(Some(Failure::Codec), 1.0, early, &mut fails, 0),
        (Failure::Codec, true)
    );
    // Can't be reached, twice in a row right after a start: out, under its own name.
    let mut fails = 0;
    assert_eq!(
        slave_failed(Some(Failure::Unreachable), 0.0, early, &mut fails, 0),
        (Failure::Unreachable, false)
    );
    assert_eq!(
        slave_failed(Some(Failure::Unreachable), 0.0, early, &mut fails, 1),
        (Failure::Unreachable, true)
    );
    // A site that has been streaming: a drop, let back in, and the count starts over.
    let long = Duration::from_secs(600);
    let mut fails = 1;
    assert_eq!(
        slave_failed(Some(Failure::Dropped), 600.0, long, &mut fails, 1),
        (Failure::Dropped, false)
    );
    assert_eq!(fails, 0);
    assert_eq!(
        slave_failed(None, 600.0, long, &mut fails, 2),
        (Failure::Dropped, false)
    );
    // …until it has been let back in enough times.
    assert_eq!(
        slave_failed(Some(Failure::Dropped), 600.0, long, &mut fails, MAX_REJOINS),
        (Failure::Dropped, true)
    );
}

// ---- The run loop, with a pretend ffmpeg ----

#[cfg(unix)]
mod pretend {
    use super::*;

    /// A progress report as `-progress pipe:1` writes it.
    const PROGRESS: &str = r#"printf 'frame=120\nfps=60\nbitrate=6000.0kbits/s\nout_time_us=2000000\ndrop_frames=0\ndup_frames=0\nspeed=1.0x\nprogress=continue\n'"#;

    fn fake_ffmpeg(dir: &Path, script: &str) -> FfmpegCmd {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("fake-ffmpeg");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        FfmpegCmd {
            program,
            lib_dir: None,
        }
    }

    fn quick() -> Timing {
        Timing {
            stop_grace: Duration::from_millis(300),
            rejoin_after: Duration::from_millis(600),
            ..Timing::default()
        }
    }

    fn one_site() -> Vec<Dest> {
        vec![dest(9, "nokey123", "A")]
    }

    fn lines(p: &Path) -> usize {
        std::fs::read_to_string(p)
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    fn pretend_session() -> TestSession {
        TestSession::start("10.0.77.1", StreamConfig::default())
    }

    /// Ended, and no run in the way of the next test.
    async fn end(r: &Run, s: &TestSession) {
        r.request_stop();
        wait_for(r, 10, "ended", |v| {
            v["phase"] == "ended" || v["phase"] == "failed"
        })
        .await;
        s.end();
    }

    /// Stopping while ffmpeg finishes says so, and a start during it is refused: that ffmpeg
    /// still holds the site's key.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_says_stopping_until_ffmpeg_has_gone() {
        let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
        let dir = tmp_dir("golive-stopping");
        let s = pretend_session();
        // Never reads its input and never ends on its own.
        let ff = fake_ffmpeg(&dir, "exec sleep 300");
        let r = start_run_with(one_site(), 160, ff.clone(), quick()).unwrap();
        wait_for(&r, 5, "started", |v| v["phase"] == "starting").await;
        assert!(
            start_run_with(one_site(), 160, ff.clone(), quick()).is_none(),
            "a second run at once: the first would go on with nothing to stop it"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        r.request_stop();
        assert_eq!(r.status()["phase"], "stopping");
        assert!(diag_summary()["run"]["phase"] == "stopping");
        assert!(start_run_with(one_site(), 160, ff.clone(), quick()).is_none());
        r.wait_finished(STOP_WAIT).await;
        assert_eq!(r.status()["phase"], "ended");
        // Now it can start again.
        let r2 = start_run_with(one_site(), 160, ff, quick()).unwrap();
        assert!(Arc::ptr_eq(&current().unwrap(), &r2));
        end(&r2, &s).await;
    }

    /// The session ends with ffmpeg stuck on a dead upload: it is stopped, and the run waits for
    /// Play instead of showing LIVE for as long as the system keeps the connection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_ending_under_a_stuck_upload_does_not_hang_the_run() {
        let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
        let dir = tmp_dir("golive-stuck");
        let s = pretend_session();
        let ff = fake_ffmpeg(&dir, &format!("{PROGRESS}\nexec sleep 300"));
        let r = start_run_with(one_site(), 160, ff, quick()).unwrap();
        wait_for(&r, 5, "live", |v| v["phase"] == "live").await;
        s.end();
        wait_for(&r, 6, "waiting for Play", |v| {
            v["phase"] == "waiting_for_play"
        })
        .await;
        r.request_stop();
        wait_for(&r, 5, "ended", |v| v["phase"] == "ended").await;
    }

    /// One of two sites closes the connection right after every start (a wrong key): it gets one
    /// more try, shows the refused-key hint, and then stays out. The other is never cut again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_site_among_several_is_not_let_back_in_forever() {
        let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
        let dir = tmp_dir("golive-tee");
        let s = pretend_session();
        let starts = dir.join("starts");
        let ff = fake_ffmpeg(
            &dir,
            &format!(
                "echo x >> '{}'\n\
                 echo '[tee @ 0x1] Slave muxer #1 failed: Broken pipe, continuing with 1/2 slaves.' >&2\n\
                 {PROGRESS}\nexec sleep 300",
                starts.display()
            ),
        );
        let r = start_run_with(
            vec![dest(9, "keyA1234", "A"), dest(9, "keyB1234", "B")],
            160,
            ff,
            quick(),
        )
        .unwrap();
        wait_for(&r, 5, "B out for now", |v| {
            v["destinations"][1]["state"] == "retrying"
        })
        .await;
        assert_eq!(r.status()["destinations"][1]["error"]["code"], "rejected");
        wait_for(&r, 8, "B out for good", |v| {
            v["destinations"][1]["state"] == "failed"
        })
        .await;
        let st = r.status();
        assert_eq!(st["destinations"][1]["error"]["code"], "rejected");
        assert_eq!(st["destinations"][0]["state"], "live", "{st}");
        assert_eq!(st["phase"], "live", "{st}");
        assert_eq!(lines(&starts), 2, "one restart, not five");
        // Longer than a rejoin: nothing restarts any more.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(lines(&starts), 2);
        assert!(!r.status().to_string().contains("keyB1234"));
        end(&r, &s).await;
    }

    /// Skips add up over the ffmpegs of a run: each new one counts from zero.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn upload_skips_are_a_total_over_restarts() {
        let _turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
        let dir = tmp_dir("golive-skips");
        let s = pretend_session();
        s.feed()
            .video_header(&[0, 0, 0, 1, 0x67, 9, 0, 0, 0, 1, 0x68, 9]);
        // Takes nothing, so the feed's queue for it fills; goes away after a moment.
        let ff = fake_ffmpeg(&dir, "sleep 1.6\nexit 1");
        let r = start_run_with(one_site(), 160, ff, quick()).unwrap();
        // Keyframes: each one that finds the queue full is a skip.
        let flood = |s: &TestSession, from: u16| {
            for i in 0..1000u16 {
                let au = [0, 0, 0, 1, 0x65, (i & 0xff) as u8];
                s.feed()
                    .video(&au, from.wrapping_add(i), true, Instant::now());
            }
        };
        let readers = |s: &TestSession| {
            s.feed().status()["consumers"].as_u64().unwrap_or(0)
                + s.feed().status()["go_live"].as_u64().unwrap_or(0)
        };
        for _ in 0..50 {
            if readers(&s) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        flood(&s, 0);
        wait_for(&r, 5, "skips", |v| v["upload_skips"].as_u64() > Some(0)).await;
        let first = r.status()["upload_skips"].as_u64().unwrap();
        // It went, the backoff passed, and the next one is reading.
        wait_for(&r, 6, "a restart", |v| v["reconnects"].as_u64() == Some(1)).await;
        for _ in 0..80 {
            if readers(&s) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        flood(&s, 1000);
        wait_for(&r, 5, "more skips", |v| {
            v["upload_skips"].as_u64() > Some(first + first / 2)
        })
        .await;
        end(&r, &s).await;
    }
}
