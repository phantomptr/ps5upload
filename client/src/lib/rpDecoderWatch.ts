// The video decoder's watchdog: access units keep going in, but no picture has come out for a
// while (a decoder that wedged without an error), or the decoder closed under the stream
// (a browser reclaiming it). The stream then rebuilds the decoder from the codec header and asks
// for a keyframe. It stays out of the way of the other recovery paths: a decoder error already
// rebuilds at once, and one still being set up (HEVC probing) is left alone.

/** No picture for this long while access units go in: the decoder is stuck. */
export const STALL_MS = 2000;
/** Access units count as "still going in" when the last one was this recent. A still picture
 *  keeps them coming (the console sends every frame); a paused or hidden stream does not, and
 *  then nothing is judged. */
export const FED_RECENT_MS = 500;
/** Rebuilds in a row with no picture between them before giving up. */
export const MAX_REBUILDS = 3;

export interface DecoderWatchState {
  now: number;
  /** A codec header to rebuild from. */
  haveHeader: boolean;
  /** The decoder's state, or "none" when there is none right now. */
  state: CodecState | "none";
  /** The last access unit handed to the decoder (or queued for it); 0 for none. */
  lastFedAt: number;
  /** The first access unit handed in since the last picture came out (or since the decoder was
   *  made); 0 when none has gone in since. */
  waitingSince: number;
  /** Rebuilds since the last picture came out. */
  rebuilds: number;
}

export type DecoderVerdict = "ok" | "rebuild" | "give_up";

/** How long to wait with no picture before rebuild number `n` (0-based): 2 s, 4 s, then 8 s
 *  (also before giving up): about 22 s from a stuck picture to the player being told. */
export function stallLimit(rebuilds: number): number {
  return STALL_MS * 2 ** Math.min(rebuilds, 2);
}

/** What the watchdog does now. "give_up" once the rebuilds ran out: the caller says so once
 *  and waits for a picture to come out before judging again. */
export function decoderVerdict(s: DecoderWatchState): DecoderVerdict {
  if (!s.haveHeader) return "ok";
  // Nothing going in: a paused stream, a hidden page, the engine waiting for a page.
  if (!s.lastFedAt || s.now - s.lastFedAt > FED_RECENT_MS) return "ok";
  // Being set up (the codec string is still being probed): its own paths report failure.
  if (s.state === "unconfigured") return "ok";
  const stuck = s.state === "closed" || (s.waitingSince > 0 && s.now - s.waitingSince > stallLimit(s.rebuilds));
  if (!stuck) return "ok";
  return s.rebuilds >= MAX_REBUILDS ? "give_up" : "rebuild";
}
