// A stream install the PS5 could not start, and what a running one is waiting for, in the
// user's language (#942).
//
// Stream & install has the PS5 fetch the package from this computer. When it cannot connect
// (a firewall is the usual cause) the queue row sat on "0%" for a quarter of an hour with no
// word of why. The engine now finds out in seconds (its reach check) or gives up after a minute
// without a request (its watchdog), and sends the facts; this turns them into the sentence the
// row shows. The engine's own English hint stays in the log.

import { trStatic } from "./trStatic";

/** The engine's `ReachFail` (install/reach.rs). */
export interface ReachFail {
  /** `check`: found before anything was sent to the PS5. `no_fetch`: the PS5 was handed the
   *  package and never asked for it. */
  stage: "check" | "no_fetch";
  /** `http://host:port` the PS5 was (or would have been) sent to. */
  origin: string;
  cause: "timed_out" | "refused" | "other" | "unknown";
  /** Seconds waited for a request (`no_fetch`). */
  waited_s: number;
  /** The install's source kind; only `host_file` can be uploaded instead. */
  source: string;
  /** What the engine knows beyond the usual cause (Windows' account of the network, a container). */
  detail?: string | null;
}

/** What a running install waits for before the PS5 has moved a byte. */
export type InstallWaiting = "previous_install" | "reach_check" | "console_fetch";

/** How long the PS5's installer can hold on to a package it cannot fetch, at most: the next
 *  install waits for it. */
const PREVIOUS_INSTALL_MAX_MIN = 16;

const addressOf = (origin: string) => origin.replace(/^[a-z]+:\/\//i, "");

/** The failed row's sentence: what happened, the usual cause and its fix (or what the engine
 *  found instead), and the other way to install, where there is one. */
export function reachFailText(r: ReachFail): string {
  const address = addressOf(r.origin);
  const what =
    r.stage === "no_fetch"
      ? trStatic(
          "pkg.reach_no_fetch",
          "The PS5 was given the package and hadn't fetched any of it from this computer ({address}) after {seconds} seconds, so the install was stopped.",
          { address, seconds: r.waited_s },
        )
      : trStatic(
          "pkg.reach_blocked",
          "The PS5 can't connect to this computer ({address}), so it can't fetch the package.",
          { address },
        );
  const why =
    r.detail?.trim() ||
    trStatic(
      "pkg.reach_usual_cause",
      "Most often this computer's firewall is blocking it: allow PS5Upload through the firewall (on Windows, for both Private and Public networks), then press Retry.",
    );
  const parts = [what, why];
  if (r.source === "host_file") {
    parts.push(
      trStatic(
        "pkg.reach_use_upload",
        "Or press Retry via upload, which copies the package to the PS5 first and needs no connection back.",
      ),
    );
  }
  return parts.join(" ");
}

/** The running row's line while nothing is moving yet, or null. */
export function waitingNote(waiting: InstallWaiting | null | undefined): string | null {
  switch (waiting) {
    case "reach_check":
      return trStatic("pkg.wait_reach_check", "Checking that the PS5 can reach this computer…");
    case "console_fetch":
      return trStatic(
        "pkg.wait_console_fetch",
        "Waiting for the PS5 to start fetching the package from this computer…",
      );
    case "previous_install":
      return trStatic(
        "pkg.wait_previous_install",
        "The PS5's installer is still holding on to the last package it couldn't fetch. This install starts by itself when it lets go, in {minutes} min at most.",
        { minutes: PREVIOUS_INSTALL_MAX_MIN },
      );
    default:
      return null;
  }
}
