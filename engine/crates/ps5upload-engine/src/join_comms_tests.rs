// join_comms.rs's own tests: the room's rules on a Room made here, with time in our hands, and
// the socket side (Link) on the engine-wide table with a console address of each test's own.
// The guest-socket run with four talkers is in join_comms_net_tests.rs (join.rs's tests).

use super::*;
use crate::join_voice::up_frame;

fn t0() -> Instant {
    Instant::now()
}

fn ms(t: Instant, n: u64) -> Instant {
    t + Duration::from_millis(n)
}

/// A person with one socket, in `room`.
fn join(room: &mut Room, pid: u64, name: &str, player: u8, now: Instant) -> Arc<LinkShared> {
    let host = pid == HOST_PID;
    let sid = if host { 0 } else { room.new_sid() };
    let mut p = Person::new(sid, name, Some(player), host, now);
    p.muted_voice = !host && room.modes.start_muted;
    let l = Arc::new(LinkShared {
        q: Mutex::new(LinkQ::default()),
        notify: Notify::new(),
    });
    p.links.push((pid, l.clone()));
    room.people.insert(pid, p);
    l
}

/// Accepted the notice and turned the mic on.
fn talker(room: &mut Room, pid: u64, now: Instant) {
    room.accept(pid, NOTICE_VERSION, now).unwrap();
    room.mic(pid, "on", now).unwrap();
}

fn texts(l: &LinkShared) -> Vec<serde_json::Value> {
    lock(&l.q)
        .text
        .drain(..)
        .map(|t| serde_json::from_str(&t).unwrap())
        .collect()
}

fn with_key<'a>(v: &'a [serde_json::Value], k: &str) -> Vec<&'a serde_json::Value> {
    v.iter().filter_map(|x| x.get(k)).collect()
}

fn heard(l: &LinkShared) -> Vec<(u8, u16)> {
    let mut q = lock(&l.q);
    let s = q.pending.sids();
    q.pending.clear();
    s
}

fn say(room: &mut Room, pid: u64, seq: u16, level: u8, now: Instant) -> Result<(), Refusal> {
    let f = up_frame(seq, level, &[0x55; 60]);
    room.voice_in(pid, join_voice::parse_up(&f).unwrap(), now)
}

#[test]
fn nothing_is_sent_or_heard_before_the_notice_is_accepted() {
    let t = t0();
    let mut r = Room::new(t);
    let sam = join(&mut r, 1, "Sam", 2, t);
    let mia = join(&mut r, 2, "Mia", 3, t);
    // Chat and the mic are refused with the notice's version, for the client to show it.
    assert_eq!(r.chat_in(1, Body::Quick(3), t), Err(Refusal::Notice));
    assert_eq!(r.mic(1, "on", t), Err(Refusal::Notice));
    let v = refusal_json("chat", Refusal::Notice);
    assert_eq!(v["comms_refused"]["why"], "notice");
    assert_eq!(v["comms_refused"]["notice"], NOTICE_VERSION);
    // An older version is not enough.
    assert_eq!(r.accept(1, 0, t), Err(Refusal::Notice));
    // Sam accepts and says GG; Mia (not accepted) gets nothing of it.
    r.accept(1, NOTICE_VERSION, t).unwrap();
    let got = texts(&sam);
    assert_eq!(with_key(&got, "comms")[0]["accepted"], true);
    assert!(with_key(&got, "chat_history")[0]
        .as_array()
        .unwrap()
        .is_empty());
    r.chat_in(1, Body::Quick(3), t).unwrap();
    assert_eq!(with_key(&texts(&sam), "chat")[0]["quick"], 3);
    assert!(with_key(&texts(&mia), "chat").is_empty());
    // Sam's mic is on, but Mia hears nothing until she accepts.
    r.mic(1, "on", t).unwrap();
    say(&mut r, 1, 1, 100, t).unwrap();
    assert!(heard(&mia).is_empty());
    r.accept(2, NOTICE_VERSION, t).unwrap();
    // …and then she gets the chat so far.
    let hist = texts(&mia);
    assert_eq!(with_key(&hist, "chat_history")[0][0]["quick"], 3);
    say(&mut r, 1, 2, 100, ms(t, 20)).unwrap();
    assert_eq!(heard(&mia), vec![(1, 2)]);
    // A frame never comes back to its speaker.
    assert!(heard(&sam).is_empty());
}

#[test]
fn host_mutes_are_enforced_by_dropping() {
    let console = "10.99.0.1";
    let host = attach(console, HOST_PID, "Host", None, true);
    let sam = attach(console, 1, "Sam", Some(2), false);
    for l in [&host, &sam] {
        l.on_text(r#"{"notice": 1}"#);
    }
    host.on_text(r#"{"room_mic": "on"}"#);
    sam.on_text(r#"{"mic": "on"}"#);
    // On the host's page the console mic's own "mic" key is not the room's.
    host.on_text(r#"{"mic": "off"}"#);
    assert!(with_room(console, |r| r.people[&HOST_PID].mic_on).unwrap());
    texts(&sam.shared);
    // Sam talks: the host hears him.
    assert!(sam.on_binary(&up_frame(1, 90, &[1; 30])));
    assert_eq!(heard(&host.shared), vec![(1, 1)]);
    // The host mutes Sam's voice (by player number): his frames are dropped, he is told.
    assert_eq!(mute(console, Target::Player(2), Some(true), None), Ok(1));
    let got = texts(&sam.shared);
    assert_eq!(with_key(&got, "muted")[0]["voice"], true);
    let full = with_key(&got, "voice");
    let full = full.iter().rfind(|v| v.get("mic").is_some()).unwrap();
    assert_eq!(full["mic"], "off", "the mic goes off");
    sam.on_binary(&up_frame(2, 90, &[1; 30]));
    assert!(heard(&host.shared).is_empty());
    sam.on_text(r#"{"mic": "on"}"#);
    let got = texts(&sam.shared);
    assert_eq!(with_key(&got, "comms_refused")[0]["why"], "muted");
    with_room(console, |r| {
        assert_eq!(r.people[&1].drops.refused, 1);
        // Chat still works until chat is muted too.
        r.chat_in(1, Body::Text("hi".into()), Instant::now())
            .unwrap();
    });
    mute(console, Target::Sid(1), None, Some(true)).unwrap();
    with_room(console, |r| {
        assert_eq!(
            r.chat_in(1, Body::Text("hi".into()), Instant::now()),
            Err(Refusal::Muted)
        )
    });
    // The host can't be muted; nobody on player 4.
    assert_eq!(mute(console, Target::Sid(0), Some(true), None), Err("host"));
    assert_eq!(
        mute(console, Target::Player(4), Some(true), None),
        Err("no_player")
    );
    // Unmuted: he talks again.
    mute(console, Target::Player(2), Some(false), Some(false)).unwrap();
    sam.on_text(r#"{"mic": "on"}"#);
    sam.on_binary(&up_frame(3, 90, &[1; 30]));
    assert_eq!(heard(&host.shared), vec![(1, 3)]);
    drop(sam);
    drop(host);
    room_closed(console);
}

#[test]
fn mute_all_and_start_muted_cover_new_players_too() {
    let console = "10.99.0.2";
    let t = t0();
    let mut sam = attach(console, 11, "Sam", Some(2), false);
    set_modes(console, |m| m.start_muted = false);
    assert_eq!(mute(console, Target::All, Some(true), None), Ok(1));
    // Kim joins after "mute all": muted from the start.
    let kim = attach(console, 12, "Kim", Some(3), false);
    let k = texts(&kim.shared);
    assert_eq!(with_key(&k, "muted")[0]["voice"], true, "{k:?}");
    assert!(modes(console).start_muted);
    // Unmute all: both may talk again, and later joiners start unmuted.
    assert_eq!(mute(console, Target::All, Some(false), None), Ok(2));
    assert!(!modes(console).start_muted);
    let _ = (&mut sam, t);
    drop(sam);
    drop(kim);
    room_closed(console);
}

#[test]
fn the_rooms_modes_decide_chat_and_voice() {
    let t = t0();
    let mut r = Room::new(t);
    let _sam = join(&mut r, 1, "Sam", 2, t);
    let mia = join(&mut r, 2, "Mia", 3, t);
    talker(&mut r, 1, t);
    talker(&mut r, 2, t);
    // Chat off: nothing; quick only: no typing.
    r.modes.chat = ChatMode::Off;
    assert_eq!(r.chat_in(1, Body::Quick(1), t), Err(Refusal::Off));
    r.modes.chat = ChatMode::Quick;
    assert_eq!(
        r.chat_in(1, Body::Text("hey".into()), t),
        Err(Refusal::QuickOnly)
    );
    assert_eq!(r.chat_in(1, Body::Quick(1), t), Ok(()));
    assert_eq!(r.chat_in(1, Body::Quick(99), t), Err(Refusal::BadQuick));
    // Typed links are defanged for everyone; a Public room strips codes.
    r.modes.chat = ChatMode::On;
    r.modes.public = true;
    texts(&mia);
    r.chat_in(1, Body::Text("go to evil.example code ABC-DEF".into()), t)
        .unwrap();
    let line = &with_key(&texts(&mia), "chat")[0].clone();
    assert_eq!(line["text"], "go to evil·example code ···");
    assert_eq!(line["from"], "Sam");
    assert!(line.get("pid").is_none());

    // Push-to-talk only: frames go while the key is held and 300 ms after.
    r.modes.voice = VoiceMode::Ptt;
    assert_eq!(say(&mut r, 1, 1, 90, t), Err(Refusal::PttOnly));
    r.mic(1, "ptt_down", t).unwrap();
    say(&mut r, 1, 2, 90, ms(t, 20)).unwrap();
    r.mic(1, "ptt_up", ms(t, 40)).unwrap();
    say(&mut r, 1, 3, 90, ms(t, 300)).unwrap();
    assert_eq!(say(&mut r, 1, 4, 90, ms(t, 400)), Err(Refusal::PttOnly));
    assert_eq!(heard(&mia), vec![(1, 2), (1, 3)]);
    // The talk key needs the mic on.
    r.mic(1, "off", ms(t, 500)).unwrap();
    assert_eq!(r.mic(1, "ptt_down", ms(t, 500)), Err(Refusal::MicOff));

    // Voice off: nobody's mic goes on, frames are dropped.
    r.modes.voice = VoiceMode::Off;
    assert_eq!(r.mic(1, "on", t), Err(Refusal::Off));
    assert_eq!(say(&mut r, 2, 1, 90, t), Err(Refusal::Off));
}

#[test]
fn voice_off_closes_every_mic_and_tells_everyone() {
    let console = "10.99.0.3";
    let sam = attach(console, 21, "Sam", Some(2), false);
    sam.on_text(r#"{"notice": 1}"#);
    sam.on_text(r#"{"mic": "on"}"#);
    texts(&sam.shared);
    set_modes(console, |m| m.voice = VoiceMode::Off);
    let got = texts(&sam.shared);
    assert_eq!(with_key(&got, "comms")[0]["voice"], "off");
    assert_eq!(with_key(&got, "voice")[0]["mic"], "off");
    drop(sam);
    room_closed(console);
}

#[test]
fn each_listener_hears_at_most_three_speakers_and_a_fourth_waits() {
    let t = t0();
    let mut r = Room::new(t);
    let links: Vec<_> = (1..=5u64)
        .map(|i| join(&mut r, i, &format!("P{i}"), i as u8, t))
        .collect();
    for i in 1..=5 {
        talker(&mut r, i, t);
    }
    // Four talk at once: listener 5 hears the first three.
    for seq in 0..3u16 {
        let now = ms(t, 20 * u64::from(seq));
        for i in 1..=4 {
            say(&mut r, i, seq, 100, now).unwrap();
        }
    }
    let got = heard(&links[4]);
    let mut sids: Vec<u8> = got.iter().map(|f| f.0).collect();
    sids.dedup();
    sids.sort();
    sids.dedup();
    assert_eq!(sids, vec![1, 2, 3]);
    // Speaker 1 stops; after the 300 ms hangover speaker 4 takes the place.
    for seq in 3..30u16 {
        let now = ms(t, 20 * u64::from(seq));
        for i in 2..=4 {
            say(&mut r, i, seq, 100, now).unwrap();
        }
    }
    let late: Vec<u8> = heard(&links[4])
        .iter()
        .filter(|f| f.1 >= 25)
        .map(|f| f.0)
        .collect();
    assert!(late.contains(&4), "{late:?}");
    assert!(late.len() <= 3 * 5);
    // Nobody ever gets more than three speakers in a bundle's worth.
    for l in &links {
        let q = lock(&l.q);
        assert!(q.speakers.len() <= join_voice::MAX_SPEAKERS);
    }
}

#[test]
fn a_poor_route_pauses_voice_both_ways_and_a_better_one_brings_it_back() {
    let t = t0();
    let mut r = Room::new(t);
    let sam = join(&mut r, 1, "Sam", 2, t);
    let mia = join(&mut r, 2, "Mia", 3, t);
    talker(&mut r, 1, t);
    talker(&mut r, 2, t);
    texts(&sam);
    r.tick(1, Some(Tier::Poor), Some(2), t);
    let got = texts(&sam);
    let v = with_key(&got, "voice");
    assert_eq!(v.last().unwrap()["paused"], "poor");
    assert_eq!(v.last().unwrap()["kbps"], 0);
    // Others see it by his name.
    let m = texts(&mia);
    let people = &with_key(&m, "voice").last().unwrap()["people"];
    assert!(people
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["sid"] == 1 && p["paused"] == true));
    // His frames don't go; he hears nobody.
    assert_eq!(say(&mut r, 1, 1, 90, t), Err(Refusal::Poor));
    say(&mut r, 2, 1, 90, t).unwrap();
    assert!(heard(&sam).is_empty());
    assert_eq!(r.people[&1].drops.paused, 1);
    // Playable: 12 kb/s, 40 ms bundles — after 10 s there.
    for s in 0..=10 {
        r.tick(1, Some(Tier::Playable), Some(2), t + Duration::from_secs(s));
    }
    let v = texts(&sam);
    let last = (*with_key(&v, "voice").last().unwrap()).clone();
    assert_eq!(
        (last["kbps"].clone(), last["frame_ms"].clone()),
        (12.into(), 40.into())
    );
    say(&mut r, 2, 2, 90, t + Duration::from_secs(10)).unwrap();
    assert!(lock(&sam.q).low, "his bundles go every 40 ms");
}

#[test]
fn rate_limits_slow_scripts_not_people() {
    let t = t0();
    let mut r = Room::new(t);
    let _sam = join(&mut r, 1, "Sam", 2, t);
    let mia = join(&mut r, 2, "Mia", 3, t);
    talker(&mut r, 1, t);
    talker(&mut r, 2, t);
    // A person talking for a minute (50 frames a second) is never dropped.
    for i in 0..3000u16 {
        say(&mut r, 1, i, 90, ms(t, 20 * u64::from(i))).unwrap();
    }
    assert_eq!(r.people[&1].drops.rate, 0);
    heard(&mia);
    // A flood of 1000 frames at once: the burst goes, the rest is dropped.
    let at = ms(t, 70_000);
    let ok = (0..1000u16)
        .filter(|i| say(&mut r, 1, *i, 90, at).is_ok())
        .count();
    assert!(ok <= join_voice::FRAMES_BURST as usize + 1, "{ok}");
    assert!(heard(&mia).len() <= join_voice::PENDING_MAX);
    // Chat: a quick exchange goes, a flood is told to slow down (with the wait).
    let mut refused = None;
    for i in 0..20 {
        if let Err(e) = r.chat_in(1, Body::Text(format!("line {i}")), at) {
            refused = Some((i, e));
            break;
        }
    }
    let (i, e) = refused.unwrap();
    assert_eq!(i, join_chat::CHAT_BURST as usize);
    assert!(matches!(e, Refusal::SlowDown(d) if d > Duration::from_secs(2)));
    let v = refusal_json("chat", e);
    assert!(v["comms_refused"]["retry_ms"].as_u64().unwrap() > 2000);
}

#[test]
fn refusals_are_told_at_most_once_a_second_voice_every_five() {
    let t = t0();
    let mut r = Room::new(t);
    let sam = join(&mut r, 1, "Sam", 2, t);
    let p = r.people.get_mut(&1).unwrap();
    for i in 0..10 {
        p.refuse("chat", Refusal::Empty, ms(t, 50 * i));
        p.refuse("voice", Refusal::MicOff, ms(t, 500 * i));
    }
    let got = texts(&sam);
    let r = with_key(&got, "comms_refused");
    assert_eq!(r.iter().filter(|v| v["why"] == "empty").count(), 1);
    assert_eq!(r.iter().filter(|v| v["why"] == "mic_off").count(), 1);
}

#[test]
fn the_rooms_voice_budget_drops_the_quietest_speaker_first() {
    let t = t0();
    let mut r = Room::new(t);
    let links: Vec<_> = (1..=12u64)
        .map(|i| join(&mut r, i, &format!("P{i}"), (i % 4) as u8, t))
        .collect();
    for i in 1..=12 {
        talker(&mut r, i, t);
    }
    // Speakers 1-3 loud, 4 quiet, big frames to 11 listeners each: far over 0.8 Mb/s.
    for seq in 0..150u16 {
        let now = ms(t, 20 * u64::from(seq));
        for i in 1..=4u64 {
            let level = if i == 4 { 10 } else { 200 };
            let f = up_frame(seq, level, &[0x55; 250]);
            let _ = r.voice_in(i, join_voice::parse_up(&f).unwrap(), now);
        }
        for l in &links {
            lock(&l.q).pending.clear();
        }
    }
    assert!(r.people[&4].drops.cap > 0, "the quiet one goes");
    assert_eq!(r.people[&1].drops.cap, 0, "a loud one stays");
}

#[test]
fn flags_reach_the_host_once_per_person() {
    let t = t0();
    let mut r = Room::new(t);
    let host = join(&mut r, HOST_PID, "Host", 1, t);
    let _sam = join(&mut r, 1, "Sam", 2, t);
    let _lee = join(&mut r, 2, "Lee", 3, t);
    texts(&host);
    let lee_sid = r.people[&2].sid;
    r.flag(1, lee_sid);
    r.flag(1, lee_sid);
    r.flag(1, 0); // never the host
    let got = texts(&host);
    let f = with_key(&got, "flagged");
    assert_eq!(f.len(), 1);
    assert_eq!(
        (f[0]["by"].clone(), f[0]["who"].clone()),
        ("Sam".into(), "Lee".into())
    );
}

#[tokio::test]
async fn the_socket_side_sends_voice_before_chat_and_waits_without_spinning() {
    let console = "10.99.0.4";
    let sam = attach(console, 31, "Sam", Some(2), false);
    let mut mia = attach(console, 32, "Mia", Some(3), false);
    for l in [&sam, &mia] {
        l.on_text(r#"{"notice": 1}"#);
        l.on_text(r#"{"mic": "on"}"#);
    }
    // Drain what joining said.
    while let Ok(m) = tokio::time::timeout(Duration::from_millis(50), mia.next(false)).await {
        assert!(matches!(m, Message::Text(_)));
    }
    // A chat line and a voice frame wait: the frame goes first.
    sam.on_text(r#"{"chat": {"quick": 5}}"#);
    assert!(sam.on_binary(&up_frame(1, 99, &[7; 40])));
    let first = mia.next(false).await;
    let Message::Binary(b) = first else {
        panic!("voice first: {first:?}")
    };
    let got = join_voice::parse_bundle(&b).unwrap();
    assert_eq!((got[0].1, got[0].2.len()), (1, 40));
    // The chat line, then "nobody speaks" once the 300 ms hangover is over.
    let mut saw_chat = false;
    while let Ok(m) = tokio::time::timeout(Duration::from_millis(700), mia.next(false)).await {
        if let Message::Text(t) = m {
            saw_chat |= t.contains("\"quick\":5");
        }
    }
    assert!(saw_chat);
    // Not ours: a pad frame is left alone.
    assert!(!sam.on_binary(&[0x01, 0, 0, 0, 0]));
    // Nothing waiting: next() waits (cancelled here) instead of returning.
    assert!(
        tokio::time::timeout(Duration::from_millis(400), mia.next(false))
            .await
            .is_err()
    );
    drop(sam);
    drop(mia);
    room_closed(console);
    assert!(lock(&ROOMS)
        .as_ref()
        .is_none_or(|m| !m.contains_key(&host_key(console))));
}

#[test]
fn the_hosts_view_gets_the_rooms_talk_and_each_guests_mutes() {
    let console = "10.99.0.5";
    let sam = attach(console, 41, "Sam", Some(2), false);
    sam.on_text(r#"{"notice": 1}"#);
    sam.on_text(r#"{"chat": {"text": "hello"}}"#);
    mute(console, Target::Player(2), None, Some(true)).unwrap();
    let mut v = serde_json::json!({"open": true, "guests": [{"id": 41, "player": 2}, {"id": 999}]});
    extend_state(console, &mut v);
    assert_eq!(v["room"]["chat"], "on");
    assert_eq!(v["room"]["voice"], "open");
    assert_eq!(v["room"]["notice"], NOTICE_VERSION);
    assert_eq!(v["room"]["chat_log"][0]["text"], "hello");
    assert_eq!(v["room"]["voice_kbps"]["in"], 0);
    assert_eq!(v["guests"][0]["muted"]["chat"], true);
    assert_eq!(v["guests"][0]["voice"]["sid"], 1);
    assert!(v["guests"][1].get("muted").is_none());
    // A room the room model already put under `room` keeps its keys.
    let mut v = serde_json::json!({"room": {"name": "Alex's room"}});
    extend_state(console, &mut v);
    assert_eq!(v["room"]["name"], "Alex's room");
    assert_eq!(v["room"]["chat"], "on");
    // Closing the room forgets the chat and the guests.
    drop(sam);
    room_closed(console);
    let mut v = serde_json::json!({});
    extend_state(console, &mut v);
    assert_eq!(v["room"]["chat_log"], serde_json::json!([]));
    // Counts only in a bug report.
    let d = diag().to_string();
    assert!(!d.contains("hello") && !d.contains("Sam"), "{d}");
}

#[test]
fn defaults_follow_the_rooms_visibility() {
    let p = Modes::defaults(true);
    assert_eq!(
        (p.chat, p.voice, p.start_muted),
        (ChatMode::Quick, VoiceMode::Ptt, true)
    );
    let q = Modes::defaults(false);
    assert_eq!(
        (q.chat, q.voice, q.start_muted),
        (ChatMode::On, VoiceMode::Open, false)
    );
}
