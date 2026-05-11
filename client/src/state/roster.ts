import { create } from "zustand";
import { useConnectionStore } from "./connection";
import { useRunningAppsStore } from "./runningApps";

/**
 * Multi-PS5 roster.
 *
 * Lets a user maintain named profiles for multiple consoles
 * (living-room PS5, bedroom PS5, dev console) and switch between
 * them in one click — instead of retyping the IP every time and
 * losing the previous host's per-console state.
 *
 * Storage strategy: localStorage. Same as `connection.ts::host` so
 * the roster is available synchronously during first paint without
 * waiting for a Tauri round-trip. The on-disk settings.json mirror
 * (`state/userConfig.ts`) snapshots ps5_host but not the full
 * roster yet — that's a follow-up if/when we want roster sync
 * across machines.
 *
 * Migration: on first load, if a host is set in connection store
 * but no roster exists, seed the roster with one entry pointing at
 * that host. Existing single-PS5 users get a transparent upgrade
 * with no manual setup.
 *
 * Why a separate store from connection.ts: keeps the connection
 * store focused on "current PS5's live state" (probe results,
 * version, kernel) while the roster owns "the list of known
 * consoles." Switching active profile mutates connection.host;
 * everything downstream of it (probes, status pills, payload
 * version) re-fires naturally via the existing reactive flow.
 */

const ROSTER_STORAGE_KEY = "ps5upload.roster.v1";

export interface PS5Profile {
  /** Stable id; UUID generated on creation. Used as the React key
   *  and as the lookup key when activating a profile. */
  id: string;
  /** User-visible name, e.g. "Living-room PS5". */
  name: string;
  /** IP address. Same shape as `connection.host`. */
  host: string;
  /** Last-seen kernel.version string. Cached so the roster row can
   *  show firmware without a probe round-trip. Updated by
   *  AppShell's status poller via `noteSeen`. */
  last_seen_kernel?: string | null;
  /** Last-seen ps5upload payload version string. Cached so the row
   *  shows "v2.2.61" without forcing a probe. */
  last_seen_payload?: string | null;
  /** Unix seconds; last time this profile's host successfully
   *  responded to a probe. Surfaces "stale: not seen in 3 weeks"
   *  in the row. */
  last_seen_at?: number | null;
  /** Free-text note set by the user. Useful for things like
   *  "9.00 firmware, do NOT update" or "test box, fan loud at idle".
   *  No length limit at the type level; UI textarea soft-limits to
   *  ~500 chars to keep the picker tooltip legible. */
  notes?: string | null;
}

interface RosterState {
  profiles: PS5Profile[];
  /** Id of the currently-active profile. The store's `host` value
   *  in `connection.ts` mirrors this profile's host; switching here
   *  updates the connection store via `setActive`. */
  active_id: string | null;
  setActive: (id: string) => void;
  add: (input: { name: string; host: string }) => string;
  remove: (id: string) => void;
  rename: (id: string, name: string) => void;
  updateHost: (id: string, host: string) => void;
  setNotes: (id: string, notes: string) => void;
  /** Called by AppShell's status poller when a probe lands a fresh
   *  kernel/payload pair, so the roster row stays current without
   *  the user opening Settings. */
  noteSeen: (
    id: string,
    patch: {
      kernel?: string | null;
      payload?: string | null;
    },
  ) => void;
}

function loadStored(): { profiles: PS5Profile[]; active_id: string | null } {
  if (typeof window === "undefined") return { profiles: [], active_id: null };
  try {
    const raw = window.localStorage.getItem(ROSTER_STORAGE_KEY);
    if (!raw) return { profiles: [], active_id: null };
    const parsed = JSON.parse(raw) as {
      profiles?: PS5Profile[];
      active_id?: string | null;
    };
    return {
      profiles: Array.isArray(parsed.profiles) ? parsed.profiles : [],
      active_id: typeof parsed.active_id === "string" ? parsed.active_id : null,
    };
  } catch {
    return { profiles: [], active_id: null };
  }
}

function persist(profiles: PS5Profile[], active_id: string | null) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(
      ROSTER_STORAGE_KEY,
      JSON.stringify({ profiles, active_id }),
    );
  } catch {
    // Quota exceeded / private mode — best-effort persistence.
  }
}

function genId(): string {
  // Avoid pulling in uuid for one-off id minting; crypto.randomUUID
  // is widely supported in Tauri's webview as of v2 (Edge WebView2,
  // WebKit 16+, recent webkit2gtk).
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  // Fallback for ancient webkit2gtk builds — RFC 4122-ish, not
  // cryptographically strong but unique enough for our scope.
  return "p_" + Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
}

const initial = loadStored();

export const useRosterStore = create<RosterState>((set, get) => ({
  profiles: initial.profiles,
  active_id: initial.active_id,
  setActive: (id) => {
    const profile = get().profiles.find((p) => p.id === id);
    if (!profile) return;
    set({ active_id: id });
    persist(get().profiles, id);
    // Drop the previously-active console's running-apps set so
    // Library rows don't briefly show "running" badges keyed to
    // the wrong PS5. RunningAppsPanel will repopulate after the
    // first poll on the new host.
    useRunningAppsStore.getState().clearForHostChange(profile.host);
    // Sync connection store. AppShell's host-watcher kicks off
    // probes against the new host, populating
    // payloadStatus/payloadVersion/ps5Kernel naturally.
    useConnectionStore.getState().setHost(profile.host);
  },
  add: ({ name, host }) => {
    const id = genId();
    const profile: PS5Profile = {
      id,
      name: name.trim() || `PS5 (${host.trim()})`,
      host: host.trim(),
      last_seen_at: null,
      last_seen_kernel: null,
      last_seen_payload: null,
    };
    const next = [...get().profiles, profile];
    // First profile becomes active automatically — single-PS5 users
    // never need to think about the active selection.
    const next_active = get().active_id ?? id;
    set({ profiles: next, active_id: next_active });
    persist(next, next_active);
    if (next_active === id) {
      useConnectionStore.getState().setHost(profile.host);
    }
    return id;
  },
  remove: (id) => {
    const next = get().profiles.filter((p) => p.id !== id);
    let next_active = get().active_id;
    if (next_active === id) {
      next_active = next[0]?.id ?? null;
      if (next[0]) {
        useConnectionStore.getState().setHost(next[0].host);
      }
    }
    set({ profiles: next, active_id: next_active });
    persist(next, next_active);
  },
  rename: (id, name) => {
    const next = get().profiles.map((p) =>
      p.id === id ? { ...p, name: name.trim() || p.name } : p,
    );
    set({ profiles: next });
    persist(next, get().active_id);
  },
  updateHost: (id, host) => {
    const next = get().profiles.map((p) =>
      p.id === id ? { ...p, host: host.trim() } : p,
    );
    set({ profiles: next });
    persist(next, get().active_id);
    if (get().active_id === id) {
      useConnectionStore.getState().setHost(host.trim());
    }
  },
  setNotes: (id, notes) => {
    const trimmed = notes.length > 2000 ? notes.slice(0, 2000) : notes;
    const next = get().profiles.map((p) =>
      p.id === id ? { ...p, notes: trimmed } : p,
    );
    set({ profiles: next });
    persist(next, get().active_id);
  },
  noteSeen: (id, patch) => {
    const now = Math.floor(Date.now() / 1000);
    const next = get().profiles.map((p) =>
      p.id === id
        ? {
            ...p,
            last_seen_at: now,
            last_seen_kernel:
              patch.kernel !== undefined ? patch.kernel : p.last_seen_kernel,
            last_seen_payload:
              patch.payload !== undefined ? patch.payload : p.last_seen_payload,
          }
        : p,
    );
    set({ profiles: next });
    persist(next, get().active_id);
  },
}));

/** Get the currently-active profile, or null when none exists. */
export function useActiveProfile(): PS5Profile | null {
  return useRosterStore((s) => {
    if (!s.active_id) return null;
    return s.profiles.find((p) => p.id === s.active_id) ?? null;
  });
}

/** One-time migration: if the connection store has a host but the
 *  roster is empty, seed a default profile from it. Call this from
 *  AppShell's mount effect. Idempotent — safe to call on every
 *  startup. */
export function ensureRosterMigrated() {
  const { profiles, add, setActive } = useRosterStore.getState();
  if (profiles.length > 0) return;
  const host = useConnectionStore.getState().host?.trim();
  if (!host) return;
  const id = add({ name: `PS5 (${host})`, host });
  setActive(id);
}
