/**
 * The decision behind the auto-redeploy loop (AppShell), as a pure function.
 *
 * It lives here rather than inside the hook because the hook's damage comes
 * from this decision, and a hook that pushes ELFs at :9021 is not directly
 * testable. Every guard below is a regression that cost someone a console or
 * an upload; see `autoRedeployGuard.test.ts` for the ones already paid for.
 */

import type { HostPower } from "./consolePower";

/** A console's probed state, as far as the redeploy loop cares. */
export type RedeployStatus = "unknown" | "up" | "down" | undefined;

/** How many helpers the loop will DELIVER to one console with no recovery in
 *  between before it stops and says so.
 *
 *  The loop's premise is that a console which lost its helper (rest mode,
 *  wake, blip) will accept a new one and come back. When that stops being
 *  true, retrying is actively harmful: every delivered ELF replaces whatever
 *  runs at :9021, so a loop that keeps firing keeps killing each helper just
 *  as it starts, and the console never gets a chance to answer. Three
 *  attempts rides out a genuinely slow boot; past that the honest answer is
 *  to stop and tell the user.
 *
 *  Only DELIVERED sends count (AppShell's `deliveredRef`): while a console
 *  sleeps every connect fails, and that path must keep trying indefinitely —
 *  it is the case this feature exists for. The count resets on any "up". */
export const MAX_REDEPLOYS_WITHOUT_RECOVERY = 3;

export type RedeployDecision = "redeploy" | "hold" | "rearm";

/**
 * What the loop should do for one console right now.
 *
 * - `rearm`  — the console is up: clear its delivered-send count.
 * - `hold`   — do nothing this tick (also: the user stopped this console's helper).
 * - `redeploy` — push the bundled ELF.
 */
export function autoRedeployDecision(input: {
  status: RedeployStatus;
  /** A transfer to this console is in flight. */
  busy: boolean;
  /** Helpers already delivered to this console since it last read up. */
  delivered: number;
  /** The user stopped this console's helper (state/helperStopped): it stays off until they
   *  send it again (#471). */
  stopped?: boolean;
  /** The console's power state (lib/consolePower). In rest mode nothing listens and the
   *  helper cannot start until someone wakes it: probing it every 15 s all night only
   *  costs the console connects. The normal flow resumes once it reads awake again. */
  power?: HostPower;
}): RedeployDecision {
  if (input.status === "up") return "rearm";
  // "unknown" is a console the user typed in but never deliberately
  // connected to — that's the manual Connect flow's job, and a console whose
  // state we could not learn at all (engine unreachable) must never be
  // redeployed on the strength of a missing verdict.
  if (input.status !== "down") return "hold";
  // Never over a live transfer: the new instance takes over and the old one
  // shuts down, killing the upload mid-flight.
  if (input.busy) return "hold";
  // A helper the user turned off is not a helper that went missing.
  if (input.stopped) return "hold";
  // Asleep: the wake is the user's to make, and the helper is sent after it.
  if (input.power === "rest") return "hold";
  if (input.delivered >= MAX_REDEPLOYS_WITHOUT_RECOVERY) return "hold";
  return "redeploy";
}

/** How many redeploy ticks (15 s each) a helper that still accepts
 *  connections is left alone before the loop may replace it anyway.
 *
 *  A failed STATUS is not a dead helper. A FW 12.70 report caught the helper port
 *  accepting connections while STATUS failed, and its helper reported
 *  "prior_instance": "replaced" — started over a predecessor that was still
 *  running. A process that still owns our ports is alive, and a push replaces
 *  it, which drops the connection again. A minute still lets a genuinely
 *  wedged helper be replaced. */
export const MAX_LIVE_HELPER_HOLDS = 4;

/**
 * Whether to push a helper at a console that failed its status probe, given
 * whether our own ports still accept connections there.
 *
 * - `send` — nothing of ours is listening: the helper is gone.
 * - `hold` — a helper process is still there; leave it (and count the hold).
 */
export function liveHelperDecision(input: {
  /** :9120 (the helper's AVA1 port) accepted a TCP connection just now. */
  portsOpen: boolean;
  /** Consecutive holds for this console so far. */
  held: number;
}): "send" | "hold" {
  if (!input.portsOpen) return "send";
  return input.held < MAX_LIVE_HELPER_HOLDS ? "hold" : "send";
}

/** How long the loader (:9021) must have been answering before the loop pushes a helper
 *  at a console that is down.
 *
 *  On a cold boot the jailbreak's own autoloader brings elfldr up first and only then
 *  loads kstuff and the rest of the chain. The loader port is open for that whole window,
 *  and a helper pushed into it lands before the kernel patches it relies on; an FW 11.40
 *  report (kstuff test6 + SMP 1.7) reproduced a crash three times out of three by having
 *  the app running while the console booted. The user's own sends are not held: this is
 *  only the unattended loop. A wake from rest mode pays the same wait once. */
export const LOADER_SETTLE_MS = 90_000;

/**
 * Whether the loader has been up long enough for an unattended push.
 *
 * `firstOpenAt` is when the loop first saw :9021 open on this console since it went
 * down (undefined if this tick is the first sighting).
 */
export function loaderSettled(input: {
  loaderOpen: boolean;
  firstOpenAt: number | undefined;
  now: number;
  /** How long to wait; longer on a console whose Payload Manager starts the helper at boot
   *  (lib/helperAtBoot), so this loop does not send a second one while that one is coming. */
  settleMs?: number;
}): "send" | "settle" | "closed" {
  if (!input.loaderOpen) return "closed";
  const since = input.firstOpenAt ?? input.now;
  return input.now - since >= (input.settleMs ?? LOADER_SETTLE_MS) ? "send" : "settle";
}

/** What the loop sends to a down console whose loader has settled.
 *
 * - `bring-up` — Quick bring-up (bring-up playlist, then the helper): the "Bring up
 *   automatically" setting is on and the helper has not been up since the console last
 *   started (#479). Once per boot: re-sending kstuff/ShadowMount+ to a console that already
 *   runs them is not something to do unasked.
 * - `helper` — just the helper, the reconnect setting's job (rest mode, a crash, a drop).
 * - `none` — neither setting covers this case.
 */
export type RedeployAction = "bring-up" | "helper" | "none";

export function redeployAction(input: {
  /** Settings → Bring up automatically (default off). */
  autoBringUp: boolean;
  /** Settings → Reconnect automatically after rest mode / network drops (default on). */
  autoRedeploy: boolean;
  /** The helper read up on this console since it was last seen switched off (or since the
   *  app started). */
  upThisBoot: boolean;
}): RedeployAction {
  if (input.autoBringUp && !input.upThisBoot) return "bring-up";
  return input.autoRedeploy ? "helper" : "none";
}
