import type { ProfileInfo, ProfileSlot } from "../../api/ps5";

/** The Payloads catalogue entry for earthonion's np-fake-signin. */
export const NP_FAKE_SIGNIN_ID = "np-fake-signin";

/** Whether np-fake-signin can run for the signed-in user, and why not.
 *
 *  np-fake-signin finds the account slot whose name equals the foreground user's name and
 *  aborts when that slot's account id is 0 ("Not Activated!"). So the two facts it needs are
 *  the ones the Profile screen already reads. */
export type NpSignInReadiness =
  | { kind: "loading" }
  /** No user is signed in on the PS5, so there is nobody to sign in. */
  | { kind: "no_user" }
  /** No account slot carries the signed-in user's name. */
  | { kind: "no_slot"; username: string }
  /** The slot exists but has no account id: offline activation first. */
  | { kind: "not_activated"; username: string; slot: number }
  | { kind: "ready"; username: string; slot: number };

function idIsZero(id: string): boolean {
  return !/[1-9a-f]/i.test(id.replace(/^0x/i, ""));
}

export function npSignInReadiness(info: ProfileInfo | null): NpSignInReadiness {
  if (!info) return { kind: "loading" };
  const username = info.username.trim();
  if (info.uid === 0 || !username) return { kind: "no_user" };
  const slot: ProfileSlot | undefined = info.slots.find(
    (s) => s.name.trim() === username,
  );
  if (!slot) return { kind: "no_slot", username };
  if (!slot.activated || idIsZero(slot.id)) {
    return { kind: "not_activated", username, slot: slot.slot };
  }
  return { kind: "ready", username, slot: slot.slot };
}
