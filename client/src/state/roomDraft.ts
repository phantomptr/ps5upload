// Play Rooms: what the host picked on "Create a Play Room" before the room exists. Sent once with
// the open (POST /api/join/open `room`), then the engine keeps the room's settings itself
// (play_together.json `room`). The password is never kept here past the open, and never saved.

import { create } from "zustand";

import type { RoomBody } from "../lib/room/room";

interface RoomDraftState {
  /** Only what the host changed (the engine fills the rest from its saved room). */
  draft: RoomBody;
  set: (patch: RoomBody) => void;
  /** The draft for the open, once; cleared after. */
  take: () => RoomBody | null;
  clear: () => void;
}

export const useRoomDraft = create<RoomDraftState>((set, get) => ({
  draft: {},
  set: (patch) => set((s) => ({ draft: { ...s.draft, ...patch } })),
  take: () => {
    const d = get().draft;
    set({ draft: {} });
    return Object.keys(d).length ? d : null;
  },
  clear: () => set({ draft: {} }),
}));

/** The first time a room goes Public on this device, one confirm (design 5.1); never again. */
const PUBLIC_OK = "ps5upload.rooms.public_ok";
export function publicConfirmed(): boolean {
  try {
    return localStorage.getItem(PUBLIC_OK) === "1";
  } catch {
    return false;
  }
}
export function confirmPublic(): void {
  try {
    localStorage.setItem(PUBLIC_OK, "1");
  } catch {
    // Asked again next time: harmless.
  }
}
