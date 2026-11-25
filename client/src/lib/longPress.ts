// A long press with a finger: hold still on one spot for a moment. Files starts selecting with it
// on a phone (#936), as a phone's own file managers do.
//
// The browser can report the same press as a `contextmenu` event too (Android does, about when the
// hold is long enough), so either one ends the press, once. The tap that lifts the finger is not
// a tap: the click after a long press is swallowed, or it would also open the folder under it.

/** How long a finger holds still before it counts (Android's own long press is 400-500 ms). */
export const LONG_PRESS_MS = 500;
/** How far a finger may drift and still be holding still. More is a scroll. */
export const LONG_PRESS_SLOP_PX = 10;

export interface LongPress<T> {
  /** A pointer went down on `target`. Only a finger (touch) starts a press. */
  down(target: T, x: number, y: number, pointerType: string): void;
  move(x: number, y: number): void;
  /** The finger lifted, or the browser took the touch over (a scroll). */
  end(): void;
  /** The browser's own long press (`contextmenu`) for the press under way. True when it was
   *  this press's, and its menu should not open. */
  contextMenu(): boolean;
  /** Call for each click: true for the one right after a long press, to swallow. */
  takeClick(): boolean;
  /** Stop any timer (unmount). */
  dispose(): void;
}

type Timers = {
  set: (fn: () => void, ms: number) => unknown;
  clear: (id: unknown) => void;
};

const realTimers: Timers = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
};

export function createLongPress<T>(
  onLongPress: (target: T) => void,
  options: { ms?: number; slop?: number; timers?: Timers } = {},
): LongPress<T> {
  const ms = options.ms ?? LONG_PRESS_MS;
  const slop = options.slop ?? LONG_PRESS_SLOP_PX;
  const timers = options.timers ?? realTimers;
  let press: { target: T; x: number; y: number; fired: boolean } | null = null;
  let timer: unknown = null;
  let swallowClick = false;

  const stopTimer = () => {
    if (timer !== null) timers.clear(timer);
    timer = null;
  };
  const fire = () => {
    stopTimer();
    if (!press || press.fired) return;
    press.fired = true;
    swallowClick = true;
    onLongPress(press.target);
  };

  return {
    down(target, x, y, pointerType) {
      stopTimer();
      swallowClick = false;
      press = null;
      if (pointerType !== "touch") return;
      press = { target, x, y, fired: false };
      timer = timers.set(fire, ms);
    },
    move(x, y) {
      if (!press || press.fired) return;
      if (Math.hypot(x - press.x, y - press.y) > slop) {
        stopTimer();
        press = null;
      }
    },
    end() {
      stopTimer();
      press = null;
    },
    contextMenu() {
      // Already a long press (the timer won): its menu stays shut even after the finger lifted.
      if (swallowClick) return true;
      if (!press) return false;
      fire();
      return true;
    },
    takeClick() {
      const swallow = swallowClick;
      swallowClick = false;
      return swallow;
    },
    dispose() {
      stopTimer();
      press = null;
    },
  };
}
