// A long press with a finger: hold still on one spot for a moment. Files starts selecting with it
// on a phone (#936), as a phone's own file managers do.
//
// The browser can report the same press as a `contextmenu` event too (Android does, about when the
// hold is long enough), so either one ends the press, once. The finger lifting is not a tap: the
// click right after a long press is swallowed wherever it lands, or it would open the folder under
// it, or press a button of the bar that selecting just put under the finger.

/** How long a finger holds still before it counts (Android's own long press is 400-500 ms). */
export const LONG_PRESS_MS = 500;
/** How far a finger may drift and still be holding still. More is a scroll. */
export const LONG_PRESS_SLOP_PX = 10;
/** How long after the finger lifts its click may still arrive. */
const CLICK_AFTER_MS = 600;

export interface LongPress<T> {
  /** A pointer went down on `target`. Only a finger (touch) starts a press. */
  down(target: T, x: number, y: number, pointerType: string): void;
  move(x: number, y: number): void;
  /** The finger lifted, or the browser took the touch over (a scroll). */
  end(): void;
  /** The browser's own long press (`contextmenu`) for the press under way. True when it was
   *  this press's, and its menu should not open. */
  contextMenu(): boolean;
  /** Call for each click: true for the one the finger lifting after a long press makes. */
  takeClick(): boolean;
  /** Stop any timer (unmount). */
  dispose(): void;
}

type Clock = {
  set: (fn: () => void, ms: number) => unknown;
  clear: (id: unknown) => void;
  now: () => number;
};

const realClock: Clock = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
  now: () => Date.now(),
};

export function createLongPress<T>(
  onLongPress: (target: T) => void,
  options: { ms?: number; slop?: number; clock?: Clock } = {},
): LongPress<T> {
  const ms = options.ms ?? LONG_PRESS_MS;
  const slop = options.slop ?? LONG_PRESS_SLOP_PX;
  const clock = options.clock ?? realClock;
  let press: { target: T; x: number; y: number; fired: boolean } | null = null;
  let timer: unknown = null;
  /** After a long press: until when its click may come (while the finger is still down: always). */
  let swallowUntil: number | null = null;

  const stopTimer = () => {
    if (timer !== null) clock.clear(timer);
    timer = null;
  };
  const fire = () => {
    stopTimer();
    if (!press || press.fired) return;
    press.fired = true;
    swallowUntil = Infinity;
    onLongPress(press.target);
  };
  const swallowing = () => swallowUntil !== null && clock.now() <= swallowUntil;

  return {
    down(target, x, y, pointerType) {
      stopTimer();
      swallowUntil = null;
      press = null;
      if (pointerType !== "touch") return;
      press = { target, x, y, fired: false };
      timer = clock.set(fire, ms);
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
      if (press?.fired) swallowUntil = clock.now() + CLICK_AFTER_MS;
      press = null;
    },
    contextMenu() {
      // Already a long press (the timer won): its menu stays shut, even just after the lift.
      if (swallowing()) return true;
      if (!press) return false;
      fire();
      return true;
    },
    takeClick() {
      const swallow = swallowing();
      swallowUntil = null;
      return swallow;
    },
    dispose() {
      stopTimer();
      press = null;
    },
  };
}
