use std::time::Duration;

use super::super::flv;
use super::super::test_server::{self, Opts};
use super::*;

/// A made-up stream name standing in for a key (never a real one).
const NAME: &str = "test-stream-name-0001";

#[test]
fn urls_split_as_ffmpeg_splits_them() {
    let u = parse_url(&format!("rtmps://a.rtmps.youtube.com/live2/{NAME}")).unwrap();
    assert!(u.tls);
    assert_eq!((u.host.as_str(), u.port), ("a.rtmps.youtube.com", 443));
    assert_eq!(u.app, "live2");
    assert_eq!(u.stream, NAME);
    assert_eq!(u.tc_url, "rtmps://a.rtmps.youtube.com:443/live2");

    // Twitch's bandwidth test and Facebook's own query stay with the name.
    let u = parse_url(&format!(
        "rtmps://ingest.global-contribute.live-video.net/app/{NAME}?bandwidthtest=true"
    ))
    .unwrap();
    assert_eq!(u.app, "app");
    assert_eq!(u.stream, format!("{NAME}?bandwidthtest=true"));
    let u = parse_url(&format!(
        "rtmps://live-api-s.facebook.com:443/rtmp/{NAME}?s_bl=1&a=b"
    ))
    .unwrap();
    assert_eq!((u.port, u.app.as_str()), (443, "rtmp"));
    assert_eq!(u.stream, format!("{NAME}?s_bl=1&a=b"));

    // Three parts or more: the app takes two.
    let u = parse_url("rtmp://host:1936/app/inst/name/more").unwrap();
    assert_eq!(
        (u.port, u.app.as_str(), u.stream.as_str()),
        (1936, "app/inst", "name/more")
    );
    assert_eq!(u.tc_url, "rtmp://host:1936/app/inst");
    // …unless a "prefix:" comes first (mp4:name).
    let u = parse_url("rtmp://host/app/mp4:dir/name").unwrap();
    assert_eq!((u.app.as_str(), u.stream.as_str()), ("app", "mp4:dir/name"));
    // No app at all.
    let u = parse_url("rtmp://host/name").unwrap();
    assert_eq!(
        (u.app.as_str(), u.stream.as_str(), u.port),
        ("", "name", 1935)
    );

    let u = parse_url("rtmp://user:pw@[::1]:1940/live/x").unwrap();
    assert_eq!((u.host.as_str(), u.port), ("::1", 1940));
    assert_eq!(u.tc_url, "rtmp://[::1]:1940/live");

    for bad in [
        "srt://host:9000?streamid=x",
        "http://host/live/x",
        "rtmp://host/live/",
        "rtmp://host",
        "rtmp:///live/x",
        "rtmp://host:notaport/live/x",
    ] {
        assert!(parse_url(bad).is_none(), "{bad}");
    }
}

/// A chunk with an explicit header type, for the reader tests.
fn raw_chunk(fmt: u8, csid: u8, fields: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = vec![(fmt << 6) | csid];
    v.extend_from_slice(fields);
    v.extend_from_slice(data);
    v
}

#[test]
fn the_reader_follows_every_header_type() {
    let mut r = ChunkReader::default();
    // Type 0: ts 1000, len 3, type 9, stream 1.
    r.push(&raw_chunk(
        0,
        6,
        &[0, 0x03, 0xe8, 0, 0, 3, 9, 1, 0, 0, 0],
        b"abc",
    ));
    // Type 1: delta 40, len 2, type 9.
    r.push(&raw_chunk(1, 6, &[0, 0, 40, 0, 0, 2, 9], b"de"));
    // Type 2: delta 20, same length and type.
    r.push(&raw_chunk(2, 6, &[0, 0, 20], b"fg"));
    // Type 3: a new message, same delta (20).
    r.push(&raw_chunk(3, 6, &[], b"hi"));
    let all: Vec<_> = std::iter::from_fn(|| r.next().unwrap()).collect();
    let got: Vec<(u32, &[u8], u32)> = all.iter().map(|m| (m.ts, &m.body[..], m.msid)).collect();
    assert_eq!(
        got,
        vec![
            (1000, &b"abc"[..], 1),
            (1040, &b"de"[..], 1),
            (1060, &b"fg"[..], 1),
            (1080, &b"hi"[..], 1),
        ]
    );
}

#[test]
fn the_reader_reassembles_interleaved_and_split_messages_byte_by_byte() {
    let mut wire = Vec::new();
    // A 300-byte message on csid 4 at chunk size 128 with an extended timestamp, and a short
    // one on csid 5 between its chunks.
    let big: Vec<u8> = (0..300).map(|i| i as u8).collect();
    let ts = 0x0100_0000u32;
    let mut a = Vec::new();
    chunk_message(&mut a, 4, ts, 8, 1, &big, 128);
    let mut b = Vec::new();
    chunk_message(&mut b, 5, 7, 20, 0, b"zz", 128);
    // a = header + 4-byte ext + 128, then 1 + 4 + 128, then 1 + 4 + 44.
    let first = 12 + 4 + 128;
    wire.extend_from_slice(&a[..first]);
    wire.extend_from_slice(&b);
    wire.extend_from_slice(&a[first..]);
    let mut r = ChunkReader::default();
    let mut out = Vec::new();
    for byte in wire {
        r.push(&[byte]);
        while let Some(m) = r.next().unwrap() {
            out.push(m);
        }
    }
    assert_eq!(out.len(), 2);
    assert_eq!(
        (out[0].csid, &out[0].body[..], out[0].ts),
        (5, &b"zz"[..], 7)
    );
    assert_eq!((out[1].csid, out[1].ts, out[1].typ), (4, ts, 8));
    assert_eq!(out[1].body, big);
}

#[test]
fn the_reader_refuses_broken_streams() {
    let mut r = ChunkReader::default();
    // A type 3 chunk with nothing before it.
    r.push(&[0xc4, 1, 2]);
    assert!(r.next().is_err());
    // A message too big for a publisher to be sent.
    let mut r = ChunkReader::default();
    r.push(&raw_chunk(
        0,
        3,
        &[0, 0, 0, 0x7f, 0xff, 0xff, 20, 0, 0, 0, 0],
        b"",
    ));
    assert!(r.next().is_err());
    // A chunk size of zero.
    let mut r = ChunkReader::default();
    let mut w = Vec::new();
    chunk_message(
        &mut w,
        2,
        0,
        MSG_SET_CHUNK_SIZE,
        0,
        &0u32.to_be_bytes(),
        128,
    );
    r.push(&w);
    assert!(r.next().is_err());
}

async fn publish_to(
    port: u16,
    tls: Option<tokio_rustls::TlsConnector>,
    scheme: &str,
    host: &str,
) -> Result<Publisher<Box<dyn Conn>>, RtmpError> {
    let url = parse_url(&format!("{scheme}://{host}:{port}/live/{NAME}")).unwrap();
    let io = connect(&url, tls.as_ref()).await?;
    Publisher::publish(io, &url).await
}

#[tokio::test]
async fn publishes_and_sends_tags_the_server_reads_back() {
    let srv = test_server::start(
        Opts {
            window: 100,
            ..Opts::default()
        },
        None,
    )
    .await;
    let mut p = publish_to(srv.port, None, "rtmp", "127.0.0.1")
        .await
        .unwrap();
    let meta = flv::metadata(&flv::MetaInfo {
        width: 1280,
        height: 720,
        fps: 60,
        video_kbps: 8000,
        hevc: false,
        audio_kbps: 160,
        channels: 2,
    });
    p.send(flv::TAG_DATA, 0, &meta).await.unwrap();
    let frame: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
    p.send(flv::TAG_VIDEO, 0, &frame).await.unwrap();
    p.send(flv::TAG_AUDIO, 21, &[0xaf, 1, 2, 3]).await.unwrap();
    // A timestamp past 24 bits (4.6 hours in).
    p.send(flv::TAG_VIDEO, 0x0100_0010, &[0x27, 1, 0, 0, 0, 5])
        .await
        .unwrap();
    assert!(
        srv.wait(Duration::from_secs(5), |g| g
            .first()
            .is_some_and(|g| g.media.len() == 4))
            .await
    );
    p.close().await;
    assert!(srv.wait(Duration::from_secs(5), |g| g[0].unpublished).await);
    let g = &srv.got()[0];
    assert_eq!(g.app, "live");
    assert_eq!(g.tc_url, format!("rtmp://127.0.0.1:{}/live", srv.port));
    assert!(g.flash_ver.starts_with("FMLE/3.0"));
    assert_eq!(g.stream, NAME);
    assert_eq!(g.publish_type, "live");
    assert_eq!(g.client_chunk, OUT_CHUNK);
    let m: Vec<(u8, u32, usize, u32)> = g
        .media
        .iter()
        .map(|m| (m.typ, m.ts, m.body.len(), m.msid))
        .collect();
    assert_eq!(
        m,
        vec![
            (18, 0, meta.len(), 1),
            (9, 0, frame.len(), 1),
            (8, 21, 4, 1),
            (9, 0x0100_0010, 6, 1),
        ]
    );
    assert_eq!(g.media[1].body, frame);
    assert!(g.pongs >= 1, "the ping after publish is answered");
    assert!(
        g.acks >= 1,
        "bytes past the server's window are acknowledged"
    );
}

#[tokio::test]
async fn a_refused_connect_is_a_rejection() {
    let srv = test_server::start(
        Opts {
            reject_connect: true,
            ..Opts::default()
        },
        None,
    )
    .await;
    let e = publish_to(srv.port, None, "rtmp", "127.0.0.1")
        .await
        .err()
        .unwrap();
    assert_eq!(
        e,
        RtmpError::Rejected("NetConnection.Connect.Rejected".into())
    );
    assert_eq!(e.failure(), Failure::Rejected);
}

#[tokio::test]
async fn a_refused_publish_is_a_rejection_that_never_repeats_the_key() {
    let srv = test_server::start(
        Opts {
            reject_publish: true,
            ..Opts::default()
        },
        None,
    )
    .await;
    let e = publish_to(srv.port, None, "rtmp", "127.0.0.1")
        .await
        .err()
        .unwrap();
    assert_eq!(e, RtmpError::Rejected("NetStream.Publish.BadName".into()));
    assert!(!format!("{e} {e:?}").contains(NAME));
}

#[tokio::test]
async fn rtmps_checks_the_certificate() {
    let (acceptor, roots) = test_server::tls_pair();
    let srv = test_server::start(Opts::default(), Some(acceptor)).await;
    let mut p = publish_to(srv.port, tls_with_roots(roots), "rtmps", "localhost")
        .await
        .unwrap();
    p.send(flv::TAG_AUDIO, 0, &[0xaf, 0, 0x11, 0x90])
        .await
        .unwrap();
    assert!(
        srv.wait(Duration::from_secs(5), |g| g
            .iter()
            .any(|g| g.media.len() == 1))
            .await
    );
    // The public roots don't trust a self-signed certificate.
    let e = publish_to(srv.port, public_tls(), "rtmps", "localhost")
        .await
        .err()
        .unwrap();
    assert!(matches!(e, RtmpError::Tls(_)), "{e:?}");
    assert_eq!(e.failure(), Failure::Tls);
}

#[tokio::test]
async fn nothing_listening_is_unreachable_and_a_silent_server_never_finishes() {
    // A port that was free a moment ago.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let e = publish_to(port, None, "rtmp", "127.0.0.1")
        .await
        .err()
        .unwrap();
    assert!(matches!(e, RtmpError::Unreachable(_)), "{e:?}");
    assert_eq!(e.failure(), Failure::Unreachable);

    let srv = test_server::start(
        Opts {
            silent: true,
            ..Opts::default()
        },
        None,
    )
    .await;
    let r = tokio::time::timeout(
        Duration::from_millis(300),
        publish_to(srv.port, None, "rtmp", "127.0.0.1"),
    )
    .await;
    assert!(
        r.is_err(),
        "the handshake waits; the caller's timeout ends it"
    );
    assert_eq!(
        RtmpError::Timeout("connect").failure(),
        Failure::Unreachable
    );
}

#[tokio::test]
async fn a_server_that_hangs_up_mid_stream_is_a_drop() {
    let srv = test_server::start(
        Opts {
            close_after: Some(2),
            ..Opts::default()
        },
        None,
    )
    .await;
    let mut p = publish_to(srv.port, None, "rtmp", "127.0.0.1")
        .await
        .unwrap();
    let mut failed = None;
    for i in 0..2000u32 {
        if let Err(e) = p.send(flv::TAG_VIDEO, i, &[0u8; 2000]).await {
            failed = Some(e);
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let e = match failed {
        Some(e) => e,
        None => p.read_once().await.err().unwrap(),
    };
    assert!(matches!(e, RtmpError::Dropped(_)), "{e:?}");
}
