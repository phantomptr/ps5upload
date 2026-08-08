import { create } from "zustand";
import { useEffect, useMemo } from "react";

import { hostOf, transferAddr } from "../lib/addr";
import { fetchHwTemps, fetchHwPower, type HwTemps, type HwPower } from "../api/ps5";
import { useConnectionStore } from "./connection";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { isTerminal, useTaskStore } from "./tasks";

/**
 * v5 §11 — Telemetry store.
 *
 * Centralizes live sensor polling that was previously duplicated across
 * Home, Dashboard, and Hardware screens (three independent 5s intervals
 * all hitting the same RPCs). This store runs ONE poll per connected
 * host and fans samples out reactively via Zustand selectors.
 *
 * The v5 spec calls for replacing this with a single SSE stream
 * (`GET /api/telemetry/stream`). The engine already has SSE plumbing
 * (`/api/events`) but currently only broadcasts job state — not sensors.
 * When sensor SSE lands, swap `startPoll` to subscribe to the stream
 * and keep the same store interface; consumers won't need to change.
 *
 * ── Ring buffer ────────────────────────────────────────────────
 * Each host keeps the last `MAX_SAMPLES` readings (default 120 = 10 min
 * at 5s cadence). This is enough for a sparkline without bloating
 * memory. Samples are trimmed FIFO.
 *
 * ── Reference counting ─────────────────────────────────────────
 * `subscribe(host)` increments a refcount; `unsubscribe(host)` decrements.
 * The poll loop runs only while ≥1 subscriber exists. This lets the
 * Home widget mount/unmount without spinning up a global poll that fires
 * even when no one is looking.
 *
 * ── Safety ─────────────────────────────────────────────────────
 * Uses the BASIC (non-extended) `fetchHwTemps(addr)` — the extended read
 * (`extended=true`) can wedge untested firmware and is gated to explicit
 * user action on the Hardware screen. This store never sends extended.
 *
 * ── Polling guards ─────────────────────────────────────────────
 * • Skips when `document.hidden` (tab not visible — no point burning
 *   the network and payload CPU).
 * • `inFlight` flag prevents pile-up on slow networks.
 * • Pauses when THIS host's payload is not up.
 * • Pauses while a transfer/install owns this host's payload. This guard
 *   existed in v4 after #164 and must not be dropped when polling moves.
 */

const POLL_MS = 5000;
const MAX_SAMPLES = 120;

export interface SensorSample {
  /** ms-since-epoch when the sample was taken. */
  ts: number;
  /** A poll is intentionally partial: one endpoint may work while the other
   *  is unavailable on a firmware. Never throw away the successful half. */
  temps: HwTemps | null;
  power: HwPower | null;
}

interface HostBucket {
  samples: SensorSample[];
  /** Most recent successful reading (mirrors `samples.at(-1)` but
   *  nullable for "no data yet" without array indexing). */
  latest: SensorSample | null;
}

interface SensorsState {
  /** Per-host ring buffers, keyed by `hostOf(addr)`. */
  byHost: Record<string, HostBucket>;

  /** Record a sample into the host's ring buffer. */
  record: (host: string, sample: SensorSample) => void;
  /** Clear a host's history (e.g. on disconnect). */
  clear: (host: string) => void;
}

export const useSensorsStore = create<SensorsState>((set) => ({
  byHost: {},
  record: (host, sample) =>
    set((s) => {
      const key = hostOf(host) || host;
      const prev = s.byHost[key];
      const samples = prev
        ? [...prev.samples, sample].slice(-MAX_SAMPLES)
        : [sample];
      return {
        byHost: {
          ...s.byHost,
          [key]: { samples, latest: sample },
        },
      };
    }),
  clear: (host) =>
    set((s) => {
      const key = hostOf(host) || host;
      if (!s.byHost[key]) return s;
      const next = { ...s.byHost };
      delete next[key];
      return { byHost: next };
    }),
}));

// ── Selectors ──────────────────────────────────────────────────────

// Stable empty array returned by selectors when there's no data yet.
// Returning a literal `[]` here would create a NEW array reference on
// every store read, which Zustand's useSyncExternalStore detects as a
// changed snapshot → infinite re-render ("Maximum update depth
// exceeded"). This constant keeps the reference stable.
const EMPTY_SAMPLES: SensorSample[] = [];

/** Read the latest sample for a host, reactively. */
export function useLatestSample(
  host: string | null | undefined,
): SensorSample | null {
  return useSensorsStore((s) => {
    if (!host) return null;
    const key = hostOf(host) || host;
    return s.byHost[key]?.latest ?? null;
  });
}

/** Read the sample history for a host (oldest → newest), reactively.
 *  Returns a stable EMPTY_SAMPLES when no data exists. */
export function useSampleHistory(
  host: string | null | undefined,
): SensorSample[] {
  return useSensorsStore((s) => {
    if (!host) return EMPTY_SAMPLES;
    const key = hostOf(host) || host;
    return s.byHost[key]?.samples ?? EMPTY_SAMPLES;
  });
}

// ── Poll loop (ref-counted singleton) ──────────────────────────────

interface PollHandle {
  refcount: number;
  timer: number | null;
  inFlight: boolean;
  cancelled: boolean;
}

const handles = new Map<string, PollHandle>();

/** Resolve connection state for the host being polled, not whichever console
 * happens to be selected in the UI. The flat fields are only a compatibility
 * mirror for the active console; background subscribers can target another
 * roster entry. */
function payloadIsUpForHost(host: string): boolean {
  const state = useConnectionStore.getState();
  const key = hostOf(host) || "_";
  const runtime = state.runtimeByHost[key];
  if (runtime) return runtime.payloadStatus === "up";

  // Compatibility fallback for startup/tests before the fan-out poller has
  // populated runtimeByHost. Never borrow a status from another console.
  const activeKey = hostOf(state.host) || "_";
  if (key !== activeKey || state.payloadStatus !== "up") return false;
  const statusKey = state.payloadStatusHost
    ? hostOf(state.payloadStatusHost) || "_"
    : activeKey;
  return statusKey === key;
}

/** A package install can ptrace/restart ShellUI and may replace the payload for
 * the DPI fallback. Sensor RPCs add pressure to exactly that recovery window,
 * so keep them off the console until the install task reaches a terminal state. */
function packageInstallIsActiveForHost(host: string): boolean {
  const key = hostOf(host) || "_";
  return useTaskStore.getState().tasks.some(
    (task) =>
      task.kind === "pkg-install" &&
      (hostOf(task.consoleId) || "_") === key &&
      !isTerminal(task.status),
  );
}

/**
 * Subscribe to sensor updates for a host. Idempotent — multiple
 * components on the same host share one poll loop. Must be paired
 * with `unsubscribeSensor(host)` on unmount.
 */
export function subscribeSensor(host: string): void {
  const key = hostOf(host) || host;
  const existing = handles.get(key);
  if (existing) {
    existing.refcount += 1;
    return;
  }
  const handle: PollHandle = {
    refcount: 1,
    timer: null,
    inFlight: false,
    cancelled: false,
  };
  handles.set(key, handle);
  startPoll(host, handle);
}

/**
 * Unsubscribe. When the last subscriber leaves, the poll loop stops.
 */
export function unsubscribeSensor(host: string): void {
  const key = hostOf(host) || host;
  const handle = handles.get(key);
  if (!handle) return;
  handle.refcount -= 1;
  if (handle.refcount <= 0) {
    handle.cancelled = true;
    if (handle.timer !== null && typeof window !== "undefined") {
      window.clearInterval(handle.timer);
    }
    handles.delete(key);
  }
}

function startPoll(host: string, handle: PollHandle): void {
  // SSR / Node test env guard — `window` is undefined outside browser.
  const win = typeof window !== "undefined" ? window : null;

  const tick = async () => {
    if (handle.cancelled) return;
    if (handle.inFlight) return;
    if (typeof document !== "undefined" && document.hidden) return;
    if (!payloadIsUpForHost(host)) return;
    if (transferScreenBusy(host) || packageInstallIsActiveForHost(host)) return;

    handle.inFlight = true;
    const addr = transferAddr(host.trim());
    try {
      const [t, p] = await Promise.all([
        fetchHwTemps(addr).catch(() => null),
        fetchHwPower(addr).catch(() => null),
      ]);
      if (handle.cancelled) return;
      if (t || p) {
        useSensorsStore.getState().record(host, { ts: Date.now(), temps: t, power: p });
      }
    } catch {
      // ignore — transient network/payload errors
    } finally {
      handle.inFlight = false;
    }
  };

  // Fire one immediate read, then start the interval (browser only).
  tick();
  if (win) {
    handle.timer = win.setInterval(tick, POLL_MS);
  }
}

// ── Test helpers (not for app use) ─────────────────────────────────

/** @internal Test-only: returns the current refcount for a host.
 *  Returns 0 if no active subscription. */
export function _refcountForTest(host: string): number {
  const key = hostOf(host) || host;
  return handles.get(key)?.refcount ?? 0;
}

/**
 * React hook that subscribes to a host's sensor stream for the
 * component's lifetime and returns the latest sample + history.
 *
 * The returned object is memoized on `sample`/`history` identity, so
 * it only changes when the underlying store data changes — not on
 * every parent re-render. Both `sample` and `history` are stable
 * references (null / EMPTY_SAMPLES) when there's no data.
 *
 * ```ts
 * const { sample, history } = useSensors(host);
 * ```
 */
export function useSensors(host: string | null | undefined): {
  sample: SensorSample | null;
  history: SensorSample[];
} {
  const hostKey = host ?? null;
  const sample = useLatestSample(hostKey);
  const history = useSampleHistory(hostKey);

  // Subscribe/unsubscribe in an effect. The store is the source of
  // truth for data; this effect only manages the poll lifecycle.
  const effectiveHost = hostKey ?? "";
  useEffect(() => {
    if (!effectiveHost.trim()) return;
    subscribeSensor(effectiveHost);
    return () => unsubscribeSensor(effectiveHost);
  }, [effectiveHost]);

  // Memoize so the returned object identity is stable across re-renders
  // that don't change the underlying data. Without this, every parent
  // re-render would produce a new `{ sample, history }` object and
  // trigger downstream effects / memo recalculations.
  return useMemo(() => ({ sample, history }), [sample, history]);
}
