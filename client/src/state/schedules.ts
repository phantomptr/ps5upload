import { create } from "zustand";
import { useEffect, useRef } from "react";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * Browser-side scheduled operations.
 *
 * Limitation: only fires while the app window is open. Tauri without
 * the autostart plugin (and we don't ship one) can't reliably wake
 * for cron-style firings; for true cron behaviour the user would
 * need a system cron + curl on our HTTP API. The UX nudge for that
 * case lives in the schedule editor.
 *
 * Schedule kinds:
 *   - daily: fires at HH:MM every day
 *   - weekly: fires at HH:MM on the named weekday(s)
 *   - once: fires on a specific timestamp; one-shot then disabled
 *
 * Action kinds:
 *   - notif: just push a notification (lets users build "remind me
 *     to back up tonight" without an action handler)
 *
 * Older versions also had a "power_tick" action. One tick a day never kept
 * a console awake (Keep the PS5 awake → Always does that), so stored
 * schedules of that kind are dropped on load.
 */

export type ScheduleKind = "daily" | "weekly" | "once";
export type ScheduleAction = "notif";

export interface Schedule {
  id: string;
  enabled: boolean;
  kind: ScheduleKind;
  action: ScheduleAction;
  /** HH:MM 24h, used by daily/weekly. */
  hhmm?: string;
  /** Comma-separated 0-6 (0=Sunday), used by weekly. */
  weekdays?: number[];
  /** Unix ms, used by once. */
  oneShotMs?: number;
  /** Display name. */
  label: string;
  /** Optional payload for the action. */
  body?: string;
  /** Last fire ms (debounce — don't double-fire within 1 minute). */
  lastFiredMs?: number;
}

const STORAGE_KEY = "ps5upload.schedules.v1";

interface ScheduleState {
  schedules: Schedule[];
  add: (s: Omit<Schedule, "id" | "lastFiredMs">) => void;
  update: (id: string, patch: Partial<Schedule>) => void;
  remove: (id: string) => void;
  /** Called by the runner; updates lastFiredMs in place. */
  markFired: (id: string, ms: number) => void;
}

function loadInitial(): Schedule[] {
  if (typeof window === "undefined") return [];
  return parseSchedules(safeGetItem(STORAGE_KEY));
}

/** The stored list → the schedules this version can run. Anything with an
 *  action it no longer has (the old "power_tick") is dropped, so it never
 *  fires and disappears from storage on the next write. Exported for tests. */
export function parseSchedules(raw: string | null): Schedule[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (s): s is Schedule => typeof s?.id === "string" && s.action === "notif",
    );
  } catch {
    return [];
  }
}

function persist(schedules: Schedule[]) {
  if (typeof window === "undefined") return;
  try {
    safeSetItem(STORAGE_KEY, JSON.stringify(schedules));
  } catch {
    // best-effort
  }
}

function genId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return "sch_" + Math.random().toString(36).slice(2, 10);
}

export const useScheduleStore = create<ScheduleState>((set, get) => ({
  schedules: loadInitial(),
  add: (s) => {
    const sch: Schedule = { ...s, id: genId() };
    const next = [...get().schedules, sch];
    set({ schedules: next });
    persist(next);
  },
  update: (id, patch) => {
    const next = get().schedules.map((s) =>
      s.id === id ? { ...s, ...patch } : s,
    );
    set({ schedules: next });
    persist(next);
  },
  remove: (id) => {
    const next = get().schedules.filter((s) => s.id !== id);
    set({ schedules: next });
    persist(next);
  },
  markFired: (id, ms) => {
    const next = get().schedules.map((s) =>
      s.id === id
        ? {
            ...s,
            lastFiredMs: ms,
            enabled: s.kind === "once" ? false : s.enabled,
          }
        : s,
    );
    set({ schedules: next });
    persist(next);
  },
}));

/** Returns true when the schedule should fire at the given moment. */
export function shouldFire(s: Schedule, now: Date): boolean {
  if (!s.enabled) return false;
  // Debounce: don't fire twice within 60s.
  if (s.lastFiredMs && Date.now() - s.lastFiredMs < 60_000) return false;
  if (s.kind === "once") {
    if (!s.oneShotMs) return false;
    return now.getTime() >= s.oneShotMs;
  }
  if (!s.hhmm) return false;
  const [hh, mm] = s.hhmm.split(":").map((x) => parseInt(x, 10));
  if (isNaN(hh) || isNaN(mm)) return false;
  if (now.getHours() !== hh || now.getMinutes() !== mm) return false;
  if (s.kind === "weekly") {
    if (!s.weekdays || !s.weekdays.includes(now.getDay())) return false;
  }
  return true;
}

/** Subscribe-once runner. Mount this hook in AppShell to enable
 *  schedule firing. Ticks every 30 s; the per-minute resolution is
 *  fine for cron-like UX.
 *
 *  Stable-callback pattern: callers (AppShell) usually pass an inline
 *  arrow whose identity changes on every render, so naming `onFire`
 *  in deps would tear down + rebuild the 30s timer 5-10× per minute
 *  instead of letting it run. We stash the latest callback in a ref
 *  and read through it from inside `tick`. */
export function useScheduleRunner(onFire: (s: Schedule) => void) {
  const onFireRef = useRef(onFire);
  // Sync the ref to the latest callback without re-running the
  // install effect. Writing during render trips react-hooks/refs;
  // a layout effect happens before browser paint so the timer
  // ticks always see the freshest callback.
  useEffect(() => {
    onFireRef.current = onFire;
  }, [onFire]);
  useEffect(() => {
    const tick = () => {
      const now = new Date();
      const schedules = useScheduleStore.getState().schedules;
      for (const s of schedules) {
        if (shouldFire(s, now)) {
          useScheduleStore.getState().markFired(s.id, Date.now());
          onFireRef.current(s);
        }
      }
    };
    tick();
    const id = window.setInterval(tick, 30_000);
    return () => window.clearInterval(id);
  }, []);
}
