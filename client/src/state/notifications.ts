import { create } from "zustand";

import { appIsForeground, sendOsNotification } from "../lib/osNotify";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * Persistent notification inbox.
 *
 * Surfaces noteworthy events that the user might want to revisit
 * after they happened — completions, failures, version-update
 * availability, "PS5 went offline mid-upload" type things.
 *
 * Differs from `activityHistory.ts`: that store tracks the running
 * roster of operations and is consumed by the ActivityBar for
 * live status. Notifications are the *post-hoc* surface — "what
 * happened lately while I was on another screen?"
 *
 * Storage: localStorage with a 64-entry ring buffer cap so
 * notification history survives a reload / relaunch.
 */

export type NotificationLevel = "info" | "success" | "warning" | "error";

export interface Notification {
  id: string;
  /** Unix milliseconds. Use `new Date(ts)` for display. */
  ts: number;
  level: NotificationLevel;
  /** Headline — kept short, one line. UI truncates if needed. */
  title: string;
  /** Optional second line. Used for context, e.g. error message. */
  body?: string;
  /** Optional route hash — when present, the inbox row offers a
   *  "Jump to ..." link that navigates here. Pure convention; the
   *  UI just calls navigate() with this string. */
  link?: string;
  /** False until the user opens the inbox panel and the entry
   *  scrolls into view. Drives the unread badge count. */
  read: boolean;
}

interface NotificationsState {
  entries: Notification[];
  push: (
    level: NotificationLevel,
    title: string,
    extras?: { body?: string; link?: string },
  ) => string;
  markAllRead: () => void;
  clear: () => void;
  remove: (id: string) => void;
  unreadCount: () => number;
  /** Drop entries older than `maxAgeMs`. Returns the number pruned. */
  pruneOlderThan: (maxAgeMs: number) => number;
  /** Whether to mirror inbox entries to the OS notification center.
   *  Persisted; default on. */
  osNotifyEnabled: boolean;
  setOsNotifyEnabled: (enabled: boolean) => void;
}

const STORAGE_KEY = "ps5upload.notifications.v1";
const MAX_ENTRIES = 64;
const OS_NOTIFY_KEY = "ps5upload.osNotify.v1";

function loadOsNotifyEnabled(): boolean {
  if (typeof window === "undefined") return true;
  // Default ON; only an explicit "0" disables it.
  return safeGetItem(OS_NOTIFY_KEY) !== "0";
}

function loadInitial(): Notification[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = safeGetItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (e): e is Notification =>
        typeof e?.id === "string" &&
        typeof e?.ts === "number" &&
        typeof e?.title === "string" &&
        ["info", "success", "warning", "error"].includes(e?.level),
    );
  } catch {
    return [];
  }
}

function persist(entries: Notification[]) {
  if (typeof window === "undefined") return;
  try {
    safeSetItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // Quota exceeded — best-effort. We've already capped at
    // MAX_ENTRIES so this should be near-impossible in practice.
  }
}

function genId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return "n_" + Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
}

export const useNotificationsStore = create<NotificationsState>((set, get) => ({
  entries: loadInitial(),
  push: (level, title, extras) => {
    const entry: Notification = {
      id: genId(),
      ts: Date.now(),
      level,
      title,
      body: extras?.body,
      link: extras?.link,
      read: false,
    };
    // Newest first, ring-buffered.
    const next = [entry, ...get().entries].slice(0, MAX_ENTRIES);
    set({ entries: next });
    persist(next);
    // Mirror to the OS notification center / Android shade — but only
    // when the app isn't in the foreground, so the user doesn't get a
    // redundant system banner while looking at the in-app inbox.
    // Fire-and-forget: osNotify no-ops outside Tauri and swallows errors.
    if (get().osNotifyEnabled && !appIsForeground()) {
      void sendOsNotification(level, title, extras?.body);
    }
    return entry.id;
  },
  markAllRead: () => {
    const next = get().entries.map((e) => ({ ...e, read: true }));
    set({ entries: next });
    persist(next);
  },
  clear: () => {
    set({ entries: [] });
    persist([]);
  },
  remove: (id) => {
    const next = get().entries.filter((e) => e.id !== id);
    set({ entries: next });
    persist(next);
  },
  unreadCount: () => get().entries.filter((e) => !e.read).length,
  osNotifyEnabled: loadOsNotifyEnabled(),
  setOsNotifyEnabled: (enabled) => {
    if (typeof window !== "undefined") {
      safeSetItem(OS_NOTIFY_KEY, enabled ? "1" : "0");
    }
    set({ osNotifyEnabled: enabled });
  },
  pruneOlderThan: (maxAgeMs) => {
    const cutoff = Date.now() - maxAgeMs;
    const before = get().entries.length;
    const next = get().entries.filter((e) => e.ts >= cutoff);
    if (next.length === before) return 0;
    set({ entries: next });
    persist(next);
    return before - next.length;
  },
}));

/** Convenience: push a notification without subscribing to the
 *  store. Use from callers that just want to fire-and-forget. */
export function pushNotification(
  level: NotificationLevel,
  title: string,
  extras?: { body?: string; link?: string },
): string {
  return useNotificationsStore.getState().push(level, title, extras);
}

const PRUNE_DAYS_KEY = "ps5upload.notif_prune_days.v1";
const DEFAULT_PRUNE_DAYS = 30;

/** How many days to keep notifications. 0 = never prune.
 *  Stored as a number string in localStorage; clamped to [0, 365]. */
export function getNotifPruneDays(): number {
  if (typeof window === "undefined") return DEFAULT_PRUNE_DAYS;
  try {
    const raw = safeGetItem(PRUNE_DAYS_KEY);
    if (raw === null) return DEFAULT_PRUNE_DAYS;
    const n = parseInt(raw, 10);
    if (!Number.isFinite(n) || n < 0) return DEFAULT_PRUNE_DAYS;
    return Math.min(n, 365);
  } catch {
    return DEFAULT_PRUNE_DAYS;
  }
}

export function setNotifPruneDays(days: number) {
  if (typeof window === "undefined") return;
  const clamped = Math.max(0, Math.min(365, Math.floor(days)));
  try {
    safeSetItem(PRUNE_DAYS_KEY, String(clamped));
  } catch {
    // best-effort
  }
}

/** Run once at startup + every 6 hours while the app stays open.
 *  No-ops when prune days is 0 (user disabled). */
export function runNotificationAutoPrune() {
  const days = getNotifPruneDays();
  if (days <= 0) return;
  useNotificationsStore.getState().pruneOlderThan(days * 86400 * 1000);
}
