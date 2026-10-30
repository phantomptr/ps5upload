// What the Play setup checklist can't read from the console (screens/Play/setup): the user's
// tick for the rest-mode settings, and whether a picture ever came through, per console; and
// whether Home shows its Play card. Kept on this device.

import { create } from "zustand";

import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useRemotePlayStore } from "./remotePlay";

const KEY = "ps5upload.play_setup.v1";

interface Marks {
  rest: Record<string, true>;
  played: Record<string, true>;
  /** Home's Play card is hidden (Play › Set up shows it again). */
  homeHidden: boolean;
}

function load(): Marks {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "{}") as Partial<Marks>;
    const ticks = (o: unknown): Record<string, true> =>
      Object.fromEntries(
        Object.entries(o && typeof o === "object" ? o : {}).filter(([k, x]) => k && x === true),
      ) as Record<string, true>;
    return { rest: ticks(v.rest), played: ticks(v.played), homeHidden: v.homeHidden === true };
  } catch {
    return { rest: {}, played: {}, homeHidden: false };
  }
}

interface State extends Marks {
  setRest: (host: string, on: boolean) => void;
  setPlayed: (host: string) => void;
  setHomeHidden: (hidden: boolean) => void;
}

export const usePlaySetupMarks = create<State>((set, get) => {
  const save = (m: Marks) =>
    safeSetItem(KEY, JSON.stringify({ rest: m.rest, played: m.played, homeHidden: m.homeHidden }));
  return {
    ...load(),
    setRest(host, on) {
      const h = host.trim();
      if (!h) return;
      const rest = { ...get().rest };
      if (on) rest[h] = true;
      else delete rest[h];
      set({ rest });
      save({ ...get(), rest });
    },
    setPlayed(host) {
      const h = host.trim();
      if (!h || get().played[h]) return;
      const played = { ...get().played, [h]: true as const };
      set({ played });
      save({ ...get(), played });
    },
    setHomeHidden(homeHidden) {
      set({ homeHidden });
      save({ ...get(), homeHidden });
    },
  };
});

// The first picture from a console ticks its last step.
useRemotePlayStore.subscribe((s, prev) => {
  if (s.phase === "playing" && prev.phase !== "playing" && s.target?.host) {
    usePlaySetupMarks.getState().setPlayed(s.target.host);
  }
});
