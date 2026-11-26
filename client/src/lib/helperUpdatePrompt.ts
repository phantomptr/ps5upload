/**
 * "The PS5 helper is out of date": when to say it, as a pure function (the runtime is
 * state/helperUpdatePrompt.ts).
 *
 * Until 12.6.15 a bar across the app said the helper and the app were on different versions,
 * with a Send helper button. The automatic update replaced it (lib/helperAutoUpdate), and when
 * that was switched off after #903 nothing was left: an older helper showed only on the
 * console's card under Manage connection (#941). This is the notice that came back. It never
 * sends anything by itself: it offers one button, and the send behind it is the one Resend
 * helper makes.
 *
 * Said once per console per app version: a toast and an entry in the bell, plus a notice on
 * Home that stays until the helper is updated or the notice is dismissed. The next app version
 * says it again. Never for a console that is asleep, unreachable, waiting to pair, or whose
 * helper the user stopped; never for a newer helper (the fix there is updating this app).
 */

import type { HostPower } from "./consolePower";
import { helperAge } from "./helperAutoUpdate";

export interface HelperPromptInput {
  appVersion: string | null;
  /** The version the console's helper reported (the status poller's). */
  helperVersion: string | null;
  status: "up" | "down" | "unknown" | undefined;
  power?: HostPower;
  /** The helper answered but wants this device paired first. */
  pairing: boolean;
  /** The user stopped this console's helper (state/helperStopped). */
  stopped: boolean;
  /** A helper send to this console is running (the old one may still be answering). */
  sending: boolean;
  /** The automatic update looks after this console (lib/helperAutoUpdate): it says its own. */
  autoUpdates: boolean;
  /** What was already said for this console (`HelperPromptRecord`). */
  record: HelperPromptRecord | undefined;
}

/** What one console was told, by app version. */
export interface HelperPromptRecord {
  /** The app version the toast and the bell entry went out for. */
  notified?: string;
  /** The app version the notice on Home was dismissed for. */
  dismissed?: string;
  /** The bell entry's id, so it goes when the helper is updated. */
  notificationId?: string;
}

export interface HelperPromptDecision {
  /** The helper answers and is older than this app: the card and Home mark it. */
  outdated: boolean;
  /** The notice on Home shows (not dismissed for this app version). */
  show: boolean;
  /** The toast and the bell entry go out now (not yet for this app version). */
  notify: boolean;
}

const NOTHING: HelperPromptDecision = { outdated: false, show: false, notify: false };

export function helperPromptDecision(i: HelperPromptInput): HelperPromptDecision {
  // Only a console that answers right now: nothing can be sent to one that is asleep or away,
  // and a notice about it would only nag.
  if (i.status !== "up" || i.power === "rest" || i.power === "unreachable") return NOTHING;
  if (i.pairing || i.stopped) return NOTHING;
  if (!i.appVersion || helperAge(i.helperVersion, i.appVersion) !== "older") return NOTHING;
  if (i.autoUpdates) return { outdated: true, show: false, notify: false };
  const show = i.record?.dismissed !== i.appVersion;
  // Not while a send runs: the old helper answers until the new one takes over.
  const notify = show && !i.sending && i.record?.notified !== i.appVersion;
  return { outdated: true, show, notify };
}

/** Why the Update helper button does not send right now: this app is in the middle of something
 *  on that console, and a new helper would stop it. */
export type UpdateHold = "transfer" | "launch" | "stream" | "busy";

export interface UpdateHoldInput {
  /** An upload, download, copy or the queue runs on this console. */
  transfer: boolean;
  /** An install, extraction or other task of this app runs there. */
  task: boolean;
  /** A game is being started there. */
  launch: boolean;
  /** Remote Play (or Play Together) shows that console. */
  stream: boolean;
  /** A bring-up or another helper send runs there. */
  setup: boolean;
}

export function updateHold(i: UpdateHoldInput): UpdateHold | null {
  if (i.transfer || i.task) return "transfer";
  if (i.launch) return "launch";
  if (i.stream) return "stream";
  if (i.setup) return "busy";
  return null;
}
