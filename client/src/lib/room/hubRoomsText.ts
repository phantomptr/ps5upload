// What a Rooms list miss means, in words, apart from hubRooms.ts so the guest page (which uses the
// friends lookup) doesn't carry these strings.

import type { HubMiss } from "./hubRooms";

type Tr = (key: string, vars: undefined, fallback: string) => string;

/** A miss in words, with what to do (design 8.1). */
export function hubMissText(tr: Tr, m: HubMiss): string {
  switch (m) {
    case "old_hub":
      return tr("rooms_old_hub", undefined, "Your rooms service has no public rooms. Check Settings › Play Room.");
    case "off":
      return tr("rooms_off", undefined, "Public rooms are switched off on the rooms service right now.");
    case "needs_direct":
      return tr("rooms_needs_direct", undefined, "Details need a direct connection right now.");
    case "no_answer":
      return tr("rooms_no_answer", undefined, "The host didn't answer. Try again in a moment.");
    case "no_room":
      return tr("rooms_no_room", undefined, "This room is closed.");
    case "unreachable":
      return tr("rooms_unreachable", undefined, "Can't reach the rooms list. Check your internet.");
  }
}
