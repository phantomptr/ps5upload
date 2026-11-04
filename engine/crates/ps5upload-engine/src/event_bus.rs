//! The engine's one event feed: what `GET /api/events` serves. Every event gets a sequence number
//! (the SSE `id:`) and stays in a ring, so a client that reconnects with `Last-Event-ID` gets what
//! it missed. Transfer jobs keep the default event name (the shape existing consumers read);
//! newer kinds are named: `journal`, `install`, `op`, `queue`. A client that fell too far behind gets a
//! `resync` event and refetches `/api/jobs`.
//!
//! Job snapshots still go out on `AppState::events_tx`, sent under the job map's lock so their
//! order holds; [`forward_jobs`] numbers them onto this bus in that order.

use axum::{
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
};
use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::sync::broadcast;
use tokio_stream::{
    wrappers::{errors::BroadcastStreamRecvError, BroadcastStream},
    Stream, StreamExt,
};

/// How many events a reconnecting client can catch up on.
pub(crate) const RING_CAP: usize = 1000;
/// Live subscribers may fall this far behind before they get a `resync`.
const CHANNEL_CAP: usize = 2048;
/// Job rows remembered for `/api/jobs?since_seq=`. The job map holds 256 at most.
const JOB_SEQS_CAP: usize = 1024;

/// One published event.
#[derive(Debug)]
pub(crate) struct BusEvent {
    pub seq: u64,
    /// The SSE event name. `None` is the default event (job snapshots).
    pub kind: Option<&'static str>,
    pub data: String,
    /// A job's snapshot: the newer one replaces it in the ring.
    job: Option<String>,
}

impl BusEvent {
    fn to_sse(&self) -> Event {
        let ev = Event::default().id(self.seq.to_string()).data(&self.data);
        match self.kind {
            Some(k) => ev.event(k),
            None => ev,
        }
    }
}

struct Ring {
    next_seq: u64,
    /// The newest number that fell off the end of the ring. A job snapshot replaced by a newer one
    /// of that job is not a loss: the client gets the newer one.
    lost_through: u64,
    events: VecDeque<Arc<BusEvent>>,
}

pub(crate) struct EventBus {
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Arc<BusEvent>>,
    cap: usize,
    job_seqs: Mutex<HashMap<String, u64>>,
}

/// What a new subscriber starts from.
pub(crate) struct Subscription {
    /// Events numbered after the client's `Last-Event-ID`, oldest first.
    pub backlog: Vec<Arc<BusEvent>>,
    /// Events the client asked for that the ring no longer has (`Some(0)`: it named an event
    /// this engine never sent, so it should refetch too).
    pub missed: Option<u64>,
    pub rx: broadcast::Receiver<Arc<BusEvent>>,
}

/// Numbers keep rising across restarts: a client's `Last-Event-ID` from before a restart is then
/// always older than the ring, which is the truth (it missed everything since).
fn first_seq() -> u64 {
    // 2^20 events a second before it could overlap; under 2^53, so JavaScript reads it exactly.
    (ps5upload_core::events::now_ms() / 1000).max(1) << 20
}

impl EventBus {
    pub(crate) fn new(cap: usize, first: u64) -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAP);
        EventBus {
            ring: Mutex::new(Ring {
                next_seq: first,
                lost_through: first.saturating_sub(1),
                events: VecDeque::with_capacity(cap.min(RING_CAP)),
            }),
            tx,
            cap,
            job_seqs: Mutex::new(HashMap::new()),
        }
    }

    /// Publishes an event whose body needs its own number. The lock makes the ring's order and
    /// the live order the same, and makes [`Self::subscribe`] exact.
    pub(crate) fn publish_with(
        &self,
        kind: Option<&'static str>,
        data: impl FnOnce(u64) -> String,
    ) -> u64 {
        self.publish_inner(kind, None, data)
    }

    fn publish_inner(
        &self,
        kind: Option<&'static str>,
        job: Option<String>,
        data: impl FnOnce(u64) -> String,
    ) -> u64 {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let seq = ring.next_seq;
        ring.next_seq += 1;
        // A transfer ticks several times a second: one row per job in the ring, or the ticks would
        // push out the named events a reconnecting client came for.
        if let Some(id) = &job {
            ring.events.retain(|e| e.job.as_ref() != Some(id));
        }
        let ev = Arc::new(BusEvent {
            seq,
            kind,
            data: data(seq),
            job,
        });
        if ring.events.len() >= self.cap {
            if let Some(gone) = ring.events.pop_front() {
                ring.lost_through = ring.lost_through.max(gone.seq);
            }
        }
        ring.events.push_back(ev.clone());
        let _ = self.tx.send(ev);
        seq
    }

    pub(crate) fn publish(&self, kind: Option<&'static str>, data: String) -> u64 {
        self.publish_with(kind, |_| data)
    }

    /// Publishes a named event with a JSON body.
    pub(crate) fn publish_json(&self, kind: &'static str, data: &serde_json::Value) -> u64 {
        self.publish(Some(kind), data.to_string())
    }

    /// A subscriber from now on, plus what it missed after `last` (nothing is replayed to a
    /// client that names no last event: it starts live, as it always did).
    pub(crate) fn subscribe(&self, last: Option<u64>) -> Subscription {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let rx = self.tx.subscribe();
        let Some(last) = last else {
            return Subscription {
                backlog: Vec::new(),
                missed: None,
                rx,
            };
        };
        let backlog: Vec<_> = ring
            .events
            .iter()
            .filter(|e| e.seq > last)
            .cloned()
            .collect();
        let missed = if last >= ring.next_seq {
            Some(0)
        } else if last < ring.lost_through {
            Some(ring.lost_through - last)
        } else {
            None
        };
        Subscription {
            backlog,
            missed,
            rx,
        }
    }

    /// The number of the newest event, or the one before the first when none was sent yet.
    pub(crate) fn last_seq(&self) -> u64 {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_seq
            .saturating_sub(1)
    }

    pub(crate) fn note_job(&self, job_id: &str, seq: u64) {
        let mut g = self.job_seqs.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() >= JOB_SEQS_CAP && !g.contains_key(job_id) {
            // The oldest half: long gone from the job map, which holds 256.
            let mut seqs: Vec<u64> = g.values().copied().collect();
            seqs.sort_unstable();
            let cut = seqs[seqs.len() / 2];
            g.retain(|_, s| *s >= cut);
        }
        g.insert(job_id.to_string(), seq);
    }

    /// The number of the newest event about a job (0 before the first).
    pub(crate) fn job_seq(&self, job_id: &str) -> u64 {
        self.job_seqs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job_id)
            .copied()
            .unwrap_or(0)
    }
}

/// The process's bus.
pub(crate) fn bus() -> &'static EventBus {
    static BUS: OnceLock<EventBus> = OnceLock::new();
    BUS.get_or_init(|| EventBus::new(RING_CAP, first_seq()))
}

/// Numbers the job snapshots sent on `events_tx` onto the bus until the sender is gone.
pub(crate) fn forward_jobs(events_tx: &broadcast::Sender<String>) {
    forwarded()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(events_tx.downgrade());
    let rx = events_tx.subscribe();
    tokio::spawn(forward_loop(rx, bus()));
}

fn forwarded() -> &'static Mutex<Vec<broadcast::WeakSender<String>>> {
    static CHANNELS: OnceLock<Mutex<Vec<broadcast::WeakSender<String>>>> = OnceLock::new();
    CHANNELS.get_or_init(Default::default)
}

/// Someone would read a progress tick sent on `events_tx`: a client of `GET /api/events`, or a
/// receiver of the channel other than the forwarder. A tick is built only then. A change of a job's
/// state is always sent: `/api/jobs?since_seq=` numbers jobs by it.
pub(crate) fn tick_wanted(events_tx: &broadcast::Sender<String>) -> bool {
    let forwarders = {
        let mut channels = forwarded().lock().unwrap_or_else(|e| e.into_inner());
        channels.retain(|w| w.strong_count() > 0);
        channels
            .iter()
            .filter_map(broadcast::WeakSender::upgrade)
            .filter(|s| s.same_channel(events_tx))
            .count()
    };
    events_tx.receiver_count() > forwarders || (forwarders > 0 && bus().tx.receiver_count() > 0)
}

async fn forward_loop(mut rx: broadcast::Receiver<String>, bus: &'static EventBus) {
    loop {
        match rx.recv().await {
            Ok(msg) => forward_one(bus, msg),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                bus.publish(Some("resync"), resync_body(n));
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn forward_one(bus: &EventBus, msg: String) {
    let job_id = serde_json::from_str::<serde_json::Value>(&msg)
        .ok()
        .and_then(|v| v.get("job_id").and_then(|j| j.as_str()).map(str::to_string));
    let seq = bus.publish_inner(None, job_id.clone(), |_| msg);
    if let Some(id) = job_id {
        bus.note_job(&id, seq);
    }
}

fn resync_body(dropped: u64) -> String {
    serde_json::json!({ "resync": true, "dropped": dropped }).to_string()
}

fn resync_event(dropped: u64) -> Event {
    Event::default().event("resync").data(resync_body(dropped))
}

/// The SSE items for a subscription: `resync` first when events are gone, the backlog, then live.
pub(crate) fn event_stream(sub: Subscription) -> impl Stream<Item = Result<Event, Infallible>> {
    let head = sub.missed.map(resync_event).into_iter();
    let backlog = sub.backlog.into_iter().map(|e| e.to_sse());
    let replay = tokio_stream::iter(
        head.chain(backlog)
            .map(Ok::<_, Infallible>)
            .collect::<Vec<_>>(),
    );
    let live = BroadcastStream::new(sub.rx).map(|msg| {
        Ok(match msg {
            Ok(ev) => ev.to_sse(),
            // Live events were dropped under this client: it refetches instead of guessing.
            Err(BroadcastStreamRecvError::Lagged(n)) => resync_event(n),
        })
    });
    replay.chain(live)
}

/// `GET /api/events`: job snapshots (default event) and the named kinds. `Last-Event-ID` replays
/// what the ring still has.
pub(crate) async fn events_handler(
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let last = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    Sse::new(event_stream(bus().subscribe(last))).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus_of(cap: usize) -> EventBus {
        EventBus::new(cap, 100)
    }

    #[test]
    fn job_ticks_do_not_push_named_events_out_of_the_replay() {
        let bus = bus_of(4);
        let first = bus.publish_json("queue", &serde_json::json!({"n": 1}));
        for tick in 0..50 {
            forward_one(&bus, format!(r#"{{"job_id":"j","tick":{tick}}}"#));
        }
        let sub = bus.subscribe(Some(first - 1));
        assert_eq!(sub.missed, None, "a replaced tick is not a loss");
        assert_eq!(sub.backlog.len(), 2);
        assert_eq!(sub.backlog[0].kind, Some("queue"));
        assert!(sub.backlog[1].data.contains(r#""tick":49"#));
    }

    #[tokio::test]
    async fn the_forwarder_alone_wants_no_ticks() {
        let (tx, rx) = broadcast::channel::<String>(8);
        drop(rx);
        assert!(!tick_wanted(&tx), "nobody at all");
        let _plain = tx.subscribe();
        assert!(tick_wanted(&tx), "a receiver and no forwarder");
        drop(_plain);
        forward_jobs(&tx);
        // The feed's own client is another test's business: only this channel's count is asked here.
        assert_eq!(tx.receiver_count(), 1);
        let _other = tx.subscribe();
        assert!(tick_wanted(&tx), "someone besides the forwarder");
    }

    #[test]
    fn numbers_rise_and_the_ring_holds_the_newest() {
        let b = bus_of(3);
        for i in 0..5 {
            assert_eq!(b.publish(None, format!("{i}")), 100 + i);
        }
        let s = b.subscribe(Some(101));
        let seqs: Vec<u64> = s.backlog.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![102, 103, 104]);
        assert_eq!(s.missed, None, "102 is the next one it wanted");
        assert_eq!(b.last_seq(), 104);
    }

    #[test]
    fn a_client_older_than_the_ring_is_told_how_much_is_gone() {
        let b = bus_of(3);
        for i in 0..6 {
            b.publish(None, format!("{i}"));
        }
        let s = b.subscribe(Some(100));
        assert_eq!(s.missed, Some(2), "101 and 102 fell out of the ring");
        assert_eq!(s.backlog.first().map(|e| e.seq), Some(103));
    }

    #[test]
    fn a_number_this_engine_never_sent_is_a_resync_not_silence() {
        let b = bus_of(3);
        b.publish(None, "x".into());
        let s = b.subscribe(Some(9_999));
        assert_eq!(s.missed, Some(0));
        assert!(s.backlog.is_empty());
    }

    #[test]
    fn no_last_event_id_starts_live_with_no_replay() {
        let b = bus_of(3);
        b.publish(None, "old".into());
        let s = b.subscribe(None);
        assert!(s.backlog.is_empty());
        assert_eq!(s.missed, None);
    }

    #[test]
    fn caught_up_means_nothing_to_replay_and_nothing_missed() {
        let b = bus_of(3);
        let seq = b.publish(None, "x".into());
        let s = b.subscribe(Some(seq));
        assert!(s.backlog.is_empty());
        assert_eq!(s.missed, None);
    }

    #[test]
    fn publish_with_hands_the_event_its_own_number() {
        let b = bus_of(3);
        let seq = b.publish_with(Some("journal"), |n| format!("{{\"seq\":{n}}}"));
        let s = b.subscribe(Some(seq - 1));
        assert_eq!(s.backlog[0].data, format!("{{\"seq\":{seq}}}"));
        assert_eq!(s.backlog[0].kind, Some("journal"));
    }

    #[tokio::test]
    async fn a_lagging_subscriber_gets_a_resync_event_in_the_stream() {
        let b = bus_of(8);
        let sub = b.subscribe(None);
        // Past the channel's capacity with nobody reading.
        for i in 0..(CHANNEL_CAP as u64 + 5) {
            b.publish(None, format!("{i}"));
        }
        let mut stream = Box::pin(event_stream(sub));
        let first = stream.next().await.unwrap().unwrap();
        let text = format!("{first:?}");
        assert!(text.contains("resync"), "{text}");
    }

    #[test]
    fn jobs_are_remembered_by_their_newest_event() {
        let b = bus_of(8);
        forward_one(&b, r#"{"job_id":"a","job":{}}"#.into());
        let first = b.job_seq("a");
        forward_one(&b, r#"{"job_id":"b","job":{}}"#.into());
        forward_one(&b, r#"{"job_id":"a","job":{}}"#.into());
        assert!(b.job_seq("a") > first);
        assert_eq!(b.job_seq("nobody"), 0);
    }

    #[test]
    fn job_memory_stays_bounded() {
        let b = bus_of(8);
        for i in 0..(JOB_SEQS_CAP * 3) {
            forward_one(&b, format!(r#"{{"job_id":"j{i}","job":{{}}}}"#));
        }
        assert!(b.job_seqs.lock().unwrap().len() <= JOB_SEQS_CAP);
        assert!(b.job_seq(&format!("j{}", JOB_SEQS_CAP * 3 - 1)) > 0);
    }
}
