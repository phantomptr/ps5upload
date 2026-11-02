import { useEffect, useRef } from "react";

/** How often a cheat list with a cheat waiting for the game (#627) is read again. */
export const WAITING_POLL_MS = 4000;

/**
 * Read the cheat list again every `ms` until stopped, handing each read to `onData`. A read
 * that fails is skipped quietly; none starts while the previous one is still out or while
 * `busy()` (a switch being flipped: its answer must not be overwritten by an older read).
 * Returns the stop function.
 */
export function startWaitingPoll<T>(
  read: () => Promise<T>,
  onData: (data: T) => void,
  busy: () => boolean = () => false,
  ms: number = WAITING_POLL_MS,
): () => void {
  let stopped = false;
  let inFlight = false;
  const timer = setInterval(() => {
    if (stopped || inFlight || busy()) return;
    inFlight = true;
    read()
      .then((data) => {
        if (!stopped && !busy()) onData(data);
      })
      .catch(() => {})
      .finally(() => {
        inFlight = false;
      });
  }, ms);
  return () => {
    stopped = true;
    clearInterval(timer);
  };
}

/**
 * While `active` (some switched-on cheat is waiting for the game), read the list again quietly
 * until it applies. `restartKey` names what is read (title and console): a change starts over.
 */
export function useWaitingPoll<T>(
  active: boolean,
  restartKey: string,
  read: () => Promise<T>,
  onData: (data: T) => void,
  busy?: () => boolean,
): void {
  const fns = useRef({ read, onData, busy });
  useEffect(() => {
    fns.current = { read, onData, busy };
  });
  useEffect(() => {
    if (!active) return;
    return startWaitingPoll(
      () => fns.current.read(),
      (d) => fns.current.onData(d),
      () => fns.current.busy?.() ?? false,
    );
  }, [active, restartKey]);
}
