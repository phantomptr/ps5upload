import { useCallback, useEffect, useRef } from "react";

import { hostOf } from "./addr";
import { awakeConsoles } from "./awakeWork";
import { useTaskStore, type Task } from "../state/tasks";
import { useTransferStore, type TransferPhase } from "../state/transfer";
import { useUploadQueueStore } from "../state/uploadQueue";

/**
 * Polling that stays out of the way of transfers.
 *
 * Status pollers (power, storage, clock, notifications…) each send a small
 * management frame to the console. One is nothing; five of them every few
 * seconds while an upload runs measurably slow the upload down, and a poll
 * nobody can see (window hidden) is pure waste. So a poll:
 *   - never overlaps itself: the next one is scheduled only after the
 *     previous call settles;
 *   - skips while the document is hidden, and runs once as soon as it is
 *     visible again;
 *   - skips while an upload, install or copy is running against the same
 *     console (or any console, without a host), and runs once when it ends.
 */

export interface PollerOptions {
  /** Called on every due tick. A returned promise holds the next tick until
   *  it settles. Errors are the caller's to handle; a throw never stops the
   *  poller. */
  fn: () => unknown;
  ms: number;
  /** True while ticks should be skipped. Checked at every tick. */
  paused?: () => boolean;
  /** Run once right away on start (default true). */
  immediate?: boolean;
  setTimer?: (cb: () => void, ms: number) => unknown;
  clearTimer?: (t: unknown) => void;
}

export interface Poller {
  start: () => void;
  stop: () => void;
  /** Run now unless a call is in flight or the poller is stopped (or paused,
   *  unless `force`); resets the interval. Used when a pause ends, and with
   *  `force` for a user's explicit refresh. */
  kick: (force?: boolean) => void;
  /** Whether a call is in flight. */
  inFlight: () => boolean;
}

/** The framework-free core of `usePoll`, testable with fake timers. */
export function createPoller(opts: PollerOptions): Poller {
  const setT = opts.setTimer ?? ((cb, ms) => setTimeout(cb, ms));
  const clearT =
    opts.clearTimer ?? ((t) => clearTimeout(t as ReturnType<typeof setTimeout>));
  let timer: unknown = null;
  let running = false;
  let busy = false;

  const schedule = () => {
    if (!running) return;
    if (timer !== null) clearT(timer);
    timer = setT(tick, opts.ms);
  };

  const run = () => {
    busy = true;
    let result: unknown;
    try {
      result = opts.fn();
    } catch {
      result = undefined;
    }
    const done = () => {
      busy = false;
      schedule();
    };
    if (result && typeof (result as Promise<unknown>).then === "function") {
      (result as Promise<unknown>).then(done, done);
    } else {
      done();
    }
  };

  function tick() {
    timer = null;
    if (!running) return;
    if (busy) return; // the in-flight call reschedules when it settles
    if (opts.paused?.()) {
      schedule();
      return;
    }
    run();
  }

  return {
    start() {
      if (running) return;
      running = true;
      if (opts.immediate === false) schedule();
      else tick();
    },
    stop() {
      running = false;
      if (timer !== null) clearT(timer);
      timer = null;
    },
    kick(force = false) {
      if (!running || busy || (!force && opts.paused?.())) return;
      if (timer !== null) clearT(timer);
      timer = null;
      run();
    },
    inFlight: () => busy,
  };
}

/** Whether data is moving to or from `host` (any console when null): a live
 *  upload/install/copy task, a one-shot upload, or a running queue. */
export function transferBusy(
  host: string | null | undefined,
  tasks: readonly Task[],
  phasesByHost: Record<string, TransferPhase>,
  runningHosts: Record<string, boolean>,
): boolean {
  const want = host?.trim() ? hostOf(host) : null;
  const live = (p: TransferPhase) =>
    p.kind === "starting" || p.kind === "running";
  if (want === null) {
    if (awakeConsoles(tasks).size > 0) return true;
    for (const h in phasesByHost) if (live(phasesByHost[h])) return true;
    for (const h in runningHosts) if (runningHosts[h]) return true;
    return false;
  }
  if (awakeConsoles(tasks).has(want)) return true;
  const phase = phasesByHost[want];
  if (phase && live(phase)) return true;
  return !!runningHosts[want];
}

export interface UsePollOptions {
  /** The console the poll talks to; transfers to other consoles don't pause
   *  it. Omit to pause for a transfer to any console. */
  host?: string | null;
  /** False stops polling (e.g. no console connected). Default true. */
  enabled?: boolean;
  /** Keep polling during transfers (for something that must stay live, like
   *  the transfer's own progress). Default false. */
  duringTransfers?: boolean;
  /** Run once on mount (default true). */
  immediate?: boolean;
}

/**
 * Call `fn` every `ms` while the screen is mounted, with the pauses described
 * at the top of this file. `ms` null disables. Returns `refresh`, which runs
 * the poll now (unless one is already in flight).
 */
export function usePoll(
  fn: () => unknown,
  ms: number | null,
  { host, enabled = true, duringTransfers = false, immediate = true }: UsePollOptions = {},
): { refresh: () => void } {
  const fnRef = useRef(fn);
  useEffect(() => {
    fnRef.current = fn;
  });

  // Three narrow boolean selectors, so a progress tick re-renders nothing.
  const busy = useTaskStore((s) =>
    duringTransfers ? false : transferBusy(host, s.tasks, {}, {}),
  );
  const phaseBusy = useTransferStore((s) =>
    duringTransfers ? false : transferBusy(host, [], s.phasesByHost, {}),
  );
  const queueBusy = useUploadQueueStore((s) =>
    duringTransfers ? false : transferBusy(host, [], {}, s.runningHosts),
  );
  const paused = busy || phaseBusy || queueBusy;
  const pausedRef = useRef(paused);
  useEffect(() => {
    pausedRef.current = paused;
  });

  const pollerRef = useRef<Poller | null>(null);
  const active = enabled && ms !== null && ms > 0;

  useEffect(() => {
    if (!active) return;
    const poller = createPoller({
      fn: () => fnRef.current(),
      ms: ms as number,
      immediate,
      paused: () =>
        pausedRef.current ||
        (typeof document !== "undefined" && document.visibilityState === "hidden"),
    });
    pollerRef.current = poller;
    poller.start();
    const onVisible = () => {
      if (document.visibilityState === "visible") poller.kick();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      poller.stop();
      if (pollerRef.current === poller) pollerRef.current = null;
    };
    // host is part of the key: a new console starts a fresh poll.
  }, [active, ms, immediate, host]);

  // A transfer just ended: catch up now rather than at the next interval.
  useEffect(() => {
    if (!paused) pollerRef.current?.kick();
  }, [paused]);

  const refresh = useCallback(() => {
    const p = pollerRef.current;
    if (p) p.kick(true);
    else void fnRef.current();
  }, []);
  return { refresh };
}
