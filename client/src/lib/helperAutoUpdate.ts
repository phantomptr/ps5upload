/**
 * When this app may replace an OLDER helper on a console with its own, by itself, as a pure
 * function (the runtime is state/helperAutoUpdate.ts).
 *
 * People kept having to press Send helper after updating the app. An older helper is now
 * replaced automatically, but only when nothing can break: replacing the helper restarts it, and
 * its start-up sweep stops whatever the previous one was running (an archive extraction, an
 * install it drives, a mounted save, cheats in a running game, a Mesh Drive pass), every device's
 * transfer drops, and a helper that is merely slow must never be replaced (two helpers fight over
 * the takeover: memory/fw1270-pldmgr-disconnect-report). So the answer is one of
 *
 *   - `update` — send now (through lib/ensurePayloadCurrent: single-flight per console, never a
 *     downgrade, Mesh told first);
 *   - `check`  — everything this app knows says yes: ask the console itself (`ConsoleCheck`), then
 *     decide again;
 *   - `wait`   — not now, for `why`; asked again when things change;
 *   - `never`  — nothing to do, or not this app's to do (`why`).
 *
 * An older helper that cannot say whether the PS5 is busy (no task list before 11.1.0, a save
 * query it answers with an error) is waited on, never replaced on a guess. One that has no cheat
 * engine status is not held for it: cheats come back with the new helper, nothing is lost.
 */

import type { HostPower } from "./consolePower";
import { compareVersions } from "./semver";

/** The helper must have been answering this long: a helper that just started (a boot copy, a
 *  send moments ago) is still settling, and the black-box read and elfldr check run then. */
export const AUTO_UPDATE_SETTLE_MS = 20_000;
/** No automatic update this soon after any send to the console (the Send button, the reconnect
 *  loop, a queue's recovery): that helper is still starting, or just failed to. */
export const AUTO_UPDATE_AFTER_SEND_MS = 2 * 60_000;
/** After an automatic update that did not bring this app's helper up, the next try waits this long. */
export const AUTO_UPDATE_RETRY_MS = 10 * 60_000;
/** Automatic tries per version pair while the app runs; then it says why and leaves it to Send. */
export const MAX_AUTO_UPDATE_ATTEMPTS = 3;
/** Another device told the console it is restarting the helper this recently: let it. */
export const OTHER_RESTART_WINDOW_MS = 3 * 60_000;

/** One thing the console was asked about. `unknown`: it could not say (an error, an older helper). */
export type Known = "busy" | "idle" | "unknown";

/** What the console says right before an automatic update. */
export interface ConsoleCheck {
  /** A fresh status call answered (never decided on a failed or slow probe). */
  reachable: boolean;
  /** The helper version that answer gave. */
  version: string | null;
  /** Other devices' work on this console (Mesh presence). `idle` for a helper or engine without
   *  Mesh: there is nobody to ask, and nobody who could have told. */
  othersBusy: Known;
  /** Console locks held (a Mesh Drive pass, another device's job). */
  locks: Known;
  /** Another device announced a helper restart in the last few minutes. */
  otherRestart: boolean;
  /** Tasks the console runs for its own page (extract, install, copy). `unknown` also for a
   *  helper before 11.1.0, which has no such list. */
  consoleTasks: Known;
  /** A save is mounted. */
  saveOpen: Known;
  /** The cheat engine is on with a game running. */
  cheats: Known;
  /** The payload loader (:9021) or Payload Manager answers: something can take the helper. */
  loaderOpen: boolean;
  /** What the helper itself says a restart would stop (STATUS_ACK `busy`, 12.6.15). `unknown` for a
   *  helper before that, which is exactly the one being updated: the other answers decide then. */
  consoleBusy?: Known;
}

export interface AutoUpdateInput {
  /** Settings › This PS5 › Update the PS5 helper automatically (default on). */
  enabled: boolean;
  /** The user stopped this console's helper (state/helperStopped). */
  stopped: boolean;
  appVersion: string | null;
  /** The version the console's helper reported (the status poller's). */
  helperVersion: string | null;
  status: "up" | "down" | "unknown" | undefined;
  power?: HostPower;
  /** The helper answered but wants this device paired first. */
  pairing: boolean;
  /** How long the helper has been answering, as the poller saw it (null: not up). */
  upForMs: number | null;
  /** What this app is running on that console (a transfer, an install, a queue, Files, Remote
   *  Play, Play Together...), or null when nothing. */
  busyHere: string | null;
  /** A send to this console is running right now (lib/helperSendGate). */
  sendInFlight: boolean;
  /** How long ago a helper was last sent to this console (null: never in this run). */
  sentAgoMs: number | null;
  /** Automatic tries for this version pair so far, and how long ago the last ended. */
  attempts: number;
  lastAttemptAgoMs: number | null;
  /** The console's own answers; null until asked (`check`). */
  console: ConsoleCheck | null;
}

export type WaitReason =
  | "asleep"
  | "not-up"
  | "pairing"
  | "settling"
  | "sending"
  | "recent-send"
  | "retry-later"
  | "busy-here"
  | "not-answering"
  | "busy-others"
  | "another-device"
  | "console-work"
  | "console-unknown"
  | "loader-closed";

export type NeverReason = "off" | "stopped" | "current" | "newer" | "unknown-version" | "gave-up";

export type AutoUpdateDecision =
  | { kind: "update" }
  | { kind: "check" }
  | { kind: "wait"; why: WaitReason }
  | { kind: "never"; why: NeverReason };

/** How the helper's version stands against this app's: only `older` is ours to update. */
export function helperAge(helper: string | null, app: string | null): "older" | "current" | "newer" | "unknown" {
  if (!helper || !app) return "unknown";
  const c = compareVersions(helper, app);
  if (c === null) return "unknown";
  return c < 0 ? "older" : c > 0 ? "newer" : "current";
}

const NEVER_FOR_AGE = { current: "current", newer: "newer", unknown: "unknown-version" } as const;

/** Whether automatic helper updates exist in this build at all. Off since 12.6.18: two consoles
 *  crashed when a helper was replaced (#903) and the cause is not known yet. Sending the helper by
 *  hand is unchanged. Tests switch it on to exercise the rules. */
export const helperAutoUpdateGate = { available: false };

export function helperAutoUpdateDecision(i: AutoUpdateInput): AutoUpdateDecision {
  const wait = (why: WaitReason): AutoUpdateDecision => ({ kind: "wait", why });
  const never = (why: NeverReason): AutoUpdateDecision => ({ kind: "never", why });

  if (!i.enabled) return never("off");
  // A helper the user turned off stays off; sending it is theirs to do.
  if (i.stopped) return never("stopped");
  // Only an older helper. A newer one is another device's (the fix is updating this app), and a
  // version that does not read as one is not compared on a guess.
  const age = helperAge(i.helperVersion, i.appVersion);
  if (age !== "older") return never(NEVER_FOR_AGE[age]);
  if (i.attempts >= MAX_AUTO_UPDATE_ATTEMPTS) return never("gave-up");

  if (i.power === "rest") return wait("asleep");
  if (i.status !== "up") return wait("not-up");
  if (i.pairing) return wait("pairing");
  if (i.upForMs === null || i.upForMs < AUTO_UPDATE_SETTLE_MS) return wait("settling");
  // One send per console at a time, and none on top of one that is still starting.
  if (i.sendInFlight) return wait("sending");
  if (i.sentAgoMs !== null && i.sentAgoMs < AUTO_UPDATE_AFTER_SEND_MS) return wait("recent-send");
  if (i.lastAttemptAgoMs !== null && i.lastAttemptAgoMs < AUTO_UPDATE_RETRY_MS) return wait("retry-later");
  if (i.busyHere) return wait("busy-here");

  const c = i.console;
  if (!c) return { kind: "check" };
  // Only on a fresh answer: a failed or slow probe says nothing about whether it is safe.
  if (!c.reachable) return wait("not-answering");
  const fresh = helperAge(c.version, i.appVersion);
  if (fresh !== "older") return never(NEVER_FOR_AGE[fresh]);
  if (c.othersBusy !== "idle" || c.locks !== "idle") return wait("busy-others");
  if (c.otherRestart) return wait("another-device");
  if (c.consoleTasks === "busy" || c.saveOpen === "busy" || c.cheats === "busy" || c.consoleBusy === "busy")
    return wait("console-work");
  // Not knowing is not knowing it is idle, for what a restart would break: the console's own
  // tasks and an open save. The cheat engine is not held for (see the top).
  if (c.consoleTasks === "unknown" || c.saveOpen === "unknown") return wait("console-unknown");
  // Nothing can take the helper: the loader's own guidance applies (quietly).
  if (!c.loaderOpen) return wait("loader-closed");
  return { kind: "update" };
}
