// A touch screen as a laptop's touchpad, for the PS5's mouse (native keyboard & mouse on a phone
// or tablet with nothing attached, and the phone link page's Touchpad & keyboard tab):
//   one finger drag        moves the pointer (relative, times the speed setting)
//   tap                    left click
//   two-finger tap         right click
//   two-finger drag        scrolls
//   tap, then touch & drag holds the left button (drag, select); tap twice is a double click
// Pure: fed pointer events with times, it calls the sink.

export interface TouchpadSink {
  move(dx: number, dy: number): void;
  /** MouseEvent.button numbering: 0 left, 2 right. */
  button(button: number, down: boolean): void;
  /** Pixels, as WheelEvent deltaY / deltaX (down and right positive). */
  scroll(dy: number, dx: number): void;
}

/** A tap: down and up within this long, moving less than TAP_SLOP. */
export const TAP_MS = 250;
export const TAP_SLOP = 10;
/** A second touch this soon after a tap (and near it) holds the button. */
export const DOUBLE_MS = 300;
export const DOUBLE_SLOP = 40;
/** Two fingers count as a two-finger tap when the second lands this soon after the first. */
export const TWO_FINGER_MS = 150;

interface Finger {
  x: number;
  y: number;
  x0: number;
  y0: number;
  t0: number;
}

export class TouchpadGestures {
  private fingers = new Map<number, Finger>();
  /** Most fingers down at once in this gesture. */
  private most = 0;
  /** The gesture moved past a tap. */
  private moved = false;
  private gestureT0 = 0;
  private lastTap: { t: number; x: number; y: number } | null = null;
  /** The left button is held by tap-and-drag. */
  private dragging = false;

  constructor(
    private sink: TouchpadSink,
    private speed: () => number = () => 1,
  ) {}

  down(id: number, x: number, y: number, t: number): void {
    if (!this.fingers.size) {
      this.most = 0;
      this.moved = false;
      this.gestureT0 = t;
      // A touch right after a tap, near it: the left button is held until it lifts.
      const tap = this.lastTap;
      if (tap && t - tap.t <= DOUBLE_MS && Math.hypot(x - tap.x, y - tap.y) <= DOUBLE_SLOP) {
        this.dragging = true;
        this.sink.button(0, true);
      }
      this.lastTap = null;
    }
    this.fingers.set(id, { x, y, x0: x, y0: y, t0: t });
    this.most = Math.max(this.most, this.fingers.size);
  }

  move(id: number, x: number, y: number): void {
    const f = this.fingers.get(id);
    if (!f) return;
    const dx = x - f.x;
    const dy = y - f.y;
    f.x = x;
    f.y = y;
    if (!this.moved && Math.hypot(x - f.x0, y - f.y0) > TAP_SLOP) this.moved = true;
    if (!this.moved) return;
    if (this.fingers.size >= 2) {
      // Two fingers scroll, by their average movement (each finger reports half of it); the
      // content follows the fingers, as on a touchpad, so the wheel turns the other way.
      if (this.dragging) return;
      const n = this.fingers.size;
      this.sink.scroll(-dy / n, -dx / n);
      return;
    }
    if (this.most >= 2) return; // the fingers of a two-finger gesture lifting one by one
    const k = this.speed();
    this.sink.move(dx * k, dy * k);
  }

  up(id: number, t: number): void {
    const f = this.fingers.get(id);
    if (!f) return;
    this.fingers.delete(id);
    if (this.fingers.size) return;
    if (this.dragging) {
      this.dragging = false;
      this.sink.button(0, false);
      return;
    }
    const quick = t - this.gestureT0 <= TAP_MS + (this.most >= 2 ? TWO_FINGER_MS : 0);
    if (this.moved || !quick) return;
    const b = this.most >= 2 ? 2 : 0;
    this.sink.button(b, true);
    this.sink.button(b, false);
    if (b === 0) this.lastTap = { t, x: f.x, y: f.y };
  }

  /** The touch was taken away (pointercancel): let go of everything. */
  cancel(): void {
    this.fingers.clear();
    this.lastTap = null;
    if (this.dragging) {
      this.dragging = false;
      this.sink.button(0, false);
    }
  }
}
