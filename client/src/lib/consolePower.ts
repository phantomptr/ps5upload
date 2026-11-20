/**
 * Rest mode, told apart from "off" or "broken".
 *
 * A PS5 in rest mode answers ping and refuses TCP at once, and its discovery
 * port (UDP 9302) replies `620 Server Standby`. An awake console replies 200.
 * One that is off, unplugged, on another network, or resting with its network
 * features turned off says nothing at all. The engine asks DDP when the helper
 * cannot be reached and reports the verdict on the status reply as `power`
 * (and, for rest mode, the reason token `console_rest_mode`).
 *
 * `payloadStatus` keeps its meaning ("down" while asleep); this is the extra
 * fact that lets the app say "in rest mode" calmly instead of "helper isn't
 * running", hold the reconnect loop, and park uploads until the wake.
 */

/** One console's power state as far as the app knows. */
export type HostPower = "awake" | "rest" | "unreachable" | "unknown";

/** The engine's reason token for "the console is in rest mode". */
export const REST_MODE_REASON = "console_rest_mode";

/** The fields of a `payloadCheck` result this module reads. `power` and `reason` are read
 *  defensively: an older engine (or an older client type) does not carry them. */
interface CheckLike {
  reachable?: boolean;
  engineReachable?: boolean;
  session?: string | null;
  /** A helper answered but is too old for this app (#877). */
  helperTooOld?: boolean;
}

function isPower(v: unknown): v is "awake" | "rest" | "unreachable" {
  return v === "awake" || v === "rest" || v === "unreachable";
}

/** The reason token on a status reply, when there is one. */
export function reasonFromCheck(r: CheckLike | null | undefined): string | null {
  const reason = (r as { reason?: unknown } | null | undefined)?.reason;
  return typeof reason === "string" && reason ? reason : null;
}

/**
 * The power state a `payloadCheck` result says, or null when it says nothing
 * about the console (the engine itself did not answer: keep what was known).
 *
 * - helper answering, answering but wanting pairing, or answering but too old → awake;
 * - `power` from the engine, when present and valid;
 * - the reason `console_rest_mode` → rest (an engine that names the reason but
 *   not the field);
 * - anything else → unknown: a refused port alone cannot tell rest mode from a
 *   console whose helper simply is not running.
 */
export function powerFromCheck(r: CheckLike | null | undefined): HostPower | null {
  if (!r) return null;
  if (r.engineReachable === false) return null;
  if (r.reachable || r.session === "needs_pairing" || r.helperTooOld === true) return "awake";
  const power = (r as { power?: unknown }).power;
  if (isPower(power)) return power;
  if (reasonFromCheck(r) === REST_MODE_REASON) return "rest";
  return "unknown";
}

/** Whether a job's failure reason says the console went into rest mode. */
export function isRestModeReason(reason: string | null | undefined): boolean {
  return (reason ?? "").toLowerCase() === REST_MODE_REASON;
}

/**
 * What one console's status light shows: the helper's state, with rest mode
 * and "not answering" told apart from a plain "down".
 */
export type ConsoleLight = "up" | "down" | "rest" | "unreachable" | "unknown" | "pairing";

export function consoleLight(rt: {
  payloadStatus: "up" | "down" | "unknown";
  power?: HostPower;
  session?: string | null;
}): ConsoleLight {
  if (rt.session === "needs_pairing") return "pairing";
  if (rt.payloadStatus === "up") return "up";
  if (rt.payloadStatus === "unknown") return "unknown";
  if (rt.power === "rest") return "rest";
  if (rt.power === "unreachable") return "unreachable";
  return "down";
}

/** The CSS color of a status light. Rest mode is the calm accent, not the fault red. */
export function consoleLightColor(light: ConsoleLight): string {
  switch (light) {
    case "up":
      return "var(--color-good)";
    case "pairing":
      return "var(--color-warn)";
    case "rest":
      return "var(--color-accent)";
    case "down":
    case "unreachable":
      return "var(--color-bad)";
    default:
      return "var(--color-muted)";
  }
}

/** DDP's reply code for "in rest mode" (`HTTP/1.1 620 Server Standby`). */
export const DDP_STANDBY = 620;

/**
 * Whether a console that did not answer on the loader port is asleep: the status reply's own
 * verdict when it has one, otherwise one DDP question (`askDdp` returns the reply code). Used
 * by Connection's Check so a sleeping console is not blamed on a VPN or the jailbreak.
 */
export async function checkSaysRest(
  check: CheckLike | null | undefined,
  askDdp: () => Promise<number>,
): Promise<boolean> {
  const verdict = powerFromCheck(check);
  if (verdict === "rest") return true;
  if (verdict === "awake") return false;
  try {
    return (await askDdp()) === DDP_STANDBY;
  } catch {
    return false;
  }
}

/** The PS5 menu path that decides whether a resting console stays on the network. */
export const REST_MODE_NETWORK_SETTING_PATH =
  "Settings › System › Power Saving › Features Available in Rest Mode";

/**
 * The consoles a keep-awake tick should reach: the wanted ones minus those in
 * rest mode. A tick needs the helper, which died with the sleep; and keeping a
 * console awake is not the same as waking it (a wake stays the user's action).
 */
export function keepAwakeTargets(
  hosts: Iterable<string>,
  runtimeByHost: Record<string, { power?: HostPower } | undefined>,
): string[] {
  const out: string[] = [];
  for (const h of hosts) {
    if (runtimeByHost[h]?.power === "rest") continue;
    out.push(h);
  }
  return out;
}
