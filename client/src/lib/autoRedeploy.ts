/**
 * The decision behind the auto-redeploy loop (AppShell), as a pure function.
 *
 * It lives here rather than inside the hook because the hook's damage comes
 * from this decision, and a hook that pushes ELFs at :9021 is not directly
 * testable. Every guard below is a regression that cost someone a console or
 * an upload; see `autoRedeployGuard.test.ts` for the ones already paid for.
 */

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
 * - `hold`   — do nothing this tick.
 * - `redeploy` — push the bundled ELF.
 */
export function autoRedeployDecision(input: {
  status: RedeployStatus;
  /** A transfer to this console is in flight. */
  busy: boolean;
  /** Helpers already delivered to this console since it last read up. */
  delivered: number;
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
  if (input.delivered >= MAX_REDEPLOYS_WITHOUT_RECOVERY) return "hold";
  return "redeploy";
}
