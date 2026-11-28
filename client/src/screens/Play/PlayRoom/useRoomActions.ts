import { useCallback } from "react";

import { roomApi, type RoomBody, type RoomRefusal } from "../../../lib/room/room";
import { pushNotification } from "../../../state/notifications";
import { useTr } from "../../../state/lang";
import { joinApi, type JoinHostState } from "../playTogetherState";

type Tr = ReturnType<typeof useTr>;

/** A room change the engine refused, in words (design 3.1, 10.1). */
export function refusalText(tr: Tr, why: RoomRefusal, people = 0): string {
  switch (why) {
    case "size_below_players":
      return tr("room_err_size", { n: people }, `${people} people are in; remove someone first.`);
    case "public_needs_anywhere":
      return tr("room_err_public_anywhere", undefined, "Public rooms need Invites work: Anywhere (Settings › Play Room).");
    case "password_short":
      return tr("room_password_short", undefined, "Use 4 or more characters.");
    case "name":
      return tr("room_err_name", undefined, "Pick another name.");
    case "friends_playing":
      return tr("room_err_friends_playing", undefined, "Friends are playing. Try again when they're done.");
    case "not_open":
      return tr("room_err_not_open", undefined, "Your room isn't open.");
    case "old_engine":
      return tr("room_err_old_engine", undefined, "This needs the PS5Upload engine 13.0.0 or newer.");
    default:
      return tr("room_err_engine", undefined, "That didn't work. Try again.");
  }
}

/** The room's changes for the panel: each one, then the whole state read again. A refusal is told
 *  (never swallowed). */
export function useRoomActions(addr: string, onState: (s: JoinHostState) => void, people: number) {
  const tr = useTr();
  const after = useCallback(
    async (r: Awaited<ReturnType<typeof roomApi.update>>): Promise<boolean> => {
      if (!r.ok) {
        pushNotification("warning", refusalText(tr, r.error, people));
        return false;
      }
      const s = await joinApi.read(addr);
      if (s) onState(s);
      return true;
    },
    [addr, onState, people, tr],
  );
  return {
    update: (body: RoomBody) => roomApi.update(addr, body).then(after),
    lock: (lock: boolean) => roomApi.lock(addr, lock).then(after),
    kick: (player: number, block: "room" | "always" | null) => roomApi.kick(addr, player, block).then(after),
    unblock: (tag: string) => roomApi.unblock(addr, tag).then(after),
    mute: (m: { player?: number; voice?: boolean; chat?: boolean; all?: boolean }) => roomApi.mute(addr, m).then(after),
    seat: (player: number, to: number | "back") => roomApi.seat(addr, player, to).then(after),
    chat: (m: { quick: number } | { text: string }) => roomApi.chat(addr, m).then(after),
    ready: () => roomApi.ready(addr).then(after),
    allowGroup: (id: number) => roomApi.allowGroup(addr, id).then(after),
  };
}

export type RoomActions = ReturnType<typeof useRoomActions>;
