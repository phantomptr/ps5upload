// An internet guest's picture as a WebRTC video track (#762's design): the engine sends the
// console's pictures as RTP (join_net/rtc.rs), and the browser's own jitter buffer, retransmission
// requests (NACK), keyframe requests (PLI) and decoder play them in a <video>. The sound, the
// status and the controller stay on the data channel, so the sound plays through the page's own
// player (pad/watchPlayer) as before. When the engine sends no track (an older engine, the host's
// setting off, an H.265 session this browser has no track for) the same player shows the pictures
// from the channel: this wraps it, and shows whichever is in use.

import type { TrackMedia } from "../lib/joinProtocol";
import type { MsePlayer, SoundReport, WatchPlayer } from "../pad/watchPlayer";

type Inbound = Record<string, number | undefined>;

/** The track's numbers as the page's report to the host (`{"stats": …}`, join.rs PageReport):
 *  the browser's inbound-rtp totals, the decode time and jitter buffer delay averaged since the
 *  last report. Read on demand; getStats is asynchronous, so a report is the last one read. */
export class TrackStats {
  private last: Record<string, number | string> | null = null;
  private was: Inbound = {};

  constructor(private readonly receiver: RTCRtpReceiver) {}

  get frames(): number {
    return Number(this.last?.frames ?? 0);
  }

  /** The browser's numbers into the report's names. */
  static report(s: Inbound, was: Inbound): Record<string, number | string> {
    const n = (k: string) => s[k] ?? 0;
    const d = (k: string) => n(k) - (was[k] ?? 0);
    const per = (sum: string, count: string) => (d(count) > 0 ? Math.round((d(sum) / d(count)) * 10000) / 10 : 0);
    return {
      frames: n("framesDecoded"),
      dropped: n("framesDropped"),
      stalls: n("freezeCount"),
      stall_ms: Math.round(n("totalFreezesDuration") * 1000),
      decode_ms: per("totalDecodeTime", "framesDecoded"),
      jitter_ms: per("jitterBufferDelay", "jitterBufferEmittedCount"),
      nacks: n("nackCount"),
      plis: n("pliCount"),
      decoder: "track",
    };
  }

  /** The last report, and a new read for the next one. */
  snapshot(): Record<string, number | string> | null {
    void this.receiver
      .getStats()
      .then((r) => {
        r.forEach((s: Inbound & { type?: string; kind?: string }) => {
          if (s.type !== "inbound-rtp" || s.kind !== "video") return;
          this.last = TrackStats.report(s, this.was);
          this.was = { ...s };
        });
      })
      .catch(() => {});
    return this.last;
  }
}

export class TrackPlayer {
  readonly video = document.createElement("video");
  private readonly wrap = document.createElement("div");
  private media: TrackMedia | null = null;
  private trackStats: TrackStats | null = null;

  constructor(readonly inner: WatchPlayer | MsePlayer) {
    const v = this.video;
    v.muted = v.autoplay = v.playsInline = true;
    v.className = "block h-full w-full object-contain";
    v.dataset.testid = "track-video";
    this.wrap.className = "h-full w-full";
    this.wrap.append(v, inner.element);
    this.setTrack(null);
  }

  get element(): HTMLElement {
    return this.wrap;
  }

  /** What the picture did: the track's numbers while it plays, else the channel player's. */
  get stats(): { frames: number; snapshot(): Record<string, number | string> | null } {
    return this.trackStats ?? this.inner.stats;
  }

  /** The pictures on this track (a new connection brings a new one), or on the channel (null). */
  setTrack(m: TrackMedia | null): void {
    this.media = m;
    this.trackStats = m ? new TrackStats(m.receiver) : null;
    this.video.style.display = m ? "" : "none";
    this.inner.element.style.display = m ? "none" : "";
    if (this.video.srcObject !== (m?.stream ?? null)) this.video.srcObject = m?.stream ?? null;
    if (m) this.play();
  }

  private play(): void {
    try {
      void this.video.play()?.catch?.(() => {});
    } catch {
      // No media here (a test page).
    }
  }

  /** One binary message from the engine: on the track, its pictures never come here (only the
   *  sound does). */
  onMessage(buf: ArrayBuffer): void {
    if (this.media && new Uint8Array(buf)[0] <= 1) return;
    this.inner.onMessage(buf);
  }

  setHevc(h: boolean): void {
    this.inner.setHevc(h);
  }

  set onSoundFail(f: (() => void) | null) {
    this.inner.onSoundFail = f;
  }

  /** The sound always plays through the channel's player. */
  soundReport(): SoundReport | null {
    return this.inner.soundReport();
  }

  startSound(): void {
    this.inner.startSound();
    if (this.media) this.play();
  }

  setMuted(m: boolean): void {
    this.inner.setMuted(m);
  }

  resumeSound(): void {
    this.inner.resumeSound();
    if (this.media && this.video.paused) this.play();
  }

  stop(): void {
    this.inner.stop();
    this.video.srcObject = null;
  }
}
