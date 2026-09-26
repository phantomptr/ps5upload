// The saved servers, as the app shows them. Secrets never live here: a typed password goes
// straight to the engine, which keeps it encrypted, and comes back only as `has_secret`.

import { create } from "zustand";

import {
  remoteApi,
  type Connection,
  type ConnectionInput,
  type SecretInput,
} from "../api/remote";

export type ConnStatus = "unknown" | "reachable" | "auth-failed" | "offline";

export interface ConnectionsState {
  connections: Connection[];
  loaded: boolean;
  status: Record<string, ConnStatus>;
  load: () => Promise<void>;
  /** Add (`id` null) or edit; an edit without a secret keeps the saved one. */
  save: (id: string | null, c: ConnectionInput, s?: SecretInput) => Promise<Connection>;
  remove: (id: string) => Promise<void>;
  /** Sign in and look at the start folder; records and returns the result. */
  check: (id: string) => Promise<ConnStatus>;
  nameOf: (id: string) => string | undefined;
}

export function statusOf(error: string): ConnStatus {
  if (/sign-in failed/i.test(error)) return "auth-failed";
  if (/can't reach|unreachable|refused|timed out|timeout/i.test(error)) return "offline";
  return "offline";
}

export const useConnectionsStore = create<ConnectionsState>((set, get) => ({
  connections: [],
  loaded: false,
  status: {},

  load: async () => {
    const connections = await remoteApi.list();
    set({ connections, loaded: true });
  },

  save: async (id, c, s) => {
    const saved = id ? await remoteApi.update(id, c, s) : await remoteApi.add(c, s);
    set((st) => ({
      connections: id
        ? st.connections.map((x) => (x.id === id ? saved : x))
        : [...st.connections, saved],
      status: { ...st.status, [saved.id]: "unknown" },
    }));
    return saved;
  },

  remove: async (id) => {
    await remoteApi.remove(id);
    set((st) => {
      const status = { ...st.status };
      delete status[id];
      return { connections: st.connections.filter((x) => x.id !== id), status };
    });
  },

  check: async (id) => {
    let result: ConnStatus;
    try {
      const r = await remoteApi.test(id);
      result = r.ok ? "reachable" : statusOf(r.error);
    } catch (e) {
      result = statusOf(e instanceof Error ? e.message : String(e));
    }
    set((st) => ({ status: { ...st.status, [id]: result } }));
    return result;
  },

  nameOf: (id) => get().connections.find((c) => c.id === id)?.name,
}));
