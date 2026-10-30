import { describe, expect, it } from "vitest";
import { IDLE_PAD, PadButton } from "./remotePlayStream";
import {
  DEFAULT_CONTROLS,
  R2_BIT,
  applyControls,
  macroAt,
  mouseMoved,
  newControlsState,
  readControls,
  type GameControls,
} from "./gameControls";

const pad = (buttons = 0) => ({ ...IDLE_PAD, buttons });
const ctl = (c: Partial<GameControls>): GameControls => ({ ...DEFAULT_CONTROLS, ...c });
const none = new Set<string>();

describe("per-game controls", () => {
  it("leaves the pad alone with no profile", () => {
    const st = newControlsState();
    expect(applyControls(pad(PadButton.CROSS), null, none, st, 0)).toEqual(pad(PadButton.CROSS));
  });

  it("repeats a held turbo button at its rate, and only that button", () => {
    const st = newControlsState();
    const c = ctl({ turbo: PadButton.CROSS, turboHz: 10 }); // 50 ms on, 50 ms off
    const held = PadButton.CROSS | PadButton.R1;
    expect(applyControls(pad(held), c, none, st, 10).buttons).toBe(held);
    expect(applyControls(pad(held), c, none, st, 60).buttons).toBe(PadButton.R1);
    expect(applyControls(pad(held), c, none, st, 110).buttons).toBe(held);
  });

  it("turns the mouse's movement into the right stick, fading when it stops", () => {
    const st = newControlsState();
    const c = ctl({ mouse: true, mouseSens: 1 });
    applyControls(pad(), c, none, st, 1000);
    mouseMoved(st.mouse, 20, -10);
    const p = applyControls(pad(), c, none, st, 1016);
    expect(p.rx).toBeGreaterThan(10000);
    expect(p.ry).toBeLessThan(-5000);
    // Inverted Y flips it.
    const inv = newControlsState();
    applyControls(pad(), ctl({ mouse: true, mouseInvertY: true }), none, inv, 1000);
    mouseMoved(inv.mouse, 0, -10);
    expect(applyControls(pad(), ctl({ mouse: true, mouseInvertY: true }), none, inv, 1016).ry).toBeGreaterThan(0);
    // Half a second later, still: back to the middle.
    expect(Math.abs(applyControls(pad(), c, none, st, 1516).rx)).toBeLessThan(200);
    // A click is a trigger.
    st.mouse.left = true;
    expect(applyControls(pad(), c, none, st, 1532).r2).toBe(255);
  });

  it("plays a macro on its key's press, step by step, and stops when the player takes over", () => {
    const c = ctl({
      macros: [{ key: "KeyQ", steps: [{ buttons: PadButton.SQUARE, ms: 100 }, { buttons: 0, ms: 50 }, { buttons: R2_BIT, ms: 100 }] }],
    });
    expect(macroAt(c.macros[0], 120)).toBe(0);
    expect(macroAt(c.macros[0], 400)).toBeNull();
    const st = newControlsState();
    const q = new Set(["KeyQ"]);
    expect(applyControls(pad(), c, q, st, 0).buttons).toBe(PadButton.SQUARE);
    // Holding the key does not start it again; it carries on by itself.
    expect(applyControls(pad(), c, q, st, 120).buttons).toBe(0);
    expect(applyControls(pad(), c, none, st, 200).r2).toBe(255);
    expect(applyControls(pad(), c, none, st, 400)).toEqual(pad());
    // Pressed again, then the player presses Square: the macro lets go.
    applyControls(pad(), c, q, st, 1000);
    expect(applyControls(pad(PadButton.SQUARE), c, q, st, 1010).buttons).toBe(PadButton.SQUARE);
    expect(st.macro).toBeNull();
  });

  it("reads a stored profile safely", () => {
    expect(readControls(null)).toEqual(DEFAULT_CONTROLS);
    const r = readControls({ turboHz: 999, mouseSens: -1, macros: [{ key: "KeyE", steps: [{ buttons: 1, ms: 1 }] }, { nope: 1 }] });
    expect(r.turboHz).toBe(30);
    expect(r.mouseSens).toBe(0.1);
    expect(r.macros).toEqual([{ key: "KeyE", steps: [{ buttons: 1, ms: 16 }] }]);
  });
});
