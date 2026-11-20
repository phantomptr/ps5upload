// When each console's loader (elfldr on :9021, or Payload Manager) was first seen open, one record
// for every path that sends a helper: the reconnect loop, a queue's start and its recovery, the
// automatic update and the Send helper buttons (12.6.15).
//
// A helper sent while the console is still starting crashes it: the jailbreak's autoloader brings
// the loader up first and only then loads kstuff and the rest, and a helper that lands in between
// crashed an FW 11.40 console three times out of three. Only the reconnect loop waited for the
// loader to settle; a queue's recovery sent at once. Now every unattended send waits, and a send
// someone asks for right after the console started says so first ("Send anyway").

import { consoleKey } from "./helperSendGate";
import { LOADER_SETTLE_MS } from "./autoRedeploy";

interface Seen {
  /** When the loader was first seen open since the console was last seen gone. */
  firstOpenAt?: number;
  /** The console was seen gone (off, rest mode, loader closed) this run: a later open is a start. */
  sawGone: boolean;
  /** This app's helper was seen answering, and the console has not gone since: it is not starting. */
  helperUpAt?: number;
}

const seen = new Map<string, Seen>();

function rec(host: string): Seen {
  const k = consoleKey(host);
  let r = seen.get(k);
  if (!r) {
    r = { sawGone: false };
    seen.set(k, r);
  }
  return r;
}

/** The loader was probed: open or not. */
export function noteLoader(host: string, open: boolean, now = Date.now()): void {
  const r = rec(host);
  if (open) {
    r.firstOpenAt ??= now;
  } else {
    r.firstOpenAt = undefined;
    r.helperUpAt = undefined;
    r.sawGone = true;
  }
}

/** The console is gone: rest mode, or no answer at all (off, another network, restarting). */
export function noteConsoleGone(host: string): void {
  const r = rec(host);
  r.firstOpenAt = undefined;
  r.helperUpAt = undefined;
  r.sawGone = true;
}

/** A helper answered on the console: it is running, not starting. */
export function noteHelperUp(host: string, now = Date.now()): void {
  rec(host).helperUpAt = now;
}

/** When the loader was first seen open (undefined: not seen open since it was last gone). */
export function loaderOpenSince(host: string): number | undefined {
  return seen.get(consoleKey(host))?.firstOpenAt;
}

/** Test seam. */
export function resetLoaderSettle(): void {
  seen.clear();
}

export type SettleState = "send" | "settle" | "closed";

/** Whether an unattended send may go now. Pure, for the tests: `r` is the console's record.
 *
 * - `closed`: nothing takes a helper (the loader and Payload Manager are closed).
 * - `send`: the loader has been open `settleMs`, or this app's helper answered there since the
 *   console was last gone (a crash of the helper, not a console starting).
 * - `settle`: wait. Knowing nothing about the console is waiting too: the loader is taken as
 *   just opened, as the reconnect loop always did. */
export function settleDecision(input: {
  loaderOpen: boolean;
  firstOpenAt: number | undefined;
  helperUpAt: number | undefined;
  now: number;
  settleMs?: number;
}): SettleState {
  if (!input.loaderOpen) return "closed";
  if (input.helperUpAt !== undefined) return "send";
  const since = input.firstOpenAt ?? input.now;
  return input.now - since >= (input.settleMs ?? LOADER_SETTLE_MS) ? "send" : "settle";
}

/** settleDecision for `host`, noting the loader's state first. `sendAt`: when it may go. */
export function unattendedSettle(
  host: string,
  loaderOpen: boolean,
  settleMs = LOADER_SETTLE_MS,
  now = Date.now(),
): { state: SettleState; sendAt: number } {
  noteLoader(host, loaderOpen, now);
  const r = rec(host);
  const state = settleDecision({
    loaderOpen,
    firstOpenAt: r.firstOpenAt,
    helperUpAt: r.helperUpAt,
    now,
    settleMs,
  });
  return { state, sendAt: (r.firstOpenAt ?? now) + settleMs };
}

/** For a send someone asked for: the console was seen starting (gone, then its loader opened) less
 *  than `settleMs` ago, so the helper would land while it still loads its jailbreak. Then the app
 *  says "the PS5 just started" and offers Send anyway; `sendAt` is when that wait ends. A console
 *  this app never saw go is not called starting: it may have been up for hours. */
export function justStarted(
  host: string,
  settleMs = LOADER_SETTLE_MS,
  now = Date.now(),
): { sendAt: number } | null {
  const r = seen.get(consoleKey(host));
  if (!r || !r.sawGone || r.firstOpenAt === undefined || r.helperUpAt !== undefined) return null;
  const sendAt = r.firstOpenAt + settleMs;
  return now < sendAt ? { sendAt } : null;
}
