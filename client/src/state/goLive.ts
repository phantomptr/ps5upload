import { create } from "zustand";

import {
  goLiveApi,
  LiveApiError,
  type DestinationDraft,
  type LiveDestination,
  type StreamStatus,
} from "../lib/goLive";

/** Go live's state for the Play screen (engine rp_golive.rs; the panel is GoLivePanel.tsx). */
interface GoLiveState {
  status: StreamStatus | null;
  /** The engine could not be asked (an older engine has no Go live: 404). */
  loadError: string | null;
  /** A button's request is on its way. */
  busy: boolean;
  /** The last refusal from a button (start, add…). */
  actionError: LiveApiError | null;
  refresh: () => Promise<void>;
  start: () => Promise<boolean>;
  stop: () => Promise<void>;
  add: (d: DestinationDraft) => Promise<LiveDestination | null>;
  setEnabled: (id: string, enabled: boolean) => Promise<void>;
  setBandwidthTest: (id: string, on: boolean) => Promise<void>;
  remove: (id: string) => Promise<void>;
  installFfmpeg: () => Promise<void>;
  clearError: () => void;
}

function asError(e: unknown): LiveApiError {
  if (e instanceof LiveApiError) return e;
  return new LiveApiError("network", e instanceof Error ? e.message : String(e));
}

export const useGoLiveStore = create<GoLiveState>((set, get) => ({
  status: null,
  loadError: null,
  busy: false,
  actionError: null,

  async refresh() {
    try {
      const status = await goLiveApi.status();
      set({ status, loadError: null });
    } catch (e) {
      set({ loadError: asError(e).code });
    }
  },

  async start() {
    const ids = (get().status?.destinations ?? []).filter((d) => d.enabled).map((d) => d.id);
    set({ busy: true, actionError: null });
    try {
      await goLiveApi.start(ids);
      await get().refresh();
      return true;
    } catch (e) {
      set({ actionError: asError(e) });
      await get().refresh();
      return false;
    } finally {
      set({ busy: false });
    }
  },

  async stop() {
    set({ busy: true });
    try {
      await goLiveApi.stop();
    } catch (e) {
      set({ actionError: asError(e) });
    } finally {
      set({ busy: false });
      await get().refresh();
    }
  },

  async add(d) {
    set({ busy: true, actionError: null });
    try {
      const v = await goLiveApi.add(d);
      await get().refresh();
      return v;
    } catch (e) {
      set({ actionError: asError(e) });
      return null;
    } finally {
      set({ busy: false });
    }
  },

  async setEnabled(id, enabled) {
    try {
      await goLiveApi.update(id, { enabled });
    } catch (e) {
      set({ actionError: asError(e) });
    }
    await get().refresh();
  },

  async setBandwidthTest(id, on) {
    try {
      await goLiveApi.update(id, { bandwidth_test: on });
    } catch (e) {
      set({ actionError: asError(e) });
    }
    await get().refresh();
  },

  async remove(id) {
    try {
      await goLiveApi.remove(id);
    } catch (e) {
      set({ actionError: asError(e) });
    }
    await get().refresh();
  },

  async installFfmpeg() {
    set({ actionError: null });
    try {
      await goLiveApi.installFfmpeg();
    } catch (e) {
      set({ actionError: asError(e) });
    }
    await get().refresh();
  },

  clearError() {
    set({ actionError: null });
  },
}));
