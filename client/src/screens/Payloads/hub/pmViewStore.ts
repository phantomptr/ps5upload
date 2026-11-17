// The last Payload Manager overview read for each console, shared by "On your PS5" (which reads
// it once per refresh, so the saved list is known with the card collapsed) and the Payload
// Manager panel (which reads it on open and after each change). null: the last read failed.

import { create } from "zustand";

import type { PmOverview } from "../../../api/payloadManager";
import { hostOf } from "../../../lib/addr";

interface PmViewState {
  byHost: Record<string, PmOverview | null>;
  put: (host: string, view: PmOverview | null) => void;
}

export const pmViewKey = (host: string) => hostOf(host.trim()).toLowerCase();

export const usePmViewStore = create<PmViewState>((set) => ({
  byHost: {},
  put: (host, view) =>
    set((s) => {
      const k = pmViewKey(host);
      if (!k || s.byHost[k] === view) return s;
      return { byHost: { ...s.byHost, [k]: view } };
    }),
}));

/** The overview last read for `host` (undefined: never read). */
export function usePmView(host: string): PmOverview | null | undefined {
  return usePmViewStore((s) => s.byHost[pmViewKey(host)]);
}
