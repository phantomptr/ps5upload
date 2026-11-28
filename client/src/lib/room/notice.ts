// Play Rooms responsibility notice (design 7B): accepted once per device before the first chat,
// voice or public room, and again only when its version changes. "Not now" keeps those features
// off; the room itself works. Private rooms with chat and voice off never ask.

import { useSyncExternalStore } from "react";
import { safeGetItem, safeSetItem } from "../safeStorage";

/** Raise when the notice's words change in substance: everyone is asked again. */
export const NOTICE_VERSION = 1;
const KEY = "ps5upload.rooms.notice";

/** What a person is about to do that needs the notice first. */
export type NoticeFor = "chat" | "voice" | "public" | "rooms";

const listeners = new Set<() => void>();

export function noticeAccepted(): boolean {
  return Number(safeGetItem(KEY) ?? "0") >= NOTICE_VERSION;
}

export function acceptNotice(): void {
  safeSetItem(KEY, String(NOTICE_VERSION));
  for (const l of listeners) l();
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  const onStorage = (e: StorageEvent) => {
    if (e.key === KEY) cb();
  };
  if (typeof window !== "undefined") window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(cb);
    if (typeof window !== "undefined") window.removeEventListener("storage", onStorage);
  };
}

/** The notice was accepted on this device (live). */
export function useNoticeAccepted(): boolean {
  return useSyncExternalStore(subscribe, noticeAccepted, () => false);
}

/** Whether `what` must wait for the notice. Everything in the list needs it until accepted. */
export function needsNotice(_what: NoticeFor, accepted: boolean = noticeAccepted()): boolean {
  return !accepted;
}

/** Runs `go` now when the notice was accepted, else asks first (`ask` shows the notice and says
 *  whether it was accepted). True when `go` ran. */
export async function withNotice(what: NoticeFor, ask: (what: NoticeFor) => Promise<boolean>, go: () => void | Promise<void>): Promise<boolean> {
  if (!needsNotice(what)) {
    await go();
    return true;
  }
  if (!(await ask(what))) return false;
  acceptNotice();
  await go();
  return true;
}
