//! Tests for Go live's presets, limit checks, URLs and the encrypted key store.

use super::*;

fn session(codec: &'static str, height: u32, fps: u32, kbps: u32) -> SessionInfo {
    SessionInfo {
        host: "10.0.0.9".into(),
        codec,
        width: height * 16 / 9,
        height,
        fps,
        bitrate_kbps: kbps,
    }
}

fn tmp() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ps5upload-golive-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn input(preset: &str, key: &str) -> DestinationInput {
    DestinationInput {
        preset: preset.into(),
        key: Some(key.into()),
        ..Default::default()
    }
}

#[test]
fn every_preset_has_sane_limits() {
    for p in PRESETS {
        assert!(p.codecs.contains(&"h264"), "{} takes H.264", p.id);
        assert_eq!(p.server.is_none(), p.needs_server, "{}", p.id);
        if let Some(s) = p.server {
            assert!(s.starts_with("rtmps://"), "{} over TLS", p.id);
        }
    }
    let ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
    assert_eq!(ids, ["youtube", "twitch", "kick", "facebook", "custom"]);
}

#[test]
fn the_restart_play_offers_passes_the_check_it_answers() {
    for id in ["twitch", "kick", "facebook"] {
        let p = preset(id).unwrap();
        let fixed = session("h264", 1080, 60, p.max_kbps);
        assert!(check(p, &fixed).is_empty(), "{id}");
        // A later session back in HEVC is refused again, so the offer comes back with it.
        let later = session("h265", 1080, 60, p.max_kbps);
        assert!(check(p, &later).iter().any(|c| c.blocking), "{id}");
    }
}

#[test]
fn hevc_is_refused_where_only_h264_goes() {
    let s = session("h265", 1080, 60, 15000);
    for id in ["twitch", "kick", "facebook"] {
        let c = check(preset(id).unwrap(), &s);
        let codec = c.iter().find(|c| c.code == "codec").unwrap();
        assert!(codec.blocking, "{id}");
        assert!(codec.message.contains("H.264"), "{}", codec.message);
    }
    assert!(!check(preset("youtube").unwrap(), &s)
        .iter()
        .any(|c| c.blocking));
    assert!(!check(preset("custom").unwrap(), &s)
        .iter()
        .any(|c| c.blocking));
}

#[test]
fn hdr_is_refused_everywhere() {
    let s = session("h265_hdr", 1080, 60, 10000);
    for p in PRESETS {
        assert!(
            check(p, &s).iter().any(|c| c.code == "hdr" && c.blocking),
            "{}",
            p.id
        );
    }
}

#[test]
fn bitrate_above_the_sites_limit_is_a_warning() {
    let s = session("h264", 1080, 60, 15000);
    let c = check(preset("twitch").unwrap(), &s);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].code, "bitrate");
    assert!(!c[0].blocking);
    assert_eq!(c[0].have, 15000);
    assert_eq!(c[0].limit, 6000);
    assert_eq!(
        c[0].message,
        "Your Play bitrate 15 Mbps is above Twitch's 6 Mbps. Lower it in Play settings (Picture) before going live."
    );
    assert!(check(preset("kick").unwrap(), &session("h264", 1080, 60, 8000)).is_empty());
    assert!(check(preset("youtube").unwrap(), &s).is_empty());
    assert!(check(preset("custom").unwrap(), &s).is_empty());
    let fb = check(
        preset("facebook").unwrap(),
        &session("h264", 1080, 60, 9500),
    );
    assert!(fb[0].message.contains("9.5 Mbps"), "{}", fb[0].message);
}

#[test]
fn keys_and_urls_are_validated() {
    assert_eq!(validate_key("  abcd-1234  ").unwrap(), "abcd-1234");
    assert_eq!(validate_key("ab").unwrap_err().0, "key_missing");
    for bad in [
        "has space",
        "a|b-1234",
        "rtmp://x/y",
        "x[1]yyyy",
        "quote'1234",
    ] {
        assert_eq!(validate_key(bad).unwrap_err().0, "key_invalid", "{bad}");
    }
    // Facebook's keys carry a query.
    assert!(validate_key("FB-1234-0-AbCd?s_bl=1&s_psm=1&a=xyz").is_ok());
    assert_eq!(
        validate_url(
            "RTMPS://abc.global-contribute.live-video.net/app/",
            false,
            true
        )
        .unwrap(),
        "rtmps://abc.global-contribute.live-video.net/app"
    );
    assert!(
        validate_url("rtmp://x/app", false, true).is_err(),
        "Kick: TLS only"
    );
    assert!(validate_url("srt://x:9000", false, false).is_err());
    assert!(validate_url("srt://x:9000?latency=200", true, false).is_ok());
    assert!(validate_url("http://x/app", true, false).is_err());
    assert!(validate_url("rtmp://", true, false).is_err());
    assert!(validate_url("rtmp://x/a|b", true, false).is_err());
}

#[test]
fn publish_urls_per_site() {
    let (d, s) = from_input(&input("youtube", "aaaa-bbbb-cccc"), None).unwrap();
    assert_eq!(
        publish_url(&d, &s).unwrap(),
        (
            "rtmps://a.rtmps.youtube.com/live2/aaaa-bbbb-cccc".into(),
            "flv"
        )
    );
    let mut i = input("twitch", "live_1_xyz");
    i.bandwidth_test = Some(true);
    let (d, s) = from_input(&i, None).unwrap();
    assert_eq!(
        publish_url(&d, &s).unwrap().0,
        "rtmps://ingest.global-contribute.live-video.net/app/live_1_xyz?bandwidthtest=true"
    );
    // The test switch is Twitch's only.
    let mut i = input("youtube", "aaaa-bbbb");
    i.bandwidth_test = Some(true);
    assert!(!from_input(&i, None).unwrap().0.bandwidth_test);
    // Kick: the server from the dashboard.
    assert_eq!(
        from_input(&input("kick", "sk_us-west-2_abc"), None)
            .unwrap_err()
            .0,
        "server_missing"
    );
    let mut i = input("kick", "sk_us-west-2_abc");
    i.server = Some("rtmps://fa723fc1b171.global-contribute.live-video.net/app/".into());
    let (d, s) = from_input(&i, None).unwrap();
    assert_eq!(
        publish_url(&d, &s).unwrap().0,
        "rtmps://fa723fc1b171.global-contribute.live-video.net/app/sk_us-west-2_abc"
    );
    let (d, s) = from_input(&input("facebook", "FB-1-0-Ab?s_bl=1"), None).unwrap();
    assert_eq!(
        publish_url(&d, &s).unwrap().0,
        "rtmps://live-api-s.facebook.com:443/rtmp/FB-1-0-Ab?s_bl=1"
    );
    // Custom: the URL as entered, the key after it (RTMP) or as the stream id (SRT).
    let mut i = input("custom", "");
    i.url = Some("rtmp://192.168.1.5/live".into());
    let (d, s) = from_input(&i, None).unwrap();
    assert_eq!(d.server, "rtmp://192.168.1.5");
    assert_eq!(
        publish_url(&d, &s).unwrap(),
        ("rtmp://192.168.1.5/live".into(), "flv")
    );
    let mut i = input("custom", "mykey");
    i.url = Some("srt://192.168.1.5:9000?latency=200".into());
    let (d, s) = from_input(&i, None).unwrap();
    assert_eq!(
        publish_url(&d, &s).unwrap(),
        (
            "srt://192.168.1.5:9000?latency=200&streamid=mykey".into(),
            "mpegts"
        )
    );
}

#[test]
fn the_key_hint_shows_only_the_last_four() {
    let s = Secret {
        key: "live_test_test_key".into(),
        ..Default::default()
    };
    assert_eq!(key_hint(&s), "…_key");
    let short = Secret {
        key: "abcdefg".into(),
        ..Default::default()
    };
    assert_eq!(key_hint(&short), "…");
}

#[test]
fn keys_are_encrypted_at_rest_and_never_in_the_view() {
    let dir = tmp();
    let key = "live_987654321_SuperSecretKey";
    let st = Store::open(&dir).unwrap();
    let mut i = input("twitch", key);
    i.label = Some("My channel".into());
    let v = st.add(&i).unwrap();
    assert_eq!(v.key_hint, "…tKey");
    assert_eq!(v.dest.label, "My channel");
    // Not in the file, not in what the API shows.
    let file = std::fs::read_to_string(dir.join(FILE)).unwrap();
    assert!(!file.contains(key), "{file}");
    assert!(!file.contains("SuperSecret"));
    let shown = serde_json::to_string(&st.list()).unwrap();
    assert!(!shown.contains(key), "{shown}");
    // The key file is this user's only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(dir.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o777, 0o600);
        let m = std::fs::metadata(dir.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o077, 0);
    }
    // It comes back after a reopen.
    let st2 = Store::open(&dir).unwrap();
    let (d, s) = st2.get(&v.dest.id).unwrap();
    assert_eq!(s.key, key);
    assert_eq!(d.preset, "twitch");
    // A change without a key keeps it; with one replaces it.
    let mut ch = DestinationInput {
        bandwidth_test: Some(true),
        ..Default::default()
    };
    let v2 = st2.update(&v.dest.id, &ch).unwrap();
    assert!(v2.dest.bandwidth_test);
    assert_eq!(st2.get(&v.dest.id).unwrap().1.key, key);
    ch.key = Some("live_new_key_0000".into());
    st2.update(&v.dest.id, &ch).unwrap();
    assert_eq!(st2.get(&v.dest.id).unwrap().1.key, "live_new_key_0000");
    // A replaced key file: the secret is gone, the destination stays.
    std::fs::write(dir.join(KEY_FILE), [7u8; 32]).unwrap();
    let st3 = Store::open(&dir).unwrap();
    let (_, s) = st3.get(&v.dest.id).unwrap();
    assert!(s.key.is_empty());
    assert!(st3.remove(&v.dest.id));
    assert!(st3.list().is_empty());
    assert!(!st3.remove(&v.dest.id));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_change_names_only_what_changes() {
    // What the panel sends: no preset, no key.
    let inp: DestinationInput = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
    assert_eq!(inp.preset, "");
    assert_eq!(inp.enabled, Some(false));
    let dir = tmp();
    let st = Store::open(&dir).unwrap();
    let v = st.add(&input("twitch", "live_987654321_key")).unwrap();
    let v2 = st.update(&v.dest.id, &inp).unwrap();
    assert!(!v2.dest.enabled);
    assert_eq!(v2.dest.preset, "twitch");
    assert_eq!(st.get(&v.dest.id).unwrap().1.key, "live_987654321_key"); // gitleaks:allow (a made-up key)
                                                                         // An add still has to say which site.
    let add: DestinationInput = serde_json::from_str(r#"{"key":"abcd1234"}"#).unwrap();
    assert_eq!(st.add(&add).unwrap_err().0, "preset_unknown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_custom_urls_query_stays_a_query_after_the_key() {
    let url = |server: &str, key: &str| {
        let mut i = input("custom", key);
        i.url = Some(server.into());
        let (d, s) = from_input(&i, None).unwrap();
        publish_url(&d, &s).unwrap().0
    };
    assert_eq!(
        url("rtmp://host/app?auth=x", "key1"),
        "rtmp://host/app/key1?auth=x"
    );
    assert_eq!(
        url("rtmp://host/app/?auth=x&b=2", "key1"),
        "rtmp://host/app/key1?auth=x&b=2"
    );
    assert_eq!(url("rtmp://host/app", "key1"), "rtmp://host/app/key1");
    // The key brings a query too: both stay.
    assert_eq!(
        url("rtmp://host/app?auth=x", "key1?y=2"),
        "rtmp://host/app/key1?y=2&auth=x"
    );
    assert_eq!(
        url("rtmp://host/app?auth=x", ""),
        "rtmp://host/app?auth=x",
        "no key: the URL as entered"
    );
}

/// A replaced key file: what the file holds can't be read, and it is never overwritten with an
/// empty secret, so the old key file can still be put back.
#[test]
fn a_secret_that_cant_be_read_survives_saves_until_the_key_file_comes_back() {
    let dir = tmp();
    let st = Store::open(&dir).unwrap();
    let key = "live_987654321_SuperSecretKey";
    let a = st.add(&input("twitch", key)).unwrap();
    let mut custom = input("custom", "k3y-1234");
    custom.url = Some("rtmp://me:pw@host/app".into());
    let b = st.add(&custom).unwrap();
    let good_key = std::fs::read(dir.join(KEY_FILE)).unwrap();
    drop(st);

    std::fs::write(dir.join(KEY_FILE), [7u8; 32]).unwrap();
    let st = Store::open(&dir).unwrap();
    let shown = st.list();
    assert!(shown.iter().all(|v| v.key_lost && v.key_hint.is_empty()));
    // Nothing to publish with, for either kind.
    for id in [&a.dest.id, &b.dest.id] {
        let (d, s) = st.get(id).unwrap();
        assert_eq!(publish_url(&d, &s), None);
    }
    // Ticking the site off works, and so does a change to another one (both save the file).
    let off: DestinationInput = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
    assert!(!st.update(&a.dest.id, &off).unwrap().dest.enabled);
    assert!(!st.update(&b.dest.id, &off).unwrap().dest.enabled);
    st.add(&input("youtube", "aaaa-bbbb-cccc")).unwrap();
    drop(st);

    // The right key file back: the keys are there.
    std::fs::write(dir.join(KEY_FILE), &good_key).unwrap();
    let st = Store::open(&dir).unwrap();
    assert_eq!(st.get(&a.dest.id).unwrap().1.key, key);
    let (_, s) = st.get(&b.dest.id).unwrap();
    assert_eq!(s.key, "k3y-1234");
    assert_eq!(s.url, "rtmp://me:pw@host/app");
    assert!(!st.get(&a.dest.id).unwrap().1.sealed.is_some());
    drop(st);

    // A new key entered for one under the wrong key file replaces what could not be read.
    std::fs::write(dir.join(KEY_FILE), [7u8; 32]).unwrap();
    let st = Store::open(&dir).unwrap();
    let fresh = DestinationInput {
        key: Some("live_new_key_0000".into()),
        ..Default::default()
    };
    let v = st.update(&a.dest.id, &fresh).unwrap();
    assert!(!v.key_lost);
    assert_eq!(v.key_hint, "…0000");
    let st2 = Store::open(&dir).unwrap();
    assert_eq!(st2.get(&a.dest.id).unwrap().1.key, "live_new_key_0000");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_damaged_key_file_is_set_aside_and_never_half_written() {
    let dir = tmp();
    std::fs::write(dir.join(KEY_FILE), b"short").unwrap();
    let _st = Store::open(&dir).unwrap();
    assert_eq!(
        std::fs::read(dir.join("stream-destinations.bad")).unwrap(),
        b"short"
    );
    assert_eq!(std::fs::read(dir.join(KEY_FILE)).unwrap().len(), 32);
    assert!(!dir.join("stream-destinations.key.tmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// One unreadable moment must not switch Go live off until the engine restarts.
#[test]
fn a_store_that_failed_to_open_is_tried_again() {
    let dir = tmp();
    let slot = Mutex::new(None);
    std::fs::write(dir.join(FILE), b"{ not json").unwrap();
    let e = store_from(&slot, Some(dir.clone())).err().unwrap();
    assert!(
        e.contains(FILE),
        "the message names the file to look at: {e}"
    );
    assert!(store_from(&slot, Some(dir.clone())).is_err());
    assert!(store_from(&slot, None).is_err());
    std::fs::remove_file(dir.join(FILE)).unwrap();
    let s = store_from(&slot, Some(dir.clone())).unwrap();
    // Once open, it stays the one.
    assert!(Arc::ptr_eq(
        &s,
        &store_from(&slot, Some(dir.clone())).unwrap()
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sensitive_covers_a_custom_urls_key_parts() {
    let s = Secret {
        key: "k3y-1234".into(),
        url: "rtmp://host/live/secretpath?token=abcdef".into(),
        ..Default::default()
    };
    let v = s.sensitive();
    assert!(v.contains(&"k3y-1234".to_string()));
    assert!(v.contains(&"rtmp://host/live/secretpath?token=abcdef".to_string()));
    assert!(v.contains(&"live/secretpath?token=abcdef".to_string()));
    assert!(v.contains(&"token=abcdef".to_string()));
}

#[test]
fn a_password_in_a_custom_url_is_never_shown() {
    assert_eq!(
        origin_of("rtmp://me:secret@live.example.com:1935/app"),
        "rtmp://live.example.com:1935"
    );
    assert_eq!(
        origin_of("rtmps://live.example.com/app"),
        "rtmps://live.example.com"
    );
    let (d, s) = from_input(
        &DestinationInput {
            preset: "custom".into(),
            url: Some("rtmp://me:secret@live.example.com/app".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(!d.server.contains("secret"), "{}", d.server);
    assert!(s.url.contains("me:secret@"), "kept, encrypted, for ffmpeg");
}
