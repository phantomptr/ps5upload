import type { NpState, ProfileInfo, ProfileSlot } from "../../api/ps5";

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

/** The birth date np-fake-signin's template account carries. An account with it was signed in
 *  by np-fake-signin (its README's own example data), whatever the console says otherwise. */
export const NP_TEMPLATE_DOB = "1983-04-12";

/** The user and account slot the sign-in backup is about: the foreground user when the helper
 *  can read it, else the only local user (the helper often reads no foreground user on 13.60),
 *  matched to the slot carrying the same name, as np-fake-signin matches it. */
export function npBackupTarget(
  info: ProfileInfo | null,
): { uid: number; slot: number; username: string } | null {
  if (!info) return null;
  // An older helper (or a partial answer) may leave a name or a list out: no target, not a throw.
  let uid = info.uid ?? 0;
  let username = (info.username ?? "").trim();
  const users = info.users ?? [];
  if (uid === 0 && users.length === 1) {
    uid = users[0].uid;
    username = (users[0].username ?? "").trim();
  }
  if (uid === 0 || !username) return null;
  const slot = (info.slots ?? []).find((s) => (s.name ?? "").trim() === username);
  return slot ? { uid, slot: slot.slot, username } : null;
}

/** What Undo can do for this account: put a kept copy back, or nothing because the account was
 *  signed in by np-fake-signin before the app kept copies (its template birth date is there),
 *  or nothing because it shows no sign of np-fake-signin. */
export type NpUndo = "restore" | "no_backup" | "none";

export function npUndo(state: Pick<NpState, "backup" | "dob"> | null): NpUndo {
  if (!state) return "none";
  if (state.backup) return "restore";
  return state.dob === NP_TEMPLATE_DOB ? "no_backup" : "none";
}
