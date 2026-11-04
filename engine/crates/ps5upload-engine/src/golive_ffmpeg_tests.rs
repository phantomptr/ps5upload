//! Tests for Go live's ffmpeg command, its progress and log, and the reconnect policy.

use super::*;

fn out(url: &str) -> Output {
    Output {
        url: url.into(),
        format: "flv",
    }
}

#[test]
fn one_destination_is_a_plain_flv_output() {
    let a = args(
        &[out("rtmps://a.rtmps.youtube.com/live2/abcd-1234")],
        160,
        false,
    );
    assert!(!a.contains(&"-tag:v".to_string()));
    let s = a.join(" ");
    assert!(s.contains("-f mpegts -i pipe:0"), "{s}");
    assert!(s.contains("-c:v copy -c:a aac"), "{s}");
    assert!(s.contains("-b:a 160k"), "{s}");
    assert!(s.contains("-progress pipe:1"), "{s}");
    assert!(
        s.ends_with(
            "-f flv -flvflags no_duration_filesize rtmps://a.rtmps.youtube.com/live2/abcd-1234"
        ),
        "{s}"
    );
    assert!(!s.contains("tee"));
    // The URL is one argument, never split or quoted.
    assert_eq!(
        a.last().unwrap(),
        "rtmps://a.rtmps.youtube.com/live2/abcd-1234"
    );
}

#[test]
fn several_destinations_go_through_tee_and_failures_are_ignored() {
    let a = args(
        &[
            out("rtmps://a.rtmps.youtube.com/live2/k1"),
            out("rtmps://ingest.global-contribute.live-video.net/app/k2?bandwidthtest=true"),
            Output {
                url: "srt://example.com:9000?streamid=k3".into(),
                format: "mpegts",
            },
        ],
        500,
        false,
    );
    let s = a.join(" ");
    assert!(s.contains("-flags:a +global_header -tag:v 7 -f tee"), "{s}");
    assert!(s.contains("-b:a 320k"), "clamped: {s}");
    assert_eq!(
        a.last().unwrap(),
        "[f=flv:onfail=ignore:flvflags=no_duration_filesize]rtmps://a.rtmps.youtube.com/live2/k1|\
         [f=flv:onfail=ignore:flvflags=no_duration_filesize]rtmps://ingest.global-contribute.live-video.net/app/k2?bandwidthtest=true|\
         [f=mpegts:onfail=ignore]srt://example.com:9000?streamid=k3"
    );
}

#[test]
fn audio_bitrate_has_a_floor() {
    assert!(args(&[out("rtmp://x/y")], 8, false).contains(&"64k".to_string()));
}

#[test]
fn hevc_through_tee_gets_flvs_hvc1_tag() {
    let a = args(&[out("rtmp://x/a"), out("rtmp://x/b")], 160, true);
    let i = a.iter().position(|x| x == "-tag:v").unwrap();
    assert_eq!(a[i + 1], "0x68766331");
    assert_eq!(
        u32::from_str_radix("68766331", 16).unwrap(),
        u32::from_be_bytes(*b"hvc1")
    );
}

#[test]
fn redact_scrubs_keys_and_urls_longest_first() {
    let secrets = vec![
        "live_123_abcdef".to_string(),
        "rtmps://h/app/live_123_abcdef".to_string(),
        "ab".to_string(), // too short to scrub safely
    ];
    let s = redact(
        "Slave '[f=flv]rtmps://h/app/live_123_abcdef': error opening: live_123_abcdef ab",
        &secrets,
    );
    assert_eq!(s, "Slave '[f=flv]••••': error opening: •••• ab");
    assert!(!s.contains("live_123"));
}

#[test]
fn a_stream_is_flowing_by_frames_or_by_bytes() {
    let report = |block: &str| {
        let mut p = ProgressParser::default();
        block.lines().find_map(|l| p.push(l)).unwrap()
    };
    // ffmpeg 6.1.1 with -c copy, sending: no frames counted.
    assert!(report(
        "frame=0\nbitrate=4828.0kbits/s\ntotal_size=1048576\nout_time_us=2000000\nprogress=continue"
    )
    .flowing());
    // The size alone is enough, and the frames alone (tee reports neither of the others).
    assert!(
        report("frame=0\nbitrate=N/A\ntotal_size=9\nout_time_us=1\nprogress=continue").flowing()
    );
    assert!(
        report("frame=9\nbitrate=N/A\ntotal_size=N/A\nout_time_us=1\nprogress=continue").flowing()
    );
    // Still connecting: nothing has gone anywhere.
    assert!(
        !report("frame=0\nbitrate=N/A\ntotal_size=0\nout_time_us=0\nprogress=continue").flowing()
    );
    assert!(
        !report("frame=0\nbitrate=0.0kbits/s\ntotal_size=0\nout_time_us=5\nprogress=continue")
            .flowing()
    );
    assert!(!report("frame=7\nout_time_us=N/A\nprogress=continue").flowing());
}

#[test]
fn progress_reports_come_at_each_progress_line() {
    let mut p = ProgressParser::default();
    let block = "frame=120\nfps=59.94\nstream_0_0_q=-1.0\nbitrate=6123.4kbits/s\ntotal_size=1500000\nout_time_us=2000000\nout_time_ms=2000000\nout_time=00:00:02.000000\ndup_frames=0\ndrop_frames=3\nspeed=1.01x\nprogress=continue\n";
    let mut got = Vec::new();
    for l in block.lines() {
        if let Some(r) = p.push(l) {
            got.push(r);
        }
    }
    assert_eq!(got.len(), 1);
    let r = &got[0];
    assert_eq!(r.frame, 120);
    assert!((r.fps - 59.94).abs() < 1e-9);
    assert_eq!(r.bitrate_kbps, Some(6123.4));
    assert_eq!(r.out_time_s, 2.0);
    assert_eq!(r.drop_frames, 3);
    assert_eq!(r.speed, Some(1.01));
    assert!(!r.ended);
    // tee: no bitrate or size; and the last block says end.
    assert!(p.push("bitrate=N/A").is_none());
    assert!(p.push("speed=N/A").is_none());
    let r = p.push("progress=end").unwrap();
    assert_eq!(r.bitrate_kbps, None);
    assert_eq!(r.speed, None);
    assert!(r.ended);
    // Junk lines are nothing.
    assert!(p.push("hello").is_none());
}

#[test]
fn log_lines_are_classified() {
    let c = |s: &str| classify(s);
    assert_eq!(
        c("[tcp @ 0x1] Connection to tcp://127.0.0.1:19350?tcp_nodelay=0 failed: Connection refused"),
        Some(Failure::Unreachable)
    );
    assert_eq!(
        c("[tcp @ 0x1] Failed to resolve hostname a.rtmps.youtube.cm: nodename nor servname provided, or not known"),
        Some(Failure::Unreachable)
    );
    assert_eq!(
        c("[rtmp @ 0x1] Server error: Authentication failed."),
        Some(Failure::Rejected)
    );
    assert_eq!(
        c("[rtmp @ 0x1] NetStream.Publish.BadName"),
        Some(Failure::Rejected)
    );
    assert_eq!(
        c("[flv @ 0x1] Video codec hevc not compatible with flv"),
        Some(Failure::Codec)
    );
    assert_eq!(
        c("[out#0/flv @ 0x1] Error muxing a packet: Broken pipe"),
        Some(Failure::Dropped)
    );
    assert_eq!(c("[aac @ 0x1] Qavg: 1234.5"), None);
}

/// No trusted roots for ffmpeg's TLS (a system without a bundle): not the internet's fault.
#[test]
fn a_certificate_failure_is_said_as_one() {
    for line in [
        "[tls @ 0x1] error:0A000086:SSL routines::certificate verify failed",
        "[tls @ 0x1] The certificate is NOT trusted.",
        "[tls @ 0x1] TLS handshake failed: unknown error",
    ] {
        assert_eq!(classify(line), Some(Failure::Tls), "{line}");
    }
    let m = Failure::Tls.message("YouTube");
    assert!(m.contains("YouTube") && m.contains("SSL_CERT_FILE"), "{m}");
    assert!(!m.contains("internet"), "{m}");
}

/// Several sites at once: plain English, not "the streaming sites's server".
#[test]
fn messages_for_several_sites_read_right() {
    for f in [
        Failure::Unreachable,
        Failure::Tls,
        Failure::Rejected,
        Failure::Codec,
        Failure::Dropped,
        Failure::Other,
    ] {
        let m = f.message_many();
        assert!(!m.contains("sites's") && !m.contains("doesn't"), "{m}");
        assert!(m.starts_with(|c: char| c.is_uppercase()), "{m}");
    }
}

#[test]
fn tee_lines_name_the_output() {
    assert_eq!(
        failed_output(
            "[tee @ 0x1] Slave muxer #1 failed: Broken pipe, continuing with 1/2 slaves."
        ),
        Some(1)
    );
    assert_eq!(failed_output("Slave muxer #12 failed: x"), Some(12));
    assert_eq!(failed_output("Slave muxer #1 is fine"), None);
    assert_eq!(
        failed_open("[tee @ 0x1] Slave '[f=flv:onfail=ignore]rtmp://h/app/key': error opening: Connection refused"),
        Some("[f=flv:onfail=ignore]rtmp://h/app/key")
    );
}

#[test]
fn an_early_drop_reads_as_a_refused_key() {
    let s = Duration::from_secs;
    assert_eq!(settle(Failure::Dropped, 1.0, s(3)), Failure::Rejected);
    assert_eq!(settle(Failure::Other, 0.0, s(1)), Failure::Rejected);
    assert_eq!(settle(Failure::Dropped, 120.0, s(121)), Failure::Dropped);
    assert_eq!(
        settle(Failure::Unreachable, 0.0, s(1)),
        Failure::Unreachable
    );
}

#[test]
fn backoff_grows_resets_after_a_good_run_and_gives_up() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let mut b = Backoff::default();
    let mut got = Vec::new();
    for i in 0..7 {
        match b.after(Failure::Unreachable, s(1), t0 + s(i * 10)) {
            Next::Retry(d) => got.push(d.as_secs()),
            n => panic!("{n:?}"),
        }
    }
    assert_eq!(got, [2, 4, 8, 15, 30, 30, 30]);
    assert_eq!(b.attempts(), 7);
    // A minute of streaming resets it.
    assert_eq!(
        b.after(Failure::Dropped, s(300), t0 + s(400)),
        Next::Retry(s(2))
    );
    // Ten minutes of nothing but failures: give up.
    let mut b = Backoff::default();
    let mut last = Next::Retry(s(0));
    for i in 0..30 {
        last = b.after(Failure::Unreachable, s(1), t0 + s(i * 31));
        if matches!(last, Next::GiveUp(_)) {
            assert!(i * 31 >= 600, "gave up early at {i}");
            break;
        }
    }
    assert_eq!(last, Next::GiveUp(Failure::Unreachable));
}

#[test]
fn a_refused_key_twice_or_a_codec_is_final() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let mut b = Backoff::default();
    assert_eq!(b.after(Failure::Rejected, s(2), t0), Next::Retry(s(2)));
    assert_eq!(
        b.after(Failure::Rejected, s(2), t0 + s(5)),
        Next::GiveUp(Failure::Rejected)
    );
    // Not in a row: a dropped connection in between starts the count again.
    let mut b = Backoff::default();
    assert!(matches!(
        b.after(Failure::Rejected, s(2), t0),
        Next::Retry(_)
    ));
    assert!(matches!(
        b.after(Failure::Dropped, s(2), t0),
        Next::Retry(_)
    ));
    assert!(matches!(
        b.after(Failure::Rejected, s(2), t0),
        Next::Retry(_)
    ));
    let mut b = Backoff::default();
    assert_eq!(
        b.after(Failure::Codec, s(1), t0),
        Next::GiveUp(Failure::Codec)
    );
}

#[test]
fn failure_messages_name_the_site() {
    assert!(Failure::Rejected.message("Twitch").contains("stream key"));
    assert!(Failure::Unreachable.message("Kick").contains("Kick"));
    assert!(Failure::Codec.message("Twitch").contains("H.264"));
}
