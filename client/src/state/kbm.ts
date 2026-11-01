// Native keyboard & mouse (lib/kbmInput.ts): the mode each game plays in, kept per title on this
// device (the console's home screen, no game, has its own), what the console can make, and what
// the engine says the PS5 sees now.

import { create } from "zustand";

import { getEngineUrl } from "./engine";
import { KBM_MODES, type KbmCaps, type KbmMode, type KbmStatus } from "../lib/kbmInput";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { DEFAULT_SHORTCUTS, readShortcut, type Shortcut, type ShortcutKind, type Shortcuts } from "../lib/kbmShortcuts";

const KEY = "ps5upload.rp_kbm.v1";
/** The stream's keyboard & mouse shortcuts (lib/kbmShortcuts.ts), on this device only. */
const SHORTCUTS_KEY = "ps5upload.rp_kbm_keys.v1";

function loadShortcuts(): Shortcuts {
  try {
    const raw = JSON.parse(safeGetItem(SHORTCUTS_KEY) ?? "{}") as Partial<Record<ShortcutKind, unknown>>;
    return {
      release: readShortcut(raw.release) ?? { ...DEFAULT_SHORTCUTS.release },
      cycle: readShortcut(raw.cycle) ?? { ...DEFAULT_SHORTCUTS.cycle },
    };
  } catch {
    return { release: { ...DEFAULT_SHORTCUTS.release }, cycle: { ...DEFAULT_SHORTCUTS.cycle } };
  }
}
/** The mode's key when no game is known (the console's home screen). */
export const HOME = "home";

/** What the console can make: from `GET /api/ps5/vinput/caps`. */
export interface KbmConsoleCaps extends KbmCaps {
  host: string;
  /** The helper has the methods at all. */
  supported: boolean;
  /** Why nothing can be made (`vinput_unsupported`, `vinput_needs_loader`,
   *  `vinput_companion_failed`), or why it could not be asked. */
  reason: string | null;
  /** The console's keyboard & mouse process: idle, starting, running, stopped or unavailable
   *  (null from an engine that does not say). */
  companion: string | null;
}

interface Saved {
  modes: Record<string, KbmMode>;
  /** The "If the game ignores the mouse" hint was shown once. */
  hinted: boolean;
  /** The first capture's "how to get your keyboard & mouse back" card was shown on this device. */
  captureHinted: boolean;
}

function load(): Saved {
  try {
    const raw = JSON.parse(safeGetItem(KEY) ?? "{}") as Partial<Saved>;
    const modes: Record<string, KbmMode> = {};
    for (const [t, m] of Object.entries(raw.modes ?? {}))
      if ((t === HOME || /^[A-Z]{4}\d{5}$/.test(t)) && KBM_MODES.includes(m as KbmMode)) modes[t] = m as KbmMode;
    return { modes, hinted: raw.hinted === true, captureHinted: raw.captureHinted === true };
  } catch {
    return { modes: {}, hinted: false, captureHinted: false };
  }
}

interface State extends Saved {
  caps: KbmConsoleCaps | null;
  status: KbmStatus | null;
  /** The one-time hint was given in this run (Settings shows it under the modes). */
  hintNow: boolean;
  setMode: (title: string | null, mode: KbmMode) => void;
  markHinted: () => void;
  markCaptureHinted: () => void;
  shortcuts: Shortcuts;
  /** Sets a shortcut (null: back to the default). */
  setShortcut: (kind: ShortcutKind, s: Shortcut | null) => void;
  setCaps: (c: KbmConsoleCaps | null) => void;
  setStatus: (s: KbmStatus | null) => void;
}

export const useKbmStore = create<State>((set, get) => {
  const save = () =>
    safeSetItem(KEY, JSON.stringify({ modes: get().modes, hinted: get().hinted, captureHinted: get().captureHinted }));
  return {
    ...load(),
    caps: null,
    status: null,
    hintNow: false,
    setMode: (title, mode) => {
      set({ modes: { ...get().modes, [title ?? HOME]: mode } });
      save();
    },
    markHinted: () => {
      set({ hinted: true, hintNow: true });
      save();
    },
    markCaptureHinted: () => {
      set({ captureHinted: true });
      save();
    },
    shortcuts: loadShortcuts(),
    setShortcut: (kind, sc) => {
      const shortcuts = { ...get().shortcuts, [kind]: sc ?? { ...DEFAULT_SHORTCUTS[kind] } };
      set({ shortcuts });
      safeSetItem(SHORTCUTS_KEY, JSON.stringify(shortcuts));
    },
    setCaps: (caps) => set({ caps }),
    setStatus: (status) => set({ status }),
  };
});

/** The mode remembered for `title` (Controller when none is). */
export function rememberedMode(modes: Record<string, KbmMode>, title: string | null): KbmMode {
  return modes[title ?? HOME] ?? "controller";
}

/** Asks the engine what `host` can make. Never throws: a failure is "nothing", with why. */
export async function fetchKbmCaps(host: string): Promise<KbmConsoleCaps> {
  const none = (reason: string, supported = false): KbmConsoleCaps => ({
    host,
    mouse: false,
    keyboard: false,
    supported,
    reason,
    companion: null,
  });
  try {
    const r = await fetch(`${getEngineUrl()}/api/ps5/vinput/caps?addr=${encodeURIComponent(host)}`);
    if (!r.ok) return none("unreachable");
    const v = (await r.json()) as Record<string, unknown>;
    if (v.supported !== true) return none("helper_old");
    return {
      host,
      supported: true,
      mouse: v.mouse === true,
      keyboard: v.keyboard === true,
      reason: typeof v.reason === "string" ? v.reason : null,
      companion: typeof v.companion === "string" ? v.companion : null,
    };
  } catch {
    return none("unreachable");
  }
}
