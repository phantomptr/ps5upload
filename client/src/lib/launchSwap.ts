// Starting a game while another one is running.
//
// Asking the console to start a second game over a running one does not work from here: the
// launcher answers "already running, kill needed" (0x80940010), the running game is closed,
// the new one never starts, and the next launch is refused once more. Measured on a Pro and
// a Phat (FW 13.60, ShadowMount+ 1.7), 2026-10-07. What does work, measured the same night:
// close the running game with the app's own Stop, wait until it is gone, give the console a
// few seconds (ShadowMount+ puts things back when it sees the game stop), then launch.
//
// The PS5 asks before closing a game for another one, and so does this.

import type { RunningGame } from "./runningGames";

/** How long the console is given after the old game is gone. 8 s was enough on the Phat. */
export const SETTLE_AFTER_CLOSE_MS = 8_000;
/** How long the closed game has to leave the process list. */
const GONE_POLL_MS = 500;
const GONE_TRIES = 30;

export interface SwapDeps {
  /** The games running now, by title id. */
  running: () => Promise<Map<string, RunningGame>>;
  /** Ask the user whether to close `other`. */
  confirm: (other: RunningGame) => Promise<boolean>;
  /** Close `other`; false when it could not be closed. */
  close: (other: RunningGame) => Promise<boolean>;
  sleep: (ms: number) => Promise<void>;
}

export type SwapOutcome =
  /** Nothing else was running: launch. */
  | "clear"
  /** The other game was closed and the console has settled: launch. */
  | "closed"
  /** The user kept the running game: do not launch. */
  | "cancelled"
  /** The running game would not close: do not launch over it. */
  | "close_failed";

/** Makes way for `titleId`. Launch only on "clear" or "closed". */
export async function closeRunningGameFirst(
  titleId: string,
  deps: SwapDeps,
): Promise<SwapOutcome> {
  let others: RunningGame[];
  try {
    others = [...(await deps.running()).values()].filter(
      (g) => g.titleId !== titleId,
    );
  } catch {
    return "clear";
  }
  if (others.length === 0) return "clear";
  const other = others[0];
  if (!(await deps.confirm(other))) return "cancelled";
  for (const g of others) {
    if (!(await deps.close(g))) return "close_failed";
  }
  for (let i = 0; i < GONE_TRIES; i++) {
    let left: Map<string, RunningGame>;
    try {
      left = await deps.running();
    } catch {
      left = new Map();
    }
    if (![...left.keys()].some((id) => id !== titleId)) {
      await deps.sleep(SETTLE_AFTER_CLOSE_MS);
      return "closed";
    }
    await deps.sleep(GONE_POLL_MS);
  }
  return "close_failed";
}
