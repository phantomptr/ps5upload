// Someone whose keyboard and mouse the PS5 has may not know the release key. These are the signs
// that they are trying to get out, so the stream can show the release key again (StreamPanel):
//   - Esc twice within a second;
//   - the ⌘ / Windows key (⌘Q, ⌘Tab, the Start menu), as far as the web view delivers it;
//   - the mouse pushed against an edge of the picture again and again (towards the menu bar or
//     the Dock). The pointer is hidden and held still while captured, so a virtual pointer
//     follows the movement from where the capture began, clamped to the picture: a push is
//     movement past an edge; three pushes at the same edge within a few seconds is an attempt.
//     One long turn in a game is a single push, and moving back and forth near an edge never
//     reaches it;
//   - a click where the stream bar is (the top of the picture).
// The detector only says "show the hint"; nothing is taken from the game.

export const ESC_TWICE_MS = 1000;
/** Movement past an edge that counts as a push, px. */
export const PUSH_PX = 200;
export const PUSHES = 3;
export const PUSH_WINDOW_MS = 4000;
/** A push ends when the movement stops this long (or turns away from the edge). */
export const PUSH_GAP_MS = 250;
/** The stream bar's strip at the top of the picture, px. */
export const BAR_STRIP_PX = 64;

type Edge = "left" | "right" | "top" | "bottom";

export type EscapeSign = "esc_twice" | "system_key" | "edge" | "bar_click";

export class EscapeAttemptDetector {
  private lastEsc = -Infinity;
  private x = 0;
  private y = 0;
  private w = 0;
  private h = 0;
  /** The push in progress: its edge, how far past it, and when it last moved. */
  private push: { edge: Edge; px: number; counted: boolean; t: number } | null = null;
  private pushes: Array<{ edge: Edge; t: number }> = [];

  constructor(private now: () => number = () => performance.now()) {}

  /** The capture began at (x, y) in a w × h picture. */
  start(x: number, y: number, w: number, h: number): void {
    this.w = Math.max(1, w);
    this.h = Math.max(1, h);
    this.x = Math.min(Math.max(0, x), this.w);
    this.y = Math.min(Math.max(0, y), this.h);
    this.push = null;
    this.pushes = [];
  }

  /** A key went down (KeyboardEvent.code). */
  key(code: string, down: boolean, repeat = false): EscapeSign | null {
    if (!down || repeat) return null;
    const t = this.now();
    if (code === "MetaLeft" || code === "MetaRight" || code === "OSLeft" || code === "OSRight") return "system_key";
    if (code !== "Escape") return null;
    if (t - this.lastEsc <= ESC_TWICE_MS) {
      this.lastEsc = -Infinity;
      return "esc_twice";
    }
    this.lastEsc = t;
    return null;
  }

  /** Mouse movement while captured. */
  move(dx: number, dy: number): EscapeSign | null {
    if (!this.w) return null;
    const t = this.now();
    if (this.push && t - this.push.t > PUSH_GAP_MS) this.push = null;
    const nx = this.x + dx;
    const ny = this.y + dy;
    // Past which edge, and how far (the larger overshoot when a corner).
    const over: Array<[Edge, number]> = [
      ["left", -nx],
      ["right", nx - this.w],
      ["top", -ny],
      ["bottom", ny - this.h],
    ];
    const [edge, px] = over.reduce((a, b) => (b[1] > a[1] ? b : a));
    this.x = Math.min(Math.max(0, nx), this.w);
    this.y = Math.min(Math.max(0, ny), this.h);
    if (px <= 0) {
      // Moving away from the edge of the push in progress ends it.
      const p = this.push;
      if (p) {
        const away =
          (p.edge === "left" && dx > 0) ||
          (p.edge === "right" && dx < 0) ||
          (p.edge === "top" && dy > 0) ||
          (p.edge === "bottom" && dy < 0);
        if (away) this.push = null;
      }
      return null;
    }
    if (!this.push || this.push.edge !== edge) this.push = { edge, px: 0, counted: false, t };
    this.push.px += px;
    this.push.t = t;
    if (this.push.counted || this.push.px < PUSH_PX) return null;
    this.push.counted = true;
    this.pushes = this.pushes.filter((p) => t - p.t <= PUSH_WINDOW_MS && p.edge === edge);
    this.pushes.push({ edge, t });
    if (this.pushes.length < PUSHES) return null;
    this.pushes = [];
    return "edge";
  }

  /** A mouse button went down while captured. */
  click(): EscapeSign | null {
    return this.w && this.y <= BAR_STRIP_PX ? "bar_click" : null;
  }
}
