import type { HostPower } from "../../lib/consolePower";

/**
 * What TV mode's console card shows (#976), from what the app knows:
 *
 * - `none`: no console yet (Add a PS5, or join a friend's game);
 * - `playing`: a stream is on (Resume);
 * - `setup`: the console is not paired for Remote Play (Set up Remote Play);
 * - `on`, `rest`, `unreachable`, `checking`: paired, and Play starts it. Play wakes a console in
 *   rest mode; one that does not answer may still be reached (Tailscale, a slow network), so Play
 *   stays on offer there too.
 */
export type TvCardState = "none" | "playing" | "setup" | "on" | "rest" | "unreachable" | "checking";

export function tvCardState(s: {
  hasConsole: boolean;
  paired: boolean;
  streaming: boolean;
  power: HostPower;
}): TvCardState {
  if (!s.hasConsole) return "none";
  if (s.streaming) return "playing";
  if (!s.paired) return "setup";
  switch (s.power) {
    case "awake":
      return "on";
    case "rest":
      return "rest";
    case "unreachable":
      return "unreachable";
    default:
      return "checking";
  }
}

/** The card's main button: what it does, and so which one takes the focus. */
export type TvPrimary = "add" | "resume" | "setup" | "play";

export function tvPrimary(state: TvCardState): TvPrimary {
  if (state === "none") return "add";
  if (state === "playing") return "resume";
  if (state === "setup") return "setup";
  return "play";
}

/** The test id of the button the remote starts on: the card's main one, except with no console
 *  of its own, where joining a friend's game is what the screen is for (#926). */
export function tvFocusTarget(primary: TvPrimary): string {
  return primary === "add" ? "tv-join-go" : `tv-${primary}`;
}

/** Back on TV mode's home: asks, and leaves (the app to the background) only on a yes. One
 *  question at a time: a second Back while it is up answers it (the dialog closes as Stay). */
export async function leaveAfterAsking(ask: () => Promise<boolean>, leave: () => void): Promise<boolean> {
  const ok = await ask();
  if (ok) leave();
  return ok;
}
