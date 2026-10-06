//! Runs one connection after its handshake: answers Ping, records RTT from Pong,
//! sends heartbeats, declares the peer dead after `dead_after` without a byte, and
//! hands every other frame to the owner (SPEC.md §6).
//!
//! Every frame goes out through one writer task fed by a bounded queue (`Outbox`), so
//! no reader ever waits on a socket write, and enqueueing is cancel-safe: a frame is
//! either queued whole or not at all, and only the writer task touches the stream.
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

use crate::conn::{now_us, Frame, FrameBody, FrameReader, FrameWriter, Pace};
use crate::gen::{self, Bye, Ping, Pong};
use crate::session::Timing;
use crate::wire::FrameMessage;
use crate::Ava1Error;

/// Frames waiting for the writer, per connection. A full queue means the peer is not
/// taking our bytes; the writer's own stall limit then ends the connection.
pub(crate) const OUTBOX_DEPTH: usize = 64;
/// Frames waiting for the owner, per connection. A full queue stops the reader, which
/// pushes back on the peer through TCP.
pub(crate) const DELIVER_DEPTH: usize = 64;
/// The default slowest one frame may arrive or leave (after a `dead_after` grace) before
/// the peer counts as gone: slow links are fine, a peer dripping a frame forever is not.
pub(crate) const MIN_FRAME_RATE: u32 = 8 * 1024;
/// How many heartbeat ticks in a row may put off declaring the peer dead because this
/// process overslept. A machine that keeps stalling must not keep a dead link forever.
pub(crate) const MAX_DEFERRALS: u32 = 2;

enum Out {
    Frame {
        ty: u8,
        flags: u8,
        channel: u32,
        body: FrameBody,
        /// The writer flips it the moment it takes the frame out of the queue, before
        /// the write. Un-taken when the writer dies (the queue died with it), the
        /// frame provably never left this process — the data plane uses that to
        /// release the frame's window charge on a lane death (I3's precise form).
        taken: Option<Arc<AtomicBool>>,
    },
    /// A heartbeat. Its `t_us` is stamped when it is written, so time spent queued
    /// behind other frames never counts as round-trip time.
    Ping {
        seq: u32,
    },
    Flush(oneshot::Sender<()>),
}

/// The sending side of a connection.
#[derive(Clone)]
pub(crate) struct Outbox {
    tx: mpsc::Sender<Out>,
    /// Frames queued or being written right now. The queue's own length is not enough:
    /// the frame the writer has taken out is still on its way.
    unwritten: Arc<AtomicUsize>,
    /// True once the writer has ended, however it ended (the connection died, or the
    /// link was dropped): a frame still un-taken then never left this process.
    writer_dead: Arc<AtomicBool>,
}

/// How long a failed write waits for the reader to deliver the peer's own reason (an Error
/// it sent before closing) before the link records the write failure.
const WRITE_FAIL_GRACE: Duration = Duration::from_millis(200);

/// Sets the flag when the writer task ends, however it ends (it is dropped when the
/// task is aborted or returns): after that nothing further can leave the process.
struct DeadOnDrop(Arc<AtomicBool>);
impl Drop for DeadOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Counts a frame as unwritten until it is queued; uncounts it if it never was (the
/// queue closed, or the caller dropped the send).
struct Counted<'a>(Option<&'a AtomicUsize>);

impl<'a> Counted<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::SeqCst);
        Self(Some(n))
    }

    /// The frame is in the queue: the writer uncounts it once it is on the wire.
    fn queued(mut self) {
        self.0 = None;
    }
}

impl Drop for Counted<'_> {
    fn drop(&mut self) {
        if let Some(n) = self.0 {
            n.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Full {
    /// The queue is full: the peer is not reading.
    Full,
    /// The connection has ended.
    Closed,
}

impl Outbox {
    /// Queues a frame, waiting for room. Cancel-safe: dropped before it completes, the
    /// frame was not queued.
    pub(crate) async fn send<M: FrameMessage>(&self, channel: u32, m: &M) -> Result<(), Ava1Error> {
        let body = m.to_bytes()?;
        self.send_frame(M::TYPE, 0, channel, body).await
    }

    /// Queues an already-encoded frame with exact header flags and channel, waiting for
    /// room (the data plane's sends). The frame is queued whole or not at all, so a
    /// dropped future never leaves half a frame on the wire.
    pub(crate) async fn send_frame(
        &self,
        ty: u8,
        flags: u8,
        channel: u32,
        body: impl Into<FrameBody>,
    ) -> Result<(), Ava1Error> {
        self.send_frame_marked(ty, flags, channel, body, None).await
    }

    /// `send_frame` with a take-marker: the writer flips it the moment it takes the
    /// frame out of the queue (before the write). Un-taken at the writer's death, the
    /// frame provably never left this process.
    pub(crate) async fn send_frame_marked(
        &self,
        ty: u8,
        flags: u8,
        channel: u32,
        body: impl Into<FrameBody>,
        taken: Option<Arc<AtomicBool>>,
    ) -> Result<(), Ava1Error> {
        let body = body.into();
        let counted = Counted::new(&self.unwritten);
        self.tx
            .send(Out::Frame {
                ty,
                flags,
                channel,
                body,
                taken,
            })
            .await
            .map_err(|_| Ava1Error::Lost("the connection has ended".into()))?;
        counted.queued();
        Ok(())
    }

    /// Queues a frame only if there is room right now.
    pub(crate) fn try_send<M: FrameMessage>(&self, channel: u32, m: &M) -> Result<(), Full> {
        let body = m.to_bytes().map_err(|_| Full::Closed)?;
        self.try_queue(Out::Frame {
            ty: M::TYPE,
            flags: 0,
            channel,
            body: body.into(),
            taken: None,
        })
    }

    /// Queues a Ping only if there is room right now. The writer stamps its time.
    pub(crate) fn try_ping(&self, seq: u32) -> Result<(), Full> {
        self.try_queue(Out::Ping { seq })
    }

    fn try_queue(&self, o: Out) -> Result<(), Full> {
        let counted = Counted::new(&self.unwritten);
        self.tx.try_send(o).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => Full::Full,
            mpsc::error::TrySendError::Closed(_) => Full::Closed,
        })?;
        counted.queued();
        Ok(())
    }

    /// True once the connection's writer has ended (the queue died with it): a frame
    /// still un-taken then provably never left this process.
    pub(crate) fn writer_dead(&self) -> bool {
        self.writer_dead.load(Ordering::SeqCst)
    }

    /// Nothing is waiting to be written and nothing is being written.
    pub(crate) fn is_idle(&self) -> bool {
        self.unwritten.load(Ordering::SeqCst) == 0
    }

    /// Waits (at most `limit`) until everything queued before this call is on the wire.
    /// Never hangs on a stuck peer: past `limit` it gives up and returns false.
    pub(crate) async fn flush(&self, limit: Duration) -> bool {
        tokio::time::timeout(limit, async {
            let (tx, rx) = oneshot::channel();
            self.tx.send(Out::Flush(tx)).await.ok()?;
            rx.await.ok()
        })
        .await
        .ok()
        .flatten()
        .is_some()
    }
}

type Close = Arc<dyn Fn(String) + Send + Sync>;

pub(crate) struct Link {
    closed: watch::Receiver<Option<String>>,
    rtt_us: Arc<AtomicU64>,
    tasks: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Drop for Link {
    fn drop(&mut self) {
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
    }
}

impl Link {
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.borrow().is_some()
    }

    pub(crate) fn reason(&self) -> String {
        self.closed
            .borrow()
            .clone()
            .unwrap_or_else(|| "closed".into())
    }

    /// Waits until the connection ends; returns why.
    pub(crate) async fn closed(&self) -> String {
        let mut rx = self.closed.clone();
        loop {
            if let Some(r) = rx.borrow_and_update().clone() {
                return r;
            }
            if rx.changed().await.is_err() {
                return "closed".into();
            }
        }
    }

    pub(crate) fn rtt(&self) -> Option<Duration> {
        match self.rtt_us.load(Ordering::Relaxed) {
            0 => None,
            us => Some(Duration::from_micros(us)),
        }
    }
}

pub(crate) fn drive<R, W>(
    mut reader: FrameReader<R>,
    mut writer: FrameWriter<W>,
    timing: Timing,
    deliver: mpsc::Sender<Frame>,
) -> (Link, Outbox)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = watch::channel(None::<String>);
    let tasks: Arc<Mutex<Vec<AbortHandle>>> = Arc::default();
    let last_rx = Arc::new(AtomicU64::new(now_us()));
    let rtt = Arc::new(AtomicU64::new(0));
    let pace = Pace {
        idle: timing.dead_after,
        min_rate: timing.min_frame_rate,
    };
    reader.set_pace(last_rx.clone(), pace);
    writer.set_pace(pace);
    let (out_tx, mut out_rx) = mpsc::channel::<Out>(OUTBOX_DEPTH);
    let unwritten = Arc::new(AtomicUsize::new(0));
    let writer_dead = Arc::new(AtomicBool::new(false));
    let outbox = Outbox {
        tx: out_tx,
        unwritten: unwritten.clone(),
        writer_dead: writer_dead.clone(),
    };

    // Ends the connection: every task is aborted, which drops both socket halves.
    let close: Close = {
        let tasks = tasks.clone();
        Arc::new(move |why: String| {
            let first = tx.send_if_modified(|v| {
                if v.is_some() {
                    return false;
                }
                *v = Some(why.clone());
                true
            });
            if !first {
                return;
            }
            for t in tasks.lock().unwrap().drain(..) {
                t.abort();
            }
        })
    };

    let writer_task = {
        let close = close.clone();
        let writer_dead = writer_dead.clone();
        tokio::spawn(async move {
            let dead = DeadOnDrop(writer_dead);
            while let Some(o) = out_rx.recv().await {
                let sent = match o {
                    Out::Frame {
                        ty,
                        flags,
                        channel,
                        body,
                        taken,
                    } => {
                        // Taken by the writer: from here on the frame may reach the
                        // peer, so its window charge must be held even if the write
                        // then fails (the peer may have part of it).
                        if let Some(t) = &taken {
                            t.store(true, Ordering::SeqCst);
                        }
                        writer.send_with_flags(ty, flags, channel, &body).await
                    }
                    Out::Ping { seq } => {
                        let ping = Ping {
                            seq,
                            t_us: now_us(),
                        };
                        writer.send_msg(0, &ping).await
                    }
                    Out::Flush(ack) => {
                        let _ = ack.send(());
                        continue;
                    }
                };
                if let Err(e) = sent {
                    // The writer is done either way: say so at once (a dead lane's
                    // teardown waits on it), before any grace below.
                    drop(dead);
                    let why = match e {
                        Ava1Error::Timeout => format!(
                            "the other device stopped taking data ({} ms without progress)",
                            timing.dead_after.as_millis()
                        ),
                        e => {
                            // A peer that refuses us writes an Error and then closes: our next
                            // write (a ping) fails before the reader has read that Error, and
                            // "broken pipe" would hide the real reason ("not paired"). Give
                            // the reader a moment; its close, with the peer's reason, wins.
                            tokio::time::sleep(WRITE_FAIL_GRACE).await;
                            format!("write failed: {e}")
                        }
                    };
                    return close(why);
                }
                unwritten.fetch_sub(1, Ordering::SeqCst);
            }
        })
    };

    let reader_task = {
        let (outbox, close, rtt) = (outbox.clone(), close.clone(), rtt.clone());
        tokio::spawn(async move {
            loop {
                let f = match reader.recv().await {
                    Ok(f) => f,
                    Err(e) => return close(e.to_string()),
                };
                match f.ty {
                    Ping::TYPE => {
                        let Ok(p) = f.decode::<Ping>() else {
                            return close("malformed Ping".into());
                        };
                        // A full queue means data is on its way out, which proves we are
                        // alive just as well; a stuck writer ends the link on its own.
                        let pong = Pong {
                            seq: p.seq,
                            t_us: p.t_us,
                        };
                        if outbox.try_send(0, &pong) == Err(Full::Closed) {
                            return close("session ended".into());
                        }
                    }
                    Pong::TYPE => {
                        if let Ok(p) = f.decode::<Pong>() {
                            rtt.store(now_us().saturating_sub(p.t_us).max(1), Ordering::Relaxed);
                        }
                    }
                    Bye::TYPE => return close("the other device closed the session".into()),
                    gen::Error::TYPE => {
                        let why = match f.decode::<gen::Error>() {
                            Ok(e) => {
                                format!("the other device reported error {}: {}", e.code, e.message)
                            }
                            Err(_) => "the other device reported an error".into(),
                        };
                        return close(why);
                    }
                    _ => {
                        if deliver.send(f).await.is_err() {
                            return close("session ended".into());
                        }
                    }
                }
            }
        })
    };

    let heartbeat_task = {
        let (outbox, close, last_rx) = (outbox.clone(), close.clone(), last_rx.clone());
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(timing.ping_every);
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut seq = 0u32;
            let mut prev_tick = now_us();
            let mut deferred = 0u32;
            loop {
                iv.tick().await;
                // A tick that comes late means this process was not running (a stalled
                // runtime, a suspended machine): bytes may be waiting unread, and silence
                // we were not awake to hear is not the peer's. Judge on the next tick,
                // after the reader has had its turn — but only `MAX_DEFERRALS` times in a
                // row: by then the reader has had its turns, and the silence is real.
                let now = now_us();
                let overslept =
                    Duration::from_micros(now.saturating_sub(prev_tick)) > timing.ping_every * 3;
                prev_tick = now;
                // Any byte counts: a large frame still arriving is proof of life.
                let quiet =
                    Duration::from_micros(now.saturating_sub(last_rx.load(Ordering::Relaxed)));
                if quiet <= timing.dead_after {
                    deferred = 0;
                } else if overslept && deferred < MAX_DEFERRALS {
                    deferred += 1;
                } else {
                    return close(format!(
                        "the other device stopped answering ({} ms without a byte)",
                        quiet.as_millis()
                    ));
                }
                seq = seq.wrapping_add(1);
                // Frames queued or on their way out are proof of life for the peer: skip
                // the Ping. (One queued behind a large frame would also measure that
                // frame's write, not the round trip.)
                if outbox.is_idle() && outbox.try_ping(seq) == Err(Full::Closed) {
                    return close("session ended".into());
                }
            }
        })
    };

    tasks.lock().unwrap().extend([
        writer_task.abort_handle(),
        reader_task.abort_handle(),
        heartbeat_task.abort_handle(),
    ]);
    // The connection may already have ended (a task can finish before the handles are
    // registered): make sure nothing is left running.
    if rx.borrow().is_some() {
        for t in tasks.lock().unwrap().drain(..) {
            t.abort();
        }
    }
    (
        Link {
            closed: rx,
            rtt_us: rtt,
            tasks,
        },
        outbox,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::MAX_BODY;
    use crate::gen::RpcResponse;
    use tokio::io::{duplex, split, DuplexStream, ReadHalf, WriteHalf};

    type PeerR = FrameReader<ReadHalf<DuplexStream>>;
    type PeerW = FrameWriter<WriteHalf<DuplexStream>>;

    /// A link over an in-memory pipe that holds only `pipe` bytes, and the other end.
    fn linked(pipe: usize, timing: Timing) -> (Link, Outbox, PeerR, PeerW) {
        let (a, b) = duplex(pipe);
        let ((ar, aw), (br, bw)) = (split(a), split(b));
        let (tx, mut rx) = mpsc::channel(DELIVER_DEPTH);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let (link, outbox) = drive(FrameReader::new(ar), FrameWriter::new(aw), timing, tx);
        let mut peer_r = FrameReader::new(br);
        peer_r.set_max_body(MAX_BODY);
        (link, outbox, peer_r, FrameWriter::new(bw))
    }

    #[tokio::test]
    async fn a_slow_large_write_queues_no_ping_and_does_not_inflate_rtt() {
        let timing = Timing {
            ping_every: Duration::from_millis(200),
            dead_after: Duration::from_secs(20),
            handshake: Duration::from_secs(1),
            min_frame_rate: MIN_FRAME_RATE,
        };
        let (link, outbox, mut peer_r, mut peer_w) = linked(4096, timing);
        // Far larger than the pipe: the write lasts until the peer reads, a second from
        // now. Five heartbeat ticks pass while it is under way.
        let big = RpcResponse {
            status: 0,
            body: vec![0x5a; 200_000],
        };
        outbox.send(7, &big).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert_eq!(
            outbox.unwritten.load(Ordering::SeqCst),
            1,
            "only the large frame: no Ping is queued behind a frame that is being written"
        );
        assert_eq!(peer_r.recv().await.unwrap().ty, RpcResponse::TYPE);
        // The next heartbeat is stamped when it is written, so the second the large
        // frame took is not in its round trip.
        let ping: Ping = peer_r.recv().await.unwrap().decode().unwrap();
        let pong = Pong {
            seq: ping.seq,
            t_us: ping.t_us,
        };
        peer_w.send_msg(0, &pong).await.unwrap();
        let rtt = loop {
            match link.rtt() {
                Some(rtt) => break rtt,
                None => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        };
        assert!(
            rtt < Duration::from_millis(400),
            "RTT includes the write: {rtt:?}"
        );
    }

    #[tokio::test]
    async fn a_ping_is_stamped_when_written_not_when_queued() {
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(1),
            min_frame_rate: MIN_FRAME_RATE,
        };
        let (_link, outbox, mut peer_r, _peer_w) = linked(4096, timing);
        // The interval's first tick is immediate: take that Ping out of the way.
        let first: Ping = peer_r.recv().await.unwrap().decode().unwrap();
        assert_eq!(first.seq, 1);
        // A Ping queued behind a frame that takes 300 ms to leave.
        let big = RpcResponse {
            status: 0,
            body: vec![0x5a; 100_000],
        };
        outbox.send(7, &big).await.unwrap();
        let queued_at = now_us();
        outbox.try_ping(9).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(peer_r.recv().await.unwrap().ty, RpcResponse::TYPE);
        let ping: Ping = peer_r.recv().await.unwrap().decode().unwrap();
        assert_eq!(ping.seq, 9);
        assert!(
            ping.t_us >= queued_at + 250_000,
            "stamped {} us after it was queued",
            ping.t_us.saturating_sub(queued_at)
        );
    }

    #[tokio::test]
    async fn a_runtime_that_keeps_stalling_still_declares_a_silent_peer_dead() {
        let timing = Timing {
            ping_every: Duration::from_millis(50),
            dead_after: Duration::from_millis(200),
            handshake: Duration::from_secs(1),
            min_frame_rate: MIN_FRAME_RATE,
        };
        // The peer's ends stay open (no EOF) and never send a byte.
        let (link, _outbox, _peer_r, _peer_w) = linked(1 << 16, timing);
        // Every heartbeat tick comes late (the runtime's only thread is blocked for four
        // ping intervals at a time), so every tick looks like "we overslept".
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(200));
            // One turn for the other tasks: the heartbeat's overdue tick runs, once (a
            // timed sleep here could let a second, punctual tick in under load).
            tokio::task::yield_now().await;
            if link.is_closed() {
                break;
            }
        }
        assert!(
            link.is_closed(),
            "deferred forever: the link is still open after 2.4 s of silence"
        );
        assert!(
            link.reason().contains("stopped answering"),
            "{}",
            link.reason()
        );
    }
}
