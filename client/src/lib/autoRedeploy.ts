/**
 * The decision behind the auto-redeploy loop (AppShell), as a pure function.
 *
 * It lives here rather than inside the hook because the hook's damage comes
 * from this decision, and a hook that pushes ELFs at :9021 is not directly
 * testable. Every guard below is a regression that cost someone a console or
 * an upload; see `autoRedeployGuard.test.ts` for the ones already paid for.
 */

import type { HostPower } from "./consolePower";
import { sessionNeedsAttention, type SessionState } from "./consoleSession";
import { STATUS_SLOW_REASON } from "./statusSchedule";

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

/** After the brake, one more helper this long after the last one delivered, and again each time.
 *
 *  The brake used to be for good: a console that took three helpers without coming back was never
 *  sent another until the user pressed Send, even hours later when whatever stopped it (a loader
 *  still loading kstuff, a console still booting) was long over. One send every quarter hour
 *  cannot become the loop the brake exists to stop. */
export const REDEPLOY_GAVE_UP_RETRY_MS = 15 * 60_000;

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
  /** How long ago the last helper was delivered to this console (null: none this run). Past the
   *  brake, a send is allowed again once this reaches REDEPLOY_GAVE_UP_RETRY_MS. */
  lastDeliveredAgoMs?: number | null;
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
  if (input.delivered >= MAX_REDEPLOYS_WITHOUT_RECOVERY) {
    const ago = input.lastDeliveredAgoMs;
    return ago != null && ago >= REDEPLOY_GAVE_UP_RETRY_MS ? "redeploy" : "hold";
  }
  return "redeploy";
}

/** Whether a status check shows a helper running on the console, so nothing may be sent over it
 *  unasked: it answered, it answered and wants pairing, or it is alive and talking but slow to
 *  answer (`status_slow`: its port took a connection and its open session kept hearing from it).
 *
 *  A slow answer is what a weak Wi-Fi link gives. Sending a helper then replaces the one that is
 *  running, which drops every connection to it, and the console drops in and out over and over. */
export function checkShowsLiveHelper(check: {
  reachable: boolean;
  session?: SessionState | null;
  reason?: string | null;
  helperAlive?: boolean;
  helperTooOld?: boolean;
}): boolean {
  return (
    check.reachable ||
    sessionNeedsAttention(check.session) ||
    check.reason === STATUS_SLOW_REASON ||
    helperAnsweredButUnusable(check)
  );
}

/** The engine's reasons for a helper that answered the handshake in a way this app cannot use
 *  (12.6.15): another app version's helper, a newer one, a reply that does not read. */
export const HELPER_ANSWERED_REASONS: ReadonlySet<string> = new Set([
  "helper_alive_unreadable",
  "helper_too_new",
  "ava1_wrong_console",
]);

/** A helper answered (the engine's `helper_alive`, one of HELPER_ANSWERED_REASONS, or too old):
 *  it is running. Nothing unattended replaces it, whatever its version: two app versions used to
 *  take each other's helper for a dead one and replace it back and forth. A person may (Send
 *  helper), and the screens say why the app leaves it. */
export function helperAnsweredButUnusable(check: {
  reason?: string | null;
  helperAlive?: boolean;
  helperTooOld?: boolean;
}): boolean {
  return (
    check.helperAlive === true ||
    check.helperTooOld === true ||
    (check.reason != null && HELPER_ANSWERED_REASONS.has(check.reason))
  );
}

/** Whether the brake starts over: the console went to rest mode, or was off (no answer of any
 *  kind for two ticks in a row). Either way the next helper goes to a console that woke or booted
 *  again, which is not the one that would not take the last three. */
export function redeployBrakeResets(input: { power?: HostPower; unreachableTicks: number }): boolean {
  return input.power === "rest" || input.unreachableTicks >= 2;
}

/** Why the reconnect loop is not bringing a down console's helper back right now, for the screens
 *  that say "Helper isn't running" (state/redeployStatus). Rest mode and "stopped by you" have
 *  their own screens. */
export type RedeployWhy =
  /** Settings › This PS5 › Reconnect automatically is off. */
  | { kind: "setting-off" }
  /** A transfer to this console is running: never sent over it. */
  | { kind: "busy" }
  /** Nothing listens on the loader (:9021) or Payload Manager: nothing can take a helper. */
  | { kind: "loader-closed" }
  /** The loader just appeared (a console still booting): sent once it has been up `settleMs`. */
  | { kind: "settling"; sendAt: number }
  /** The helper's port still accepts connections: a slow helper is left alone for a while. */
  | { kind: "alive" }
  /** Sending now. */
  | { kind: "sending" }
  /** Three helpers did not bring it back: one more at `retryAt`. */
  | { kind: "gave-up"; retryAt: number }
  /** A helper answers there, only not in a way this app can use (another app version's, older or
   *  newer): never replaced by itself (12.6.15). */
  | { kind: "answering" }
  /** Unattended sends to this console stopped (the engine's `code`): too many this hour (until
   *  `retryAt`), or the console restarted twice right after one, or another console now answers
   *  at the address. A Send helper press starts them again. */
  | { kind: "limit"; code: string; retryAt: number | null }
  /** The PS5 is busy with work a restart would stop: sent once that ends. */
  | { kind: "console-busy" }
  /** This app carries a helper of another version than itself: never sent by itself. */
  | { kind: "stale-build" };

/** What the reconnect loop's screen says when the send itself held back (lib/ensurePayloadCurrent's
 *  `lastEnsureOutcome`: its own reasons, or the engine's refusal codes). null: nothing to add. */
export function redeployWhyFromOutcome(
  o: { why?: string; retryAt?: number | null } | null,
): RedeployWhy | null {
  switch (o?.why) {
    case "send_budget_spent":
    case "console_restarted_after_sends":
    case "different_console":
      return { kind: "limit", code: o.why, retryAt: o.retryAt ?? null };
    case "console-busy":
    case "console_busy":
      return { kind: "console-busy" };
    case "others-busy":
    case "files-busy":
    case "helper_busy":
    case "pkg_install_busy":
      return { kind: "busy" };
    case "stale-build":
    case "helper_version_mismatch":
      return { kind: "stale-build" };
    case "answering":
    case "helper_answers":
      return { kind: "answering" };
    case "settling":
      return o.retryAt ? { kind: "settling", sendAt: o.retryAt } : null;
    case "loader-closed":
      return { kind: "loader-closed" };
    default:
      return null;
  }
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
