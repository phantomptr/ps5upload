/**
 * When the console status poller asks each console again, and what one failed check means.
 *
 * Before, every console was asked once every 10 s whatever happened. So the first check after
 * the app opened, which usually raced the engine's own start and got no answer, left every
 * console "checking" for another 10 s; one failed check right after start called a console with
 * a perfectly good helper "down" (the "Helper isn't running" screen, Connection's way out);
 * and a console that woke was seen only on the next round.
 *
 * Now a check that says nothing (the engine is still starting, or the helper is only slow)
 * is asked again within seconds, and a failure is confirmed by quick checks before it counts:
 * a console not yet seen this run is "down" only after {@link CONFIRM_CHECKS} failures in a row
 * (a few seconds), one that was up only after {@link UP_MISS_THRESHOLD} spread over several
 * seconds. Settled consoles, up, down or asleep, are asked every {@link STATUS_POLL_MS}, and
 * only while the window shows (the poller stops when it is hidden).
 */

/** How often a settled console is asked. */
export const STATUS_POLL_MS = 10_000;

/** Failed checks in a row before a console not yet seen this run counts as down. */
export const CONFIRM_CHECKS = 3;
/** The pause before each confirming check of a console not yet seen (after the 1st, 2nd miss). */
export const CONFIRM_DELAYS_MS = [1_000, 2_000];

/** Failed checks in a row before a console that was up counts as down. */
export const UP_MISS_THRESHOLD = 3;
/** The pauses between them: a busy helper (a long scan) gets several seconds to answer. */
export const UP_MISS_DELAYS_MS = [3_000, 5_000];

/** While the engine is still starting, ask again after this long. */
export const ENGINE_STARTING_RETRY_MS = 1_000;
/** A check the engine could not answer (or the helper was only slow): the first pause before
 *  asking again, doubled each time up to {@link STATUS_POLL_MS}. */
export const BLIND_RETRY_MS = 2_000;

/** After a wake was sent (or the network came back), ask that console this often for a while. */
export const WAKE_POLL_MS = 2_000;
/** How long that lasts. */
export const WAKE_WINDOW_MS = 60_000;

/** The engine's reason token for a status call that timed out on a console still talking. */
export const STATUS_SLOW_REASON = "status_slow";

/** What one check said. */
export type CheckOutcome =
  /** The helper answered (or answered and wants pairing). */
  | "up"
  /** The console said it is in rest mode. */
  | "rest"
  /** The helper did not answer: not running, unreachable, refused. */
  | "miss"
  /** No verdict: the engine did not answer, or the helper is alive but slow. */
  | "blind";

/** The status a console gets after a failed check (`misses`: failures in a row, this one counted). */
export function statusAfterMiss(
  prev: "up" | "down" | "unknown",
  misses: number,
  opts: { upThreshold?: number } = {},
): "up" | "down" | "unknown" {
  const upThreshold = opts.upThreshold ?? UP_MISS_THRESHOLD;
  if (prev === "up") return misses < upThreshold ? "up" : "down";
  if (prev === "unknown") return misses < CONFIRM_CHECKS ? "unknown" : "down";
  return "down";
}

/** How long to wait before asking a console again. */
export function nextCheckDelay(s: {
  outcome: CheckOutcome;
  /** The status it has now (after this check). */
  status: "up" | "down" | "unknown";
  /** Failed checks in a row (for `miss`), or checks without a verdict in a row (for `blind`). */
  streak: number;
  /** The engine has not answered once yet since the app opened. */
  engineStarting?: boolean;
  /** An upload to this console is running: misses there are expected, ask at the normal pace. */
  transferring?: boolean;
  /** A wake (or a network change) asked for quick checks of this console until this time. */
  wakeUntil?: number;
  now?: number;
}): number {
  const now = s.now ?? Date.now();
  const waking = s.wakeUntil !== undefined && now < s.wakeUntil && s.status !== "up";
  let delay = STATUS_POLL_MS;
  if (s.outcome === "blind") {
    delay = s.engineStarting
      ? ENGINE_STARTING_RETRY_MS
      : Math.min(BLIND_RETRY_MS * 2 ** Math.max(0, s.streak - 1), STATUS_POLL_MS);
  } else if (s.outcome === "miss" && !s.transferring) {
    // Confirming checks, while the failure is not yet believed.
    if (s.status === "unknown") delay = CONFIRM_DELAYS_MS[s.streak - 1] ?? STATUS_POLL_MS;
    else if (s.status === "up") delay = UP_MISS_DELAYS_MS[s.streak - 1] ?? STATUS_POLL_MS;
  }
  return waking ? Math.min(delay, WAKE_POLL_MS) : delay;
}
