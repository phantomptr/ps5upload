// Canonical stale-host guard.
//
// Before 2.12.0 the same pattern was hand-rolled in ~10 screen-level
// async callbacks (Library, Hardware ×3, Volumes, Saves, Screenshots,
// FileSystem, etc.). The pattern: capture `host` at the top of an
// async callback, build an `isStale()` checker, drop the result if
// the user switched PS5 mid-flight.
//
// The 2.9.0 audit found 7 of these sites simultaneously had a P0/P1
// host-stale-clobber bug — each one was a hand-coded guard, each
// one had to be patched independently, and each was a place future
// regressions could land if the convention drifted. This hook is
// the canonical version; future "I need to call something slow on
// the PS5 from a screen" code should use it instead of re-coding the
// closure dance.
//
// Usage:
//
//   const guard = useStaleHostGuard();
//   const refresh = useCallback(async () => {
//     const probe = guard.capture();
//     // ... slow PS5 RPC ...
//     if (probe.isStale()) return;
//     setData(result);
//   }, [guard]);
//
// The captured `probe.host` is the value to use for building addrs
// inside the async work — DON'T re-read `useConnectionStore.host`
// after the await, because that defeats the whole point.

import { useCallback, useMemo } from "react";

import { useConnectionStore } from "../state/connection";

/** A single in-flight async-op's captured-then-validated host
 *  context. Hold on to it across the await; call `.isStale()`
 *  before any state mutation that could be wrong if the user has
 *  switched PS5. */
export interface HostProbe {
  /** The host as it was when capture() was called. Always use THIS
   *  (not `useConnectionStore.host`) to build addrs inside the
   *  async work — otherwise a host-switch mid-flight will silently
   *  target the new console with state derived for the old. */
  host: string;
  /** Returns true if the live `connection.host` has changed since
   *  this probe was captured. Cheap to call — reads the zustand
   *  store snapshot once. Idempotent. */
  isStale: () => boolean;
}

/** Build a fresh `HostProbe` capturing the current `connection.host`.
 *  Stable function AND OBJECT reference across renders so callers can
 *  include the guard in `useCallback`/`useEffect` deps without
 *  thrashing. The capture fn is wrapped in useCallback, AND the
 *  returned `{ capture }` object is wrapped in useMemo — without the
 *  latter, the bare object-literal would be a fresh reference every
 *  render and any consumer dependency on `guard` would re-fire
 *  every render (e.g. Hardware screen's setInterval, found by the
 *  2.12.0 crash audit). */
export function useStaleHostGuard(): { capture: () => HostProbe } {
  const capture = useCallback((): HostProbe => {
    // Read host fresh at capture-time, not via the React subscription —
    // the host the hook saw at React-render-time may be older than the
    // one in the store right now. We want the most-recent-known value
    // at the moment work starts.
    const probedHost = useConnectionStore.getState().host;
    return {
      host: probedHost,
      isStale: () => useConnectionStore.getState().host !== probedHost,
    };
  }, []);
  return useMemo(() => ({ capture }), [capture]);
}

/** Non-hook variant for use OUTSIDE React (engine bridges, queue
 *  workers, etc.). Same contract as the hook. Use the hook in
 *  components; this in modules. */
export function captureHostProbe(): HostProbe {
  const probedHost = useConnectionStore.getState().host;
  return {
    host: probedHost,
    isStale: () => useConnectionStore.getState().host !== probedHost,
  };
}
