// Per-game controls (lib/gameControls.ts), kept per title on this device, and the title they
// are for right now (the game running on the streamed console).

import { create } from "zustand";

import { DEFAULT_CONTROLS, newControlsState, readControls, type GameControls } from "../lib/gameControls";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

const KEY = "ps5upload.rp_controls.v1";

function load(): Record<string, GameControls> {
  try {
    const raw = JSON.parse(safeGetItem(KEY) ?? "{}") as Record<string, unknown>;
    const out: Record<string, GameControls> = {};
    for (const [t, v] of Object.entries(raw ?? {})) if (/^[A-Z]{4}\d{5}$/.test(t)) out[t] = readControls(v);
    return out;
  } catch {
    return {};
  }
}

interface State {
  byTitle: Record<string, GameControls>;
  /** The game running on the streamed console, or null. */
  title: string | null;
  setTitle: (t: string | null) => void;
  update: (title: string, patch: Partial<GameControls>) => void;
  reset: (title: string) => void;
}

export const useGameControlsStore = create<State>((set, get) => ({
  byTitle: load(),
  title: null,
  setTitle: (t) => {
    if (get().title !== t) set({ title: t });
  },
  update: (title, patch) => {
    const byTitle = { ...get().byTitle, [title]: { ...(get().byTitle[title] ?? DEFAULT_CONTROLS), ...patch } };
    safeSetItem(KEY, JSON.stringify(byTitle));
    set({ byTitle });
  },
  reset: (title) => {
    const byTitle = { ...get().byTitle };
    delete byTitle[title];
    safeSetItem(KEY, JSON.stringify(byTitle));
    set({ byTitle });
  },
}));

/** The running game's controls, or null (none set, or no game known). */
export function currentControls(): GameControls | null {
  const s = useGameControlsStore.getState();
  return s.title ? (s.byTitle[s.title] ?? null) : null;
}

/** The mouse and a playing macro, between pad reads. */
export const rpControls = newControlsState();
