// Decides what the Library move poller should do when an FS_OP_STATUS
// call fails. Pulled out as a pure function so the policy is unit-
// testable without spinning up React state.
//
// The poller had three failure modes baked into it before this:
//
//   1. A fuzzy substring match on the engine's error string ("looks
//      like an old payload, latch the banner forever") — gaslit users
//      on the latest payload whenever a transient error happened to
//      contain "decode FS_OP_STATUS_ACK body" in its chain.
//   2. No tolerance for transient errors — one bad poll latched the
//      banner for the rest of the move, even on healthy payloads.
//   3. The diagnosis text was version-specific ("predates 2.2.16") but
//      the trigger was an error-string match, so the version named in
//      the message was a guess from the substring rather than from
//      the actually-running payload.
//
// New policy:
//
//   - If we know the running payload version (the Connection screen's
//     STATUS probe set `payloadVersion`) and it's older than the
//     threshold for the bug we'd be hitting, latch the right banner
//     immediately. This is honest: we know it's old, the message says
//     so.
//   - If the version is current, swallow transient errors and stop
//     polling silently after N consecutive failures. Never show the
//     "old payload" banner — it would be a lie.
//   - If the version is unknown (probe hasn't completed, or the
//     payload is so old it doesn't report a version), fall back to
//     the old substring-match heuristic *only* after N consecutive
//     failures, so a single transient blip can't trip it.

import { compareVersions } from "./semver";

/** What to do after a single failed FS_OP_STATUS poll. */
export type MovePollOutcome =
  | { kind: "retry" }
  | { kind: "stop-silent" }
  | { kind: "stop-old-payload"; threshold: "2.2.7" | "2.2.16" };

/** N consecutive non-404 failures before we give up. 5 × 500 ms = 2.5 s
 *  of repeated failure — long enough to ride out a TCP reconnect or a
 *  brief mgmt-port stall, short enough that a truly broken setup
 *  doesn't keep the user staring at a "Copying…" line forever. */
export const CONSECUTIVE_FAILURE_LIMIT = 5;

/** Version thresholds where the FS_OP_STATUS path acquired/fixed
 *  capabilities. Kept here so the policy is self-contained. */
const FS_OP_STATUS_INTRODUCED = "2.2.7";
const FS_OP_STATUS_ACK_BUFFER_FIX = "2.2.16";

export function classifyMovePollError(
  payloadVersion: string | null,
  errorMsg: string,
  consecutiveFailures: number,
): MovePollOutcome {
  // Probe the version once. compareVersions returns null on any
  // unparseable input — we treat that the same as the probe never
  // running (not as "looks current"), so a malformed version string
  // can't accidentally suppress an old-payload banner.
  const introCmp = payloadVersion
    ? compareVersions(payloadVersion, FS_OP_STATUS_INTRODUCED)
    : null;
  const bufferCmp = payloadVersion
    ? compareVersions(payloadVersion, FS_OP_STATUS_ACK_BUFFER_FIX)
    : null;
  const versionKnown = introCmp !== null && bufferCmp !== null;

  // Known-old payload: fail fast with the right banner. This is the
  // only path that can latch the user-visible "your payload is old"
  // message when the version probe is healthy — and it only fires
  // when the version actually *is* old.
  if (versionKnown) {
    if (introCmp === -1) {
      return { kind: "stop-old-payload", threshold: "2.2.7" };
    }
    if (bufferCmp === -1) {
      return { kind: "stop-old-payload", threshold: "2.2.16" };
    }
  }

  // Tolerate transient errors. The poller is a side-channel — the
  // actual move runs on its own connection and isn't affected by a
  // few failed status probes. Better to ride out a brief network
  // stall than to give up after one hiccup.
  if (consecutiveFailures < CONSECUTIVE_FAILURE_LIMIT) {
    return { kind: "retry" };
  }

  // Repeatedly failing. If the version probe never produced a usable
  // value (very old payload that doesn't report version, the probe
  // hasn't run yet on this host, or the version string was
  // malformed) AND the error string matches a known old-payload
  // signature, surface the matching banner. For a known-current
  // payload reaching this branch (real transient errors on a healthy
  // setup), stop silently rather than showing a banner that would
  // be wrong.
  if (!versionKnown) {
    if (errorMsg.includes("decode FS_OP_STATUS_ACK body")) {
      return { kind: "stop-old-payload", threshold: "2.2.16" };
    }
    if (errorMsg.includes("unsupported_frame")) {
      return { kind: "stop-old-payload", threshold: "2.2.7" };
    }
  }

  // Current-or-unknown payload, repeated failures with no smoking
  // gun. Stop polling; the move continues independently. The user
  // sees "Copying… 23s" without a percentage, which is honest:
  // progress is unknown, not "your payload is broken."
  return { kind: "stop-silent" };
}

/** Whether a poll error string represents "op not currently
 *  registered" — distinct from a real failure. The engine maps the
 *  payload's `{found:false}` ACK to HTTP 404 ("op_id not in flight"),
 *  which surfaces here as part of the error message. We deliberately
 *  do *not* count these toward the consecutive-failure counter:
 *
 *    - For a few hundred ms after FS_COPY starts (between the engine
 *      forwarding the frame and the payload calling fs_op_register),
 *      the op genuinely isn't registered. Counting that as a failure
 *      would have us give up before progress could ever appear.
 *
 *    - When the op finishes, the payload releases the slot and the
 *      next poll returns 404. That's the natural termination signal,
 *      not a failure. The caller's `pollerStopped` flag will fire
 *      from the FS_COPY handler returning anyway. */
export function isExpectedNotInFlight(errorMsg: string): boolean {
  return errorMsg.includes("404") || errorMsg.includes("not in flight");
}
