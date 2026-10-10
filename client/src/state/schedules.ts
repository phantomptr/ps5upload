import { create } from "zustand";
import { useEffect, useRef } from "react";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { pushNotification } from "./notifications";

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
 * The runner ticks every 30 s and fires whatever came due since the last
 * tick, so a tick landing a minute late (a busy or throttled window) no
 * longer skips the day. A run that came due more than CATCH_UP_MS ago (the
 * app was closed or the computer asleep) is not fired late; it is recorded
 * as missed and shown in Activity, where it can be run by hand.
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
  /** Last fire ms. A run due at or before this has been handled. */
  lastFiredMs?: number;
  /** When the schedule was added or last switched on. Runs due before this
   *  are not reported as missed. Absent on schedules stored by older
   *  versions. */
  armedMs?: number;
  /** The most recent run that came due while the app couldn't fire it. */
  missedAtMs?: number;
}

const STORAGE_KEY = "ps5upload.schedules.v1";

interface ScheduleState {
  schedules: Schedule[];
  add: (s: Omit<Schedule, "id" | "lastFiredMs">) => void;
  update: (id: string, patch: Partial<Schedule>) => void;
  remove: (id: string) => void;
  /** Called by the runner; updates lastFiredMs in place. */
  markFired: (id: string, ms: number) => void;
  /** Called by the runner for a run it was too late to fire. */
  markMissed: (id: string, atMs: number) => void;
  /** Forget a missed run (dismissed, or run by hand). */
  clearMissed: (id: string) => void;
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
    const sch: Schedule = { ...s, id: genId(), armedMs: Date.now() };
    const next = [...get().schedules, sch];
    set({ schedules: next });
    persist(next);
  },
  update: (id, patch) => {
    const next = get().schedules.map((s) => {
      if (s.id !== id) return s;
      const merged = { ...s, ...patch };
      // Switching on re-arms: runs due while it was off aren't "missed".
      if (patch.enabled && !s.enabled) merged.armedMs = Date.now();
      return merged;
    });
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
  markMissed: (id, atMs) => {
    const next = get().schedules.map((s) =>
      s.id === id
        ? {
            ...s,
            missedAtMs: atMs,
            // A missed one-shot is over; it can still be run from Activity.
            enabled: s.kind === "once" ? false : s.enabled,
          }
        : s,
    );
    set({ schedules: next });
    persist(next);
  },
  clearMissed: (id) => {
    const next = get().schedules.map((s) => {
      if (s.id !== id) return s;
      const rest = { ...s };
      delete rest.missedAtMs;
      return rest;
    });
    set({ schedules: next });
    persist(next);
  },
}));

/** How late a run may still fire. Later than this it is reported as missed. */
export const CATCH_UP_MS = 15 * 60_000;

/** The latest time at or before `nowMs` this schedule was due, or null. */
export function latestDue(s: Schedule, nowMs: number): number | null {
  if (s.kind === "once") {
    return s.oneShotMs && s.oneShotMs <= nowMs ? s.oneShotMs : null;
  }
  if (!s.hhmm) return null;
  const [hh, mm] = s.hhmm.split(":").map((x) => parseInt(x, 10));
  if (isNaN(hh) || isNaN(mm) || hh < 0 || hh > 23 || mm < 0 || mm > 59) return null;
  if (s.kind === "weekly" && (!s.weekdays || s.weekdays.length === 0)) return null;
  // Walk back day by day (a week covers every weekly schedule).
  for (let back = 0; back <= 7; back++) {
    const d = new Date(nowMs);
    d.setDate(d.getDate() - back);
    d.setHours(hh, mm, 0, 0);
    const t = d.getTime();
    if (t > nowMs) continue;
    if (s.kind === "weekly" && !s.weekdays!.includes(d.getDay())) continue;
    return t;
  }
  return null;
}

export type ScheduleVerdict =
  | { kind: "fire"; dueMs: number }
  | { kind: "missed"; dueMs: number }
  | null;

/** What the runner should do with a schedule at `nowMs`: fire it, record a
 *  missed run, or nothing (not due, or that run was already handled). */
export function evaluateSchedule(
  s: Schedule,
  nowMs: number,
  catchUpMs: number = CATCH_UP_MS,
): ScheduleVerdict {
  if (!s.enabled) return null;
  const due = latestDue(s, nowMs);
  if (due === null) return null;
  if ((s.lastFiredMs ?? 0) >= due) return null;
  if ((s.missedAtMs ?? 0) >= due) return null;
  if (nowMs - due <= catchUpMs) {
    // Never fire a run that was due before the schedule existed or was
    // switched on (adding a 09:00 reminder at 09:05 shouldn't ring).
    if (s.armedMs !== undefined && due < s.armedMs) return null;
    return { kind: "fire", dueMs: due };
  }
  // Too late to fire. Only call it missed when we know the schedule was
  // live then; older stored schedules without armedMs just wait for the
  // next run.
  const since = Math.max(s.armedMs ?? 0, s.lastFiredMs ?? 0);
  if (since === 0 || due < since) return null;
  return { kind: "missed", dueMs: due };
}

/** Run a schedule's action now. The runner's fire path and Activity's
 *  "Run now" for a missed run both go through here. */
export function runScheduleAction(s: Schedule): void {
  if (s.action === "notif") {
    pushNotification("info", `Scheduled: ${s.label}`, {
      body: s.body ?? "Schedule fired.",
    });
  }
}

/** Subscribe-once runner. Mount this hook in AppShell to enable
 *  schedule firing. Ticks every 30 s and fires whatever came due since
 *  (see evaluateSchedule).
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
      const now = Date.now();
      const store = useScheduleStore.getState();
      for (const s of store.schedules) {
        const v = evaluateSchedule(s, now);
        if (v?.kind === "fire") {
          store.markFired(s.id, now);
          onFireRef.current(s);
        } else if (v?.kind === "missed") {
          store.markMissed(s.id, v.dueMs);
        }
      }
    };
    tick();
    const id = window.setInterval(tick, 30_000);
    return () => window.clearInterval(id);
  }, []);
}
