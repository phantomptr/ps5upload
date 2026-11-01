import { powerWake, wakeAndSignIn } from "../api/ps5";
import { useRosterStore, withConsolePrefix } from "../state/roster";
import { audit } from "../state/auditLog";
import { log } from "../state/logs";
import { hostOf } from "./addr";
import { wakeCredentialsFor } from "./useConsoleWake";

/** How long after one wake signal the queue waits before sending another to the same console.
 *  A wake from rest takes ~20-40 s and the helper follows a few seconds after the loader, so a
 *  console that is still asleep after this did not take the first one. */
export const WAKE_FOR_UPLOAD_RETRY_MS = 3 * 60_000;

/** Whether an upload parked on a sleeping console should send it a wake signal now. */
export function shouldWakeForUpload(input: {
  enabled: boolean;
  /** The console has a saved wake credential (or Remote Play session keys). */
  canWake: boolean;
  /** When this console was last sent a wake for an upload, or undefined. */
  lastSentAt: number | undefined;
  now: number;
}): boolean {
  if (!input.enabled || !input.canWake) return false;
  return input.lastSentAt === undefined || input.now - input.lastSentAt >= WAKE_FOR_UPLOAD_RETRY_MS;
}

const lastSent = new Map<string, number>();

/**
 * Wake a sleeping console because an upload is waiting for it. Fire-and-forget: the queue's
 * own wake path (the helper is re-sent by auto-redeploy, the parked rows resume on the
 * helper-up edge) carries the upload from there. Returns whether a wake was sent.
 *
 * Nobody is asked for anything: when the PS5 user has a login passcode the sign-in stops there
 * and the wake counts as done. The console is up at its sign-in screen, where the helper and
 * transfers work (they need no signed-in user).
 */
export function wakeForUpload(addr: string, enabled: boolean, now = Date.now()): boolean {
  const h = hostOf(addr) || addr;
  const creds = wakeCredentialsFor(useRosterStore.getState().profiles, h);
  if (!shouldWakeForUpload({ enabled, canWake: creds.canWake, lastSentAt: lastSent.get(h), now }))
    return false;
  lastSent.set(h, now);
  const send = creds.canSignIn
    ? wakeAndSignIn(h, creds.credential, creds.registKey, creds.rpKey)
    : powerWake(h, creds.credential);
  log.info("queue", `${h} is in rest mode — sending a wake for the waiting upload`);
  audit("system_wake", withConsolePrefix(h, "woken for a waiting upload"));
  send.then(
    (r) => {
      if (r && "state" in r && r.state === "passcode_needed")
        log.info("queue", `${h} is awake at its sign-in screen (the user has a passcode); the upload carries on`);
    },
    (e) => log.warn("queue", `${h}: wake for upload failed: ${String(e)}`),
  );
  return true;
}

/** Test seam: forget every wake sent. */
export function resetWakeForUpload(): void {
  lastSent.clear();
}
