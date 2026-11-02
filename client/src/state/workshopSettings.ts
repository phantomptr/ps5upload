// "Use less of this computer" (#632): the engine's own setting, so it holds for every window and
// browser that uses this engine (the Docker web UI included) and across restarts. While on,
// Workshop jobs use about half the cores at background priority, which also puts their disk
// reads and writes behind everyone else's. It applies to a running job too.

import { create } from "zustand";

import { invoke } from "../lib/invokeLogged";

interface EngineWorkshopSettings {
  gentle: boolean;
  threads?: number;
  cores?: number;
  saved?: boolean;
  error?: string;
}

interface WorkshopSettingsState {
  gentle: boolean;
  /** The engine has answered once. */
  loaded: boolean;
  /** A change is on its way to the engine. */
  busy: boolean;
  /** Why the last change did not take (or was not kept), for the line under the switch. */
  error: string | null;
  load: () => Promise<void>;
  setGentle: (on: boolean) => Promise<void>;
}

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useWorkshopSettings = create<WorkshopSettingsState>((set, get) => ({
  gentle: false,
  loaded: false,
  busy: false,
  error: null,
  load: async () => {
    try {
      const r = await invoke<EngineWorkshopSettings>("workshop_settings_get");
      // A change sent meanwhile wins over this older answer.
      if (!get().busy) set({ gentle: r.gentle === true, loaded: true });
    } catch {
      // An engine without the setting (or not answering yet): the switch shows off.
    }
  },
  setGentle: async (on) => {
    const before = get().gentle;
    set({ gentle: on, busy: true, error: null });
    try {
      const r = await invoke<EngineWorkshopSettings>("workshop_settings_set", { gentle: on });
      set({
        gentle: r.gentle === true,
        loaded: true,
        busy: false,
        error: r.saved === false ? (r.error ?? null) : null,
      });
    } catch (e) {
      set({ gentle: before, busy: false, error: messageOf(e) });
    }
  },
}));
