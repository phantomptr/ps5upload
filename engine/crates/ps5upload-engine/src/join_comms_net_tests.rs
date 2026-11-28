// Play Rooms voice and chat over real guest sockets (included into join.rs's tests, for its fakes):
// three guests on /api/join/ws and the host's side talking at once, while one guest plays. The
// pad's way to the helper must not slow down, the bytes must match the budget (join_voice.rs),
// and a Poor route must pause voice.

use super::*;
use crate::join_voice::{up_frame, KIND_VOICE_DOWN};
use std::sync::atomic::AtomicUsize;

/// A helper that notes when each pad state reached it (and which: [`pad_frame`]'s id).
struct TimedHelper {
    inner: Arc<FakeHelper>,
    seen: Mutex<Vec<(Instant, u8, u16)>>,
}

impl Port for TimedHelper {
    fn open(
        &self,
        h: &str,
        slot: u8,
        t: &str,
        bind: Option<Bind>,
    ) -> anyhow::Result<Option<Opened>> {
        self.inner.open(h, slot, t, bind)
    }
    fn states(&self, h: &str, items: &[StateItem]) -> anyhow::Result<Option<Ack>> {
        let now = Instant::now();
        self.seen
            .lock()
            .unwrap()
            .extend(items.iter().map(|i| (now, i.slot, pad_id(&i.state))));
        Port::states(&*self.inner, h, items)
    }
    fn hb(&self, h: &str, slot: u8, t: &str, live: bool) -> anyhow::Result<Option<Ack>> {
        self.inner.hb(h, slot, t, live)
    }
    fn close(&self, h: &str, slot: u8, t: &str) -> anyhow::Result<Option<Ack>> {
        self.inner.close(h, slot, t)
    }
    fn output(&self, h: &str, slot: u8) -> anyhow::Result<Option<Output>> {
        self.inner.output(h, slot)
    }
    fn status(&self, h: &str) -> anyhow::Result<Option<serde_json::Value>> {
        self.inner.status(h)
    }
}

/// A pad frame (no buttons) whose sticks carry `id`, so each one tells itself apart at the helper:
/// the left stick's x is `id % 250 + 1` (each frame differs from the one before, so each is sent),
/// the right stick's x `128 + id / 250`.
fn pad_frame(seq: u32, id: u16) -> Vec<u8> {
    let lx = (id % 250) as u8 + 1;
    let rx = 128 + (id / 250) as u8;
    let mut b = vec![KIND_PAD];
    b.extend_from_slice(&seq.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&[lx, 128, rx, 128, 0, 0]);
    b.push(0);
    b
}

/// The id [`pad_frame`] put in a state (any other state gets one no frame has).
fn pad_id(st: &PadState) -> u16 {
    match (st.lx.checked_sub(1), st.rx.checked_sub(128)) {
        (Some(l), Some(r)) if l < 250 => u16::from(l) + u16::from(r) * 250,
        _ => u16::MAX,
    }
}

/// What a guest's socket got: voice bundles (bytes, frames by speaker) and text.
#[derive(Default)]
struct Heard {
    bundles: AtomicUsize,
    bytes: AtomicUsize,
    frames: Mutex<HashMap<u8, usize>>,
    text: Mutex<Vec<serde_json::Value>>,
}

impl Heard {
    fn texts_with(&self, k: &str) -> Vec<serde_json::Value> {
        self.text
            .lock()
            .unwrap()
            .iter()
            .filter_map(|v| v.get(k).cloned())
            .collect()
    }
    fn reset(&self) {
        self.bundles.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
        self.frames.lock().unwrap().clear();
    }
}

type Sink_ = futures_util::stream::SplitSink<Guest_, TMsg>;

/// Reads a guest's socket until it closes.
fn listen(mut rx: futures_util::stream::SplitStream<Guest_>, heard: Arc<Heard>) {
    tokio::spawn(async move {
        while let Some(Ok(m)) = rx.next().await {
            match m {
                TMsg::Binary(b) if b.first() == Some(&KIND_VOICE_DOWN) => {
                    heard.bundles.fetch_add(1, Ordering::Relaxed);
                    heard.bytes.fetch_add(b.len(), Ordering::Relaxed);
                    let f = crate::join_voice::parse_bundle(&b).expect("a well-formed bundle");
                    let mut by = heard.frames.lock().unwrap();
                    for (sid, _, _) in f {
                        *by.entry(sid).or_default() += 1;
                    }
                }
                TMsg::Text(t) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) {
                        heard.text.lock().unwrap().push(v);
                    }
                }
                _ => {}
            }
        }
    });
}

async fn join_as(url: &str, key: &str, console: &str, name: &str) -> (Sink_, Arc<Heard>) {
    let mut g = guest(
        url,
        serde_json::json!({"hello": {"invite": key, "name": name}}),
    )
    .await;
    assert_eq!(next(&mut g).await["stage"], "asking");
    allow_next(console, true).await;
    let j = next_with(&mut g, "stage").await;
    assert_eq!(j["stage"], "joined", "{j}");
    let (mut tx, rx) = g.split();
    let heard = Arc::new(Heard::default());
    listen(rx, heard.clone());
    for m in [r#"{"notice": 1}"#, r#"{"mic": "on"}"#] {
        tx.send(TMsg::Text(m.into())).await.unwrap();
    }
    (tx, heard)
}

/// Polls the host's view until `f` holds (10 s).
async fn until_state(
    console: &str,
    what: &str,
    f: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..400 {
        let s = host_state(console).await;
        if f(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "timed out waiting for {what}: {}",
        host_state(console).await
    );
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Pad frames timed each way (silent, talking) in one run.
const FRAMES: u16 = 150;
/// The two ways take turns in blocks of this many frames (200 ms: under the voice's 300 ms
/// hangover, so the talkers stay speakers through a silent block).
const BLOCK: u16 = 10;
const OPUS: usize = 60;
/// Runs at most: the latency check passes when one run meets it (see the test).
const TRIES: usize = 3;

/// One run: Sam plays (a pad frame every 20 ms) for 6 s; every other [`BLOCK`] of frames, Sam,
/// Kim, Lee and the host each send a voice frame every 20 ms too. The pad latencies at the
/// helper, ms: (silent, talking), [`FRAMES`] frames sent each way.
///
/// The ways take turns in short blocks rather than one after the other: a test machine's busy
/// moments (other tests, other builds) then land on both alike, and what differs between them
/// is the voice.
async fn run(
    timed: &TimedHelper,
    sinks: &mut [Sink_],
    host: &crate::join_comms::Link,
) -> (Vec<f64>, Vec<f64>) {
    timed.seen.lock().unwrap().clear();
    let mut sent: Vec<(u16, Instant, bool)> = Vec::new();
    let mut voice_seq = 0u16;
    let mut every = tokio::time::interval(Duration::from_millis(20));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    for i in 0..FRAMES * 2 {
        every.tick().await;
        let talk = (i / BLOCK) % 2 == 1;
        // The pad frame first, as a client writes it.
        sent.push((i, Instant::now(), talk));
        sinks[0]
            .send(TMsg::Binary(pad_frame(u32::from(i), i).into()))
            .await
            .unwrap();
        if talk {
            for s in sinks.iter_mut() {
                s.send(TMsg::Binary(up_frame(voice_seq, 120, &[0x55; OPUS]).into()))
                    .await
                    .unwrap();
            }
            host.on_binary(&up_frame(voice_seq, 120, &[0x66; OPUS]));
            voice_seq += 1;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let seen = timed.seen.lock().unwrap().clone();
    let (mut silent, mut talking) = (Vec::new(), Vec::new());
    for (id, at, talk) in sent {
        let Some(ms) = seen
            .iter()
            .find(|(t, slot, s)| *slot == 2 && *s == id && *t >= at)
            .map(|(t, _, _)| t.duration_since(at).as_secs_f64() * 1000.0)
        else {
            continue;
        };
        if talk { &mut talking } else { &mut silent }.push(ms);
    }
    (silent, talking)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_talkers_leave_the_pads_latency_alone_and_fit_the_budget() {
    let _turn = PARTY_TESTS.lock().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings::default());
    let console = "10.77.0.61";
    let timed = Arc::new(TimedHelper {
        inner: Arc::new(FakeHelper::default()),
        seen: Mutex::new(Vec::new()),
    });
    link::TEST_PORTS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(host_key(console), timed.clone() as Arc<dyn Port>);
    let url = serve().await;
    set_live(console, 1);
    let (ci, h) = local();
    let s = body(
        open(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await,
    )
    .await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();

    // The host's page (its side of the room's talk, as rp_stream_api.rs attaches it): one socket
    // reading what it hears, one more of the host's (a second page) sending its frames.
    let mut host =
        crate::join_comms::attach(console, crate::join_comms::HOST_PID, "Host", None, true);
    host.on_text(r#"{"notice": 1}"#);
    host.on_text(r#"{"room_mic": "on"}"#);
    let host_heard = Arc::new(AtomicUsize::new(0));
    let (host_stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let hh = host_heard.clone();
    let host_rx = tokio::spawn(async move {
        loop {
            tokio::select! {
                m = host.next(false) => if let Message::Binary(b) = m {
                    hh.fetch_add(crate::join_voice::parse_bundle(&b).map_or(0, |f| f.len()), Ordering::Relaxed);
                },
                _ = &mut stopped => return,
            }
        }
    });
    let host_tx =
        crate::join_comms::attach(console, crate::join_comms::HOST_PID, "Host", None, true);

    let (sam, sam_h) = join_as(&url, &key, console, "Sam").await;
    let (kim, kim_h) = join_as(&url, &key, console, "Kim").await;
    let (lee, lee_h) = join_as(&url, &key, console, "Lee").await;
    let mut sinks = vec![sam, kim, lee];
    // Everyone accepted; the host's view lists their talk.
    let st = until_state(console, "everyone's voice", |s| {
        s["guests"]
            .as_array()
            .is_some_and(|g| g.len() == 3 && g.iter().all(|g| g["voice"]["accepted"] == true))
    })
    .await;
    assert_eq!(st["room"]["voice"], "open");
    // Let the helper's pads open before timing.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The pads' latency unchanged by four talkers: within 2 ms at the median and 5 ms at p95 (the
    // 8th-slowest of 150 frames). A pad that waits on voice (a bundle's 20 ms tick, a lock, a
    // queue) loses far more. What voice costs pads otherwise is CPU: on a machine with no core to
    // spare, the pad thread (join.rs `link`) waits for one while the same process relays voice,
    // measured at up to 0.8 ms at the median (none on an idle machine). Not p99: that is the
    // second-slowest frame, where a busy machine's scheduler parked a thread, silent or not. A
    // busy machine can still stall one run's talking blocks more than its silent ones; voice that
    // holds the pads up does it in every run, so the check passes when one of TRIES runs meets it.
    let mut runs = Vec::new();
    let mut unchanged = false;
    for _ in 0..TRIES {
        for h in [&sam_h, &kim_h, &lee_h] {
            h.reset();
        }
        host_heard.store(0, Ordering::Relaxed);
        let (mut silent, mut talking) = run(&timed, &mut sinks, &host_tx).await;
        assert!(
            silent.len() as u16 >= FRAMES * 3 / 4,
            "silent: {} of {FRAMES}",
            silent.len()
        );
        assert!(
            talking.len() as u16 >= FRAMES * 3 / 4,
            "talking: {} of {FRAMES}",
            talking.len()
        );
        let [s50, s90, s95, s99] = [0.5, 0.9, 0.95, 0.99].map(|p| pct(&mut silent, p));
        let [t50, t90, t95, t99] = [0.5, 0.9, 0.95, 0.99].map(|p| pct(&mut talking, p));
        let line = format!(
            "silent p50 {s50:.2} p90 {s90:.2} p95 {s95:.2} p99 {s99:.2} ms; \
             4 talkers p50 {t50:.2} p90 {t90:.2} p95 {t95:.2} p99 {t99:.2} ms"
        );
        eprintln!("pad latency at the helper: {line}");
        runs.push(line);
        unchanged = t50 <= s50 + 2.0 && t95 <= s95 + 5.0;
        if unchanged {
            break;
        }
    }
    assert!(
        unchanged,
        "voice held the pads up in each of {TRIES} runs: {runs:#?}"
    );
    // The last run's talkers each sent FRAMES frames 20 ms apart.
    let talk_s = f64::from(FRAMES) * 0.02;

    // Each guest hears the other three (two guests and the host) and never themselves.
    for (h, me) in [(&sam_h, 1u8), (&kim_h, 2), (&lee_h, 3)] {
        let by = h.frames.lock().unwrap().clone();
        assert!(!by.contains_key(&me), "{me} heard themselves: {by:?}");
        assert_eq!(by.len(), 3, "{by:?}");
        for (sid, n) in &by {
            assert!(*n >= FRAMES as usize * 85 / 100, "{me} got {n} of {sid}'s");
        }
    }
    assert!(host_heard.load(Ordering::Relaxed) >= 3 * FRAMES as usize * 85 / 100);
    // Bytes against the budget: 3 speakers × 50 frames/s × (60 B Opus + 4 B) + 2 B a bundle.
    let bytes = kim_h.bytes.load(Ordering::Relaxed) as f64;
    let bundles = kim_h.bundles.load(Ordering::Relaxed) as f64;
    let kbps = bytes * 8.0 / 1000.0 / talk_s;
    let want =
        3.0 * 50.0 * (OPUS as f64 + 4.0) * 8.0 / 1000.0 + bundles * 2.0 * 8.0 / 1000.0 / talk_s;
    eprintln!("a listener's voice: {kbps:.1} kb/s in {bundles} bundles over {talk_s:.2} s (budget {want:.1})");
    assert!((kbps - want).abs() <= want * 0.15, "{kbps:.1} vs {want:.1}");
    let st = host_state(console).await;
    assert!(st["room"]["voice_kbps"]["out"].as_u64().is_some(), "{st}");

    // Kim's route turns Poor: her voice pauses within a second or so (the status tick), both ways.
    sinks[1]
        .send(TMsg::Text(
            r#"{"stats": {"frames": 100, "tier": "poor", "route": "lan"}}"#.into(),
        ))
        .await
        .unwrap();
    let t = Instant::now();
    loop {
        if kim_h
            .texts_with("voice")
            .iter()
            .any(|v| v["paused"] == "poor")
        {
            break;
        }
        assert!(
            t.elapsed() < Duration::from_millis(2500),
            "voice not paused"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    eprintln!(
        "voice paused {} ms after the Poor report",
        t.elapsed().as_millis()
    );

    // Chat over the same sockets: Lee's line reaches Sam with Lee's name.
    sinks[2]
        .send(TMsg::Text(r#"{"chat": {"text": "gg"}}"#.into()))
        .await
        .unwrap();
    let t = Instant::now();
    while !sam_h
        .texts_with("chat")
        .iter()
        .any(|c| c["text"] == "gg" && c["from"] == "Lee")
    {
        assert!(t.elapsed() < Duration::from_secs(2), "chat line not heard");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    for s in sinks.iter_mut() {
        let _ = s.send(TMsg::Text(r#"{"leave":1}"#.into())).await;
    }
    drop(sinks);
    let _ = host_stop.send(());
    host_rx.await.unwrap();
    drop(host_tx);
    let (ci, h) = local();
    close(ci, h, Query(Default::default()), Json(CloseBody { addr: console.into(), confirm_friends: true }))
    .await;
    set_live(console, 0);
    let _ = lee_h;
}
