import { create } from "zustand";
import { useConnectionStore } from "./connection";
import { useRunningAppsStore } from "./runningApps";
import { useFsClipboardStore, evictFsClipboard } from "./fsClipboard";
import { useUploadStore, evictUploadDraft } from "./upload";
import { evictPkgLibraryStore } from "./pkgLibrary";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

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
    const raw = safeGetItem(ROSTER_STORAGE_KEY);
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
    safeSetItem(
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
  return (
    "p_" + Math.random().toString(36).slice(2, 10) + Date.now().toString(36)
  );
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
    // The FS cut/copy clipboard holds paths from the PREVIOUS console — a
    // paste after switching would target the new console with the old one's
    // paths. Stash/restore it per console so a cut/copy survives a round-trip
    // switch but never pastes one console's paths onto another. Before setHost.
    useFsClipboardStore.getState().switchToHost(profile.host);
    // (Library is per-console now — `useLibraryStore` is keyed byHost, so the
    // new console shows its own slot immediately and there's nothing to clear.
    // The wrong-target window the old clear-on-switch guarded against is gone.)
    // Stash THIS console's upload draft and restore the target console's, so a
    // switch preserves each PS5's in-progress upload (picked file + options).
    // Must run BEFORE setHost (the connection store must still hold the old
    // host the current draft belongs to).
    useUploadStore.getState().switchToHost(profile.host);
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
    const removed = get().profiles.find((p) => p.id === id);
    const next = get().profiles.filter((p) => p.id !== id);
    // The removed host is "orphaned" (safe to evict its per-console state) only
    // if no remaining profile still points at that bare host — two profiles can
    // share an IP.
    const hostOrphaned = (h: string) =>
      !next.some((p) => hostOf(p.host) === hostOf(h));
    // The pkg-library store is NOT re-created by switchToHost below, so it's
    // safe to evict up front.
    if (removed && hostOrphaned(removed.host)) {
      evictPkgLibraryStore(removed.host);
    }
    let next_active = get().active_id;
    if (next_active === id) {
      next_active = next[0]?.id ?? null;
      if (next[0]) {
        // Removing the active console promotes another one — tear down the
        // removed console's per-host state exactly like setActive() does, so
        // the newly-active console doesn't inherit stale running-apps badges,
        // FS clipboard paths, or Library entries from the one just deleted.
        useRunningAppsStore.getState().clearForHostChange(next[0].host);
        // Restore the promoted console's own clipboard + upload draft.
        useFsClipboardStore.getState().switchToHost(next[0].host);
        useUploadStore.getState().switchToHost(next[0].host);
        useConnectionStore.getState().setHost(next[0].host);
      }
    }
    // Evict the removed host's upload draft + FS clipboard AFTER the promotion
    // above. switchToHost stashes the CURRENT console's draft/clipboard under
    // connection.host — which is still the REMOVED host until setHost() runs in
    // that block. Evicting first would let switchToHost immediately re-stash the
    // removed console's state, so a future console reusing the same IP would
    // wrongly inherit it. Doing it here clears that just-created stash.
    if (removed && hostOrphaned(removed.host)) {
      evictUploadDraft(removed.host);
      evictFsClipboard(removed.host);
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
    const oldHost = get().profiles.find((p) => p.id === id)?.host;
    const next = get().profiles.map((p) =>
      p.id === id ? { ...p, host: host.trim() } : p,
    );
    // Re-pointing a profile at a new IP orphans the old IP's pkg-library
    // store; evict it (unless another profile still uses that bare host) so
    // it can't resurface stale state if that IP is reused later.
    if (
      oldHost &&
      hostOf(oldHost) !== hostOf(host.trim()) &&
      !next.some((p) => hostOf(p.host) === hostOf(oldHost))
    ) {
      evictPkgLibraryStore(oldHost);
      // Drop the now-orphaned old-IP stashes; the ACTIVE draft + clipboard are
      // kept below (this is the same console with a new address, so its
      // in-progress upload and cut/copy should follow the profile).
      evictUploadDraft(oldHost);
      evictFsClipboard(oldHost);
    }
    set({ profiles: next });
    persist(next, get().active_id);
    if (get().active_id === id) {
      // Re-pointing the active profile at a different IP: clear only the live,
      // address-tied state (running-apps badges) so a stale Unmount can't
      // target the wrong PS5. The upload DRAFT and the cut/copy CLIPBOARD are
      // deliberately KEPT — it's the same console, so they follow it to the new
      // address (the clipboard holds PS5-side paths, valid regardless of IP).
      useRunningAppsStore.getState().clearForHostChange(host.trim());
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

/**
 * Pure: the friendly console name for a bare host/IP, given the roster.
 * Falls back to the bare host when no profile matches or the matched
 * profile has no name. Port-tolerant: callers may pass an `ip:port`
 * addr and it's stripped via `hostOf`.
 *
 * Used to label upload/install queue rows with WHICH console each job
 * targets — every queue item already carries an `addr`, but the roster
 * is keyed by profile id/host, so a queue item can't show a name
 * without this reverse lookup. Pure (roster passed in) so it unit-tests
 * without a store or a PS5.
 */
export function profileNameForHost(
  host: string,
  profiles: PS5Profile[],
): string {
  const bare = hostOf(host);
  if (!bare) return host;
  const match = profiles.find((p) => hostOf(p.host) === bare);
  return match?.name?.trim() || bare;
}

/** Pure: friendly console name for an `ip:port` transfer/mgmt addr. */
export function profileNameForAddr(
  addr: string,
  profiles: PS5Profile[],
): string {
  return profileNameForHost(addr, profiles);
}

/** Reactive hook: the console label for a queue item's `addr`. Re-renders
 *  when the roster changes (e.g. the user renames a console). */
export function useConsoleLabel(addr: string): string {
  return useRosterStore((s) => profileNameForAddr(addr, s.profiles));
}

/**
 * Per-console accent palette. With two consoles both mid-upload, names
 * alone are easy to confuse at a glance (especially defaults like
 * "PS5 (192.168.1.50)" vs "PS5 (192.168.1.51)") — a stable color per
 * console gives every attribution surface (tab strip, activity rows,
 * status dots) a second, pre-attentive identity channel.
 *
 * Assignment is by roster position, so the first console is always
 * blue, the second violet, etc. Position is stable in practice (the
 * roster is append-ordered and removals are rare); deriving from the
 * profile id hash instead would survive removals but risks two
 * consoles landing on the SAME color, which defeats the purpose for
 * the 2-console case this exists for. Colors are oklch-ish mid-tones
 * that read on all three themes.
 */
const CONSOLE_ACCENTS = [
  "#3b82f6", // blue
  "#a855f7", // violet
  "#f59e0b", // amber
  "#10b981", // emerald
  "#ec4899", // pink
  "#06b6d4", // cyan
  "#ef4444", // red
  "#84cc16", // lime
];

/** Pure: the accent color for a host/addr, or null when the host isn't
 *  in the roster. Port-tolerant like `profileNameForHost`. */
export function profileAccentForHost(
  host: string,
  profiles: PS5Profile[],
): string | null {
  const bare = hostOf(host);
  if (!bare) return null;
  const idx = profiles.findIndex((p) => hostOf(p.host) === bare);
  if (idx < 0) return null;
  return CONSOLE_ACCENTS[idx % CONSOLE_ACCENTS.length];
}

/** Reactive hook: the accent color for a console's host/addr. */
export function useConsoleAccent(addr: string): string | null {
  return useRosterStore((s) => profileAccentForHost(addr, s.profiles));
}

/**
 * Prefix a notification title with the console it concerns —
 * "Living-room PS5: Backed up CUSA34882" — but only when the roster has
 * more than one console (single-PS5 users don't need telling which).
 *
 * For two consoles mid-work, an unprefixed "PS5 reboot requested" or
 * "Backed up …" toast is unanswerable; this is the one-line fix every
 * notification call site applies on its way into pushNotification (and
 * therefore into the OS notification mirror, which reuses the title).
 * Non-reactive by design: notifications are fire-and-forget strings
 * minted at event time, so a snapshot read is correct.
 */
export function withConsolePrefix(
  host: string | null | undefined,
  title: string,
): string {
  if (!host?.trim()) return title;
  const profiles = useRosterStore.getState().profiles;
  if (profiles.length < 2) return title;
  return `${profileNameForHost(host, profiles)}: ${title}`;
}

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
  const { profiles, active_id, add, setActive } = useRosterStore.getState();
  if (profiles.length > 0) {
    // The roster's active profile is the single source of truth for which
    // console is selected. connection.host persists separately (its own
    // localStorage key) and can DRIFT from active_id across sessions — e.g. a
    // stale `ps5upload.host` from before the roster existed, leaving the tab
    // strip showing console A while the screens read console B's data. On
    // load, reconcile connection.host to the active profile.
    const active = profiles.find((p) => p.id === active_id) ?? profiles[0];
    if (!active) return;
    if (active.id !== active_id) {
      setActive(active.id); // also syncs connection.host + clears per-host view state
    } else if (useConnectionStore.getState().host !== active.host) {
      useConnectionStore.getState().setHost(active.host);
    }
    return;
  }
  // No roster yet: seed one from whatever single host was persisted.
  const host = useConnectionStore.getState().host?.trim();
  if (!host) return;
  const id = add({ name: `PS5 (${host})`, host });
  setActive(id);
}
