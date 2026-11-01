// Moving a PS5 save to another account on the same console (#612): who it can go to.
//
// The engine only replaces the files in a save the console already made, so a user with no save
// of the game is listed but can't be picked: they start the game once and save first.
import type { ProfileUser, SaveEntry } from "../../api/ps5";
import { userHex, userName } from "./saveUsers";

export interface CopyTarget {
  /** The user's id, as the saves list gives it. */
  uid: number;
  /** Their name, or null when the console didn't name them. */
  name: string | null;
  /** Their PS5 save of the game, or null: start the game once as them and save. */
  save: SaveEntry | null;
}

/** The console's other local users (and any other owner of a save, should the profile list
 *  miss one), each with their PS5 save of `titleId` if they have one; those with one first. */
export function copyTargets(
  users: readonly ProfileUser[],
  saves: readonly SaveEntry[],
  titleId: string,
  fromUser: number,
): CopyTarget[] {
  const from = userHex(fromUser);
  const title = titleId.toUpperCase();
  const seen = new Map<string, CopyTarget>();
  const add = (uid: number) => {
    const hex = userHex(uid);
    if (hex === from || hex === "00000000" || seen.has(hex)) return;
    const save =
      saves.find((s) => s.kind === "ps5" && userHex(s.user_id) === hex && s.title_id.toUpperCase() === title) ??
      null;
    seen.set(hex, { uid, name: userName(uid, users), save });
  };
  for (const u of users) add(u.uid);
  for (const s of saves) if (s.kind === "ps5") add(s.user_id);
  const all = [...seen.values()];
  return [...all.filter((t) => t.save), ...all.filter((t) => !t.save)];
}

/** Copy to another user is offered on a PS5 save when the console has another user. */
export function canCopyToOtherUser(
  save: SaveEntry,
  users: readonly ProfileUser[],
  saves: readonly SaveEntry[],
): boolean {
  return save.kind === "ps5" && copyTargets(users, saves, save.title_id, save.user_id).length > 0;
}

/** The console's local users, counted from the profile list, else from the saves' owners. */
export function localUserCount(users: readonly ProfileUser[], saves: readonly SaveEntry[]): number {
  const ids = new Set(users.map((u) => userHex(u.uid)));
  for (const s of saves) if (s.kind === "ps5" || s.kind === "ps4") ids.add(userHex(s.user_id));
  ids.delete("00000000");
  return ids.size;
}
