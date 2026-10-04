// The console's AVA1 session state: the ONE thing the status pill shows.
//
// It replaces the old pair of probes (a management STATUS plus a TCP connect to the
// transfer port, whose disagreement was the "uploads fail but the dot is green" wedge).
// There is one listener now, so there is one verdict:
//
//   connected      a paired session answered node.status
//   needs_pairing  the console answered but has not accepted this app yet (Pair…)
//   helper_old     the console runs an older helper than this app speaks to (Update it)
//   down           nothing usable answered (the Send helper flow)
//
// The engine reports the reason as a token in the probe's error text; this maps the tokens.
// Tokens come from the engine (Task 8 owns `helper_old` / `legacy_helper_wedged`,
// `helper_not_ava1`; the pairing ones are `ava1_not_paired` / `not_paired`).

import { hostOf } from "./addr";

export type SessionState = "connected" | "needs_pairing" | "helper_old" | "down";

const NOT_PAIRED = ["ava1_not_paired", "not_paired", "devices are not paired"];
const HELPER_OLD = ["helper_old", "legacy_helper_wedged"];

function text(e: unknown): string {
  if (e instanceof Error) return e.message.toLowerCase();
  return typeof e === "string" ? e.toLowerCase() : "";
}

/** True when `e` (an Error, an engine error string, or an `error_reason`) says the console
 *  has not accepted this app: the pairing dialog's trigger. */
export function isNotPairedError(e: unknown): boolean {
  const t = text(e);
  return t !== "" && NOT_PAIRED.some((tok) => t.includes(tok));
}

/** True when the failure is the console's old helper (token only; the engine decides). */
export function isHelperOldError(e: unknown): boolean {
  const t = text(e);
  return t !== "" && HELPER_OLD.some((tok) => t.includes(tok));
}

/** True when it is specifically the old helper that would not exit (the console must be
 *  restarted before the update can run). */
export function isLegacyHelperWedged(e: unknown): boolean {
  return text(e).includes("legacy_helper_wedged");
}

export type ReplaceFailure =
  | "wedged"
  | "no_helper"
  | "in_progress"
  | "cooldown"
  | "starting"
  | "ava1_failed"
  | "failed";

/** What a failed `POST /api/ps5/helper/replace` means, from the token the engine puts at the
 *  start of its error. Each one needs a different reaction: only `no_helper` falls back to
 *  sending the helper; `in_progress` / `cooldown` must NOT (a send would race the replace). */
export function classifyReplaceError(e: unknown): ReplaceFailure {
  const t = text(e);
  if (t.includes("legacy_helper_wedged")) return "wedged";
  if (t.includes("helper_not_running")) return "no_helper";
  if (t.includes("replace_in_progress")) return "in_progress";
  if (t.includes("replace_cooldown")) return "cooldown";
  if (t.includes("helper_starting")) return "starting";
  if (t.includes("ava1_failed")) return "ava1_failed";
  return "failed";
}

/** The probe verdict from one payload_check reply. */
export function classifySession(r: {
  reachable: boolean;
  error: string | null;
}): SessionState {
  if (r.reachable) return "connected";
  if (isNotPairedError(r.error)) return "needs_pairing";
  if (isHelperOldError(r.error)) return "helper_old";
  return "down";
}

/** States where a person has to do something (the pill gets an action). */
export function sessionNeedsAttention(s: SessionState | null | undefined): boolean {
  return s === "needs_pairing" || s === "helper_old";
}

/** The console a command targets, from its arguments: `ip`, `addr`, or `req.addr`, port
 *  stripped. Never `from` / `to` (usually PATHS in file commands). Undefined when the command
 *  names none: a failure then says nothing about WHICH console. */
export function hostFromArgs(args: unknown): string | undefined {
  const look = (o: unknown): string | undefined => {
    if (!o || typeof o !== "object") return undefined;
    const r = o as Record<string, unknown>;
    for (const k of ["ip", "addr"]) {
      const v = r[k];
      if (typeof v === "string" && v.trim()) return hostOf(v.trim()) || undefined;
    }
    return undefined;
  };
  const a = args as Record<string, unknown> | undefined;
  return look(a) ?? look(a?.req);
}

// A tiny signal so the shared invoke wrapper can say "a call came back not_paired" without
// importing the pairing store (which imports the API layer, which imports the wrapper).
type Listener = (host?: string) => void;
const listeners = new Set<Listener>();

/** The pairing store subscribes here. Returns the unsubscribe. */
export function onNotPaired(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Call with any failure: if it is a not-paired one, listeners (the pairing dialog) hear it. */
export function reportIfNotPaired(e: unknown, host?: string): boolean {
  if (!isNotPairedError(e)) return false;
  for (const fn of listeners) {
    try {
      fn(host);
    } catch {
      /* a listener must not break the failing call's own error path */
    }
  }
  return true;
}
