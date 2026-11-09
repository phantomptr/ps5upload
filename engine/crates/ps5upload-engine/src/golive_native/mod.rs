//! Go live without ffmpeg (#763): what the phone apps stream with, since a phone can't run
//! ffmpeg. The same run as rp_golive.rs (its phases, backoff, per-site states and status), with
//! the ffmpeg process replaced by the engine itself:
//!
//! * The live feed's frames as the console sent them (rp_live.rs `next_event`), not MPEG-TS.
//! * Video untouched: each access unit re-framed for FLV (flv.rs), H.264 or HEVC (enhanced
//!   RTMP), its decoder configuration from the console's parameter sets. Times from the same
//!   smoothed clock the TS feed uses (rp_live_ts.rs's PtsClock).
//! * Sound: the console's Opus decoded and encoded to AAC-LC by the device's own codecs
//!   (audio.rs: AudioToolbox on iPhone/iPad/Mac, MediaCodec on Android).
//! * One RTMP(S) connection per site (rtmp.rs), each fed from its own short queue. A site that
//!   falls behind skips to the next keyframe (counted as an upload skip) without holding the
//!   others; one that fails is let back in after a minute on its own (the ffmpeg run restarts
//!   every site for that), or left out by the same rules (`slave_failed`). With one site, a
//!   failure ends the attempt and the run's backoff takes over, as with ffmpeg.
//! * A connection that takes longer than `connect_stall_after` to publish, or a write that
//!   blocks for `stall_after`, is a dead connection.
//!
//! SRT (Custom srt:// addresses) needs ffmpeg: refused at the start with `srt_unsupported`.
//! The desktop engine keeps ffmpeg; `PS5UPLOAD_GOLIVE=native` makes it use this instead (macOS,
//! where AudioToolbox is; elsewhere there is no sound converter and ffmpeg stays).

mod amf;
pub(crate) mod audio;
mod flv;
mod rtmp;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(test)]
pub(crate) mod test_server;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::{failure_code, rank, slave_failed, Exit, Problem, Run};
use crate::golive_ffmpeg::{Failure, Progress};
use crate::rp_live_ts::{PtsClock, Unwrap};
use crate::rp_stream_api::live_feed::{FeedEvent, FeedReader};
use audio::{AudioCodecs, AudioTrack};

/// Tags waiting for one site: about 3 s of 60 fps video with its sound.
const SINK_QUEUE: usize = 600;
/// How long a site gets to take the unpublish when the run stops.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// Upload skips this recent mean the upload can't keep up.
const SLOW_WINDOW: Duration = Duration::from_secs(5);

/// One FLV tag for the sites: its type (8 audio, 9 video), time (ms since the run's first
/// keyframe) and body.
#[derive(Debug)]
pub(crate) struct Tag {
    kind: u8,
    ts: u32,
    key: bool,
    body: Vec<u8>,
}

impl Tag {
    fn at(&self, ts: u32) -> Arc<Tag> {
        Arc::new(Tag {
            kind: self.kind,
            ts,
            key: self.key,
            body: self.body.clone(),
        })
    }
}

#[derive(Debug)]
struct SinkEnd {
    failure: Failure,
    /// Seconds of media the site took.
    media_s: f64,
    ran: Duration,
    detail: String,
}

enum SinkEvent {
    /// The first keyframe went out: the site is live.
    Live,
    Ended(SinkEnd),
}

struct Sink {
    tx: mpsc::Sender<Arc<Tag>>,
    task: tokio::task::JoinHandle<()>,
    /// Nothing until the next keyframe (new, or its queue was full).
    waiting_key: bool,
}

/// Hands `tag` to every site. A site that starts (or starts again after falling behind) gets
/// the codec configurations right before its keyframe; one whose queue is full skips to the
/// next keyframe.
fn broadcast(
    sinks: &mut [Option<Sink>],
    tag: &Arc<Tag>,
    video_cfg: Option<&Arc<Tag>>,
    cfg_changed: bool,
    audio_cfg: &Arc<Tag>,
    skips: &mut u64,
) {
    let is_key = tag.kind == flv::TAG_VIDEO && tag.key;
    for s in sinks.iter_mut().flatten() {
        if s.waiting_key {
            if !is_key {
                continue;
            }
            let Some(v) = video_cfg else { continue };
            let ok = s.tx.try_send(v.at(tag.ts)).is_ok()
                && s.tx.try_send(audio_cfg.at(tag.ts)).is_ok()
                && s.tx.try_send(tag.clone()).is_ok();
            s.waiting_key = !ok;
            continue;
        }
        let cfg_ok = match (cfg_changed && is_key, video_cfg) {
            (true, Some(v)) => s.tx.try_send(v.at(tag.ts)).is_ok(),
            _ => true,
        };
        if !cfg_ok || s.tx.try_send(tag.clone()).is_err() {
            s.waiting_key = true;
            *skips += 1;
        }
    }
}

/// Everything a site's connection needs.
#[derive(Clone)]
struct SinkCfg {
    url: String,
    meta: Arc<Vec<u8>>,
    connect_limit: Duration,
    write_limit: Duration,
}

/// One site: connect and publish, then send what comes until the queue closes (the run
/// stopped: unpublish) or the connection fails.
async fn sink(cfg: SinkCfg, mut rx: mpsc::Receiver<Arc<Tag>>, live: impl Fn()) -> SinkEnd {
    let started = Instant::now();
    let end = |e: rtmp::RtmpError, media_ms: u32| SinkEnd {
        failure: e.failure(),
        media_s: media_ms as f64 / 1000.0,
        ran: started.elapsed(),
        detail: e.to_string(),
    };
    let Some(url) = rtmp::parse_url(&cfg.url) else {
        return end(
            rtmp::RtmpError::Unreachable("not an rtmp:// or rtmps:// address".into()),
            0,
        );
    };
    let tls = rtmp::public_tls();
    let up = tokio::time::timeout(cfg.connect_limit, async {
        let io = rtmp::connect(&url, tls.as_ref()).await?;
        rtmp::Publisher::publish(io, &url).await
    })
    .await;
    let mut p = match up {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return end(e, 0),
        Err(_) => return end(rtmp::RtmpError::Timeout("connect"), 0),
    };
    match tokio::time::timeout(cfg.write_limit, p.send(flv::TAG_DATA, 0, &cfg.meta)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return end(e, 0),
        Err(_) => return end(rtmp::RtmpError::Timeout("send"), 0),
    }
    let mut base: Option<u32> = None;
    let mut media_ms = 0u32;
    let mut is_live = false;
    loop {
        tokio::select! {
            m = rx.recv() => {
                let Some(tag) = m else {
                    // The run stopped: unpublish, so the site ends the broadcast cleanly.
                    let _ = tokio::time::timeout(CLOSE_GRACE, p.close()).await;
                    return SinkEnd {
                        failure: Failure::Other,
                        media_s: media_ms as f64 / 1000.0,
                        ran: started.elapsed(),
                        detail: "stopped".into(),
                    };
                };
                // Times start at this site's first keyframe (its configuration comes with it).
                let b = *base.get_or_insert(tag.ts);
                if tag.ts < b {
                    continue;
                }
                let ts = tag.ts - b;
                match tokio::time::timeout(cfg.write_limit, p.send(tag.kind, ts, &tag.body)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return end(e, media_ms),
                    Err(_) => return end(rtmp::RtmpError::Timeout("send"), media_ms),
                }
                media_ms = media_ms.max(ts);
                if tag.kind == flv::TAG_VIDEO && tag.key && !is_live {
                    is_live = true;
                    live();
                }
            }
            r = p.read_once() => {
                if let Err(e) = r.and_then(|cmds| rtmp::Publisher::<Box<dyn rtmp::Conn>>::check_status(&cmds)) {
                    return end(e, media_ms);
                }
            }
        }
    }
}

fn spawn_sink(
    cfg: SinkCfg,
    pos: usize,
    gen: u64,
    events: mpsc::UnboundedSender<(usize, u64, SinkEvent)>,
) -> Sink {
    let (tx, rx) = mpsc::channel(SINK_QUEUE);
    let task = tokio::spawn(async move {
        let ev = events.clone();
        let end = sink(cfg, rx, move || {
            let _ = ev.send((pos, gen, SinkEvent::Live));
        })
        .await;
        let _ = events.send((pos, gen, SinkEvent::Ended(end)));
    });
    Sink {
        tx,
        task,
        waiting_key: true,
    }
}

/// The run's sites `idx`, fed from `reader`, until the session ends, the run stops, or (one
/// site) it fails or (several) every one of them is out.
pub(super) async fn run(
    r: &Arc<Run>,
    mut reader: FeedReader,
    idx: &[usize],
    codecs: Arc<dyn AudioCodecs>,
) -> Exit {
    let started = Instant::now();
    let session = reader.session.clone();
    let hevc = session.codec != "h264";
    let channels = reader.channels.clamp(1, 2);
    let (dec, enc) = match codecs.open(channels, r.audio_kbps) {
        Ok(p) => p,
        Err(e) => {
            return Exit {
                fatal: Some(Problem {
                    code: "audio_failed",
                    message: format!(
                        "This device couldn't convert the sound for streaming sites ({}: {e}).",
                        codecs.name()
                    ),
                }),
                ..Default::default()
            }
        }
    };
    let mut audio = AudioTrack::new(dec, enc, channels);
    let audio_cfg = Arc::new(Tag {
        kind: flv::TAG_AUDIO,
        ts: 0,
        key: false,
        body: flv::audio_config_tag(&flv::aac_lc_config(audio::RATE, channels)),
    });
    let meta = Arc::new(flv::metadata(&flv::MetaInfo {
        width: session.width,
        height: session.height,
        fps: session.fps,
        video_kbps: session.bitrate_kbps,
        hevc,
        audio_kbps: r.audio_kbps,
        channels,
    }));
    let cfg_for = |i: usize| SinkCfg {
        url: r.dests[i].out.url.clone(),
        meta: meta.clone(),
        connect_limit: r.timing.connect_stall_after,
        write_limit: r.timing.stall_after,
    };
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<(usize, u64, SinkEvent)>();
    let mut gens = vec![0u64; idx.len()];
    let mut sinks: Vec<Option<Sink>> = idx
        .iter()
        .enumerate()
        .map(|(p, &i)| Some(spawn_sink(cfg_for(i), p, 0, ev_tx.clone())))
        .collect();
    let mut rejoin_at: Vec<Option<Instant>> = vec![None; idx.len()];

    let mut origin: Option<Instant> = None;
    let mut vidx = Unwrap::default();
    let mut vclock = PtsClock::new(90_000 / session.fps.clamp(1, 240) as u64, 0.1);
    let mut video_cfg: Option<Arc<Tag>> = None;
    let mut frames = 0u64;
    let mut media_s = 0f64;
    let mut skips = 0u64;
    let skips_before = r.st.lock().map(|g| g.upload_skips).unwrap_or(0);
    let mut last_skip: Option<Instant> = None;
    let mut worst: Option<Failure> = None;
    // The single site's end, which is the attempt's.
    let mut single: Option<SinkEnd> = None;
    let mut fatal: Option<Problem> = None;
    let mut session_ended = false;
    let (mut last_sent, mut last_frames, mut last_media, mut last_tick) =
        (r.sent.load(Ordering::Relaxed), 0u64, 0f64, Instant::now());
    let ticks_since =
        |o: Instant, at: Instant| at.saturating_duration_since(o).as_micros() as f64 * 0.09;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.tick().await;

    loop {
        tokio::select! {
            ev = reader.next_event() => {
                let Some(ev) = ev else {
                    session_ended = true;
                    break;
                };
                match ev {
                    FeedEvent::Video { au, index, at, key, header } => {
                        if origin.is_none() && !key {
                            continue;
                        }
                        let o = *origin.get_or_insert(at);
                        let pts = vclock.pts(vidx.next(index), ticks_since(o, at));
                        let ts = (pts / 90) as u32;
                        let mut cfg_changed = false;
                        if key {
                            let cfg = header
                                .as_deref()
                                .and_then(|h| flv::video_config(h, hevc))
                                .or_else(|| flv::video_config(&au, hevc));
                            if let Some(c) = cfg {
                                let body = flv::video_config_tag(&c, hevc);
                                if video_cfg.as_ref().is_none_or(|v| v.body != body) {
                                    video_cfg = Some(Arc::new(Tag { kind: flv::TAG_VIDEO, ts, key: false, body }));
                                    cfg_changed = true;
                                }
                            }
                        }
                        if video_cfg.is_none() {
                            continue; // nothing a viewer could decode yet
                        }
                        let data = flv::frame_data(&au, hevc);
                        if data.is_empty() {
                            continue;
                        }
                        let tag = Arc::new(Tag { kind: flv::TAG_VIDEO, ts, key, body: flv::video_frame_tag(&data, key, hevc) });
                        r.sent.fetch_add(tag.body.len() as u64, Ordering::Relaxed);
                        frames += 1;
                        media_s = pts as f64 / 90_000.0;
                        let before = skips;
                        broadcast(&mut sinks, &tag, video_cfg.as_ref(), cfg_changed, &audio_cfg, &mut skips);
                        if skips > before {
                            last_skip = Some(Instant::now());
                        }
                    }
                    FeedEvent::Audio { opus, index, at } => {
                        let Some(o) = origin else { continue };
                        match audio.push(&opus, index, ticks_since(o, at)) {
                            Ok(out) => {
                                for (t, f) in out {
                                    let tag = Arc::new(Tag { kind: flv::TAG_AUDIO, ts: (t / 90) as u32, key: false, body: flv::audio_frame_tag(&f) });
                                    r.sent.fetch_add(tag.body.len() as u64, Ordering::Relaxed);
                                    broadcast(&mut sinks, &tag, video_cfg.as_ref(), false, &audio_cfg, &mut skips);
                                }
                            }
                            Err(e) => {
                                fatal = Some(Problem { code: "audio_failed", message: format!("This device's sound converter stopped ({}: {e}).", codecs.name()) });
                                break;
                            }
                        }
                    }
                }
            }
            Some((p, gen, ev)) = ev_rx.recv() => {
                if gen != gens[p] {
                    continue;
                }
                let i = idx[p];
                match ev {
                    SinkEvent::Live => {
                        let now = Instant::now();
                        r.set(|s| {
                            if s.phase != "live" {
                                s.phase = "live";
                                s.error = None;
                                s.retry_at = None;
                            }
                            s.live_since.get_or_insert(now);
                            let d = &mut s.dests[i];
                            if d.state != "failed" {
                                d.state = "live";
                                d.error = None;
                            }
                        });
                    }
                    SinkEvent::Ended(e) => {
                        sinks[p] = None;
                        if r.stopped() {
                            continue;
                        }
                        crate::log_at!(Info, "golive", "golive: {} ended after {:.0} s: {}", r.dests[i].preset.name, e.ran.as_secs_f64(), e.detail);
                        if worst.is_none_or(|w| rank(e.failure) > rank(w)) {
                            worst = Some(e.failure);
                        }
                        if idx.len() == 1 {
                            single = Some(e);
                            break;
                        }
                        let site = r.dests[i].preset.name;
                        let rejoin_after = r.timing.rejoin_after;
                        let mut back = None;
                        r.set(|s| {
                            let d = &mut s.dests[i];
                            let (f, stay_out) = slave_failed(Some(e.failure), e.media_s, e.ran, &mut d.early_fails, d.rejoins);
                            d.error = Some(Problem { code: failure_code(f), message: f.message(site) });
                            if stay_out {
                                d.state = "failed";
                            } else {
                                d.state = "retrying";
                                d.rejoins += 1;
                                back = Some(Instant::now() + rejoin_after);
                            }
                        });
                        rejoin_at[p] = back;
                        if sinks.iter().all(Option::is_none) && rejoin_at.iter().all(Option::is_none) {
                            break;
                        }
                    }
                }
            }
            _ = r.wake.notified() => {
                if r.stopped() {
                    break;
                }
            }
            _ = tick.tick() => {
                if r.stopped() {
                    break;
                }
                let now = Instant::now();
                let secs = now.saturating_duration_since(last_tick).as_secs_f64().max(0.001);
                let sent = r.sent.load(Ordering::Relaxed);
                let kbps = (sent - last_sent) as f64 * 8.0 / 1000.0 / secs;
                let fps = (frames - last_frames) as f64 / secs;
                let speed = (media_s - last_media) / secs;
                (last_sent, last_frames, last_media, last_tick) = (sent, frames, media_s, now);
                let feed_skips = reader.stats.dropped_slow.load(Ordering::Relaxed);
                let slow = last_skip.is_some_and(|t| now.saturating_duration_since(t) < SLOW_WINDOW);
                r.set(|s| {
                    s.kbps = if s.kbps == 0.0 { kbps } else { s.kbps * 0.6 + kbps * 0.4 };
                    s.upload_skips = skips_before + skips + feed_skips;
                    s.slow_upload = slow;
                    s.progress = Some(Progress {
                        frame: frames,
                        fps,
                        bitrate_kbps: Some(kbps),
                        total_size: sent,
                        out_time_s: media_s,
                        speed: (media_s > 0.0).then_some(speed),
                        ..Default::default()
                    });
                });
                for p in 0..idx.len() {
                    if rejoin_at[p].is_some_and(|t| now >= t) && sinks[p].is_none() {
                        rejoin_at[p] = None;
                        gens[p] += 1;
                        let i = idx[p];
                        crate::log_at!(Info, "golive", "golive: letting {} back in", r.dests[i].preset.name);
                        r.set(|s| {
                            if s.dests[i].state != "failed" {
                                s.dests[i].state = "connecting";
                            }
                        });
                        sinks[p] = Some(spawn_sink(cfg_for(i), p, gens[p], ev_tx.clone()));
                    }
                }
            }
        }
    }

    // Close every site: the queue's end unpublishes, then the connection goes.
    let deadline = Instant::now() + r.timing.stop_grace;
    let tasks: Vec<_> = sinks
        .into_iter()
        .flatten()
        .map(|s| {
            drop(s.tx);
            s.task
        })
        .collect();
    for t in tasks {
        let left = deadline.saturating_duration_since(Instant::now());
        let abort = t.abort_handle();
        if tokio::time::timeout(left, t).await.is_err() {
            abort.abort();
        }
    }
    let feed_skips = reader.stats.dropped_slow.load(Ordering::Relaxed);
    r.set(|s| s.upload_skips = skips_before + skips + feed_skips);
    crate::log_at!(
        Info,
        "golive",
        "golive: native run ended after {:.0} s: {frames} frames, {} sound packets lost",
        started.elapsed().as_secs_f64(),
        audio.lost
    );
    let quiet = r.stopped() || session_ended || fatal.is_some();
    match single {
        Some(e) if !quiet => Exit {
            ran: e.ran,
            out_time_s: e.media_s,
            failure: Some(e.failure),
            ..Default::default()
        },
        _ => Exit {
            ran: started.elapsed(),
            out_time_s: media_s,
            failure: if quiet {
                None
            } else {
                worst.or(Some(Failure::Other))
            },
            session_ended,
            fatal,
            ..Default::default()
        },
    }
}

/// Go live without ffmpeg is what this engine streams with: always on a phone, and on a computer
/// when `PS5UPLOAD_GOLIVE=native` asks for it.
pub(crate) fn wanted() -> bool {
    cfg!(any(target_os = "android", target_os = "ios"))
        || std::env::var("PS5UPLOAD_GOLIVE").is_ok_and(|v| v.trim().eq_ignore_ascii_case("native"))
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
