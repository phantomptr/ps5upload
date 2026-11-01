import { describe, expect, it } from "vitest";

import {
  HID_USAGE,
  KbmSender,
  KIND_KBM,
  MOVE_BATCH_MS,
  devicesFor,
  effectiveMode,
  encodeReport,
  isCycleHotkey,
  KBM_BLOCK_TEXT,
  kbmBlocked,
  kbmButtonView,
  kbmStarting,
  modeUnavailable,
  nextMode,
  readKbmStatus,
  routeFor,
  wheelNotches,
  type KbmMode,
} from "./kbmInput";

const BOTH = { mouse: true, keyboard: true };
const NONE = { mouse: false, keyboard: false };
const PLAY = { owner: true, appFirst: false, pointerLocked: true };

const hex = (b: Uint8Array) => [...b].map((x) => x.toString(16).padStart(2, "0")).join("");

describe("reports", () => {
  it("are the helper's 16 bytes after kind 8", () => {
    // the same report ps5upload-core's vinput tests pin: 1122d4feff7fff0204e1000000000000
    const b = encodeReport({ buttons: 0x11, mods: 0x22, keys: [0x04, 0xe1] }, -300, 32767, -1, 2);
    expect(b[0]).toBe(KIND_KBM);
    expect(hex(b.subarray(1))).toBe("1122d4feff7fff0204e1000000000000");
    expect(b.length).toBe(17);
  });

  it("clamp motion and keep at most eight keys", () => {
    const b = encodeReport({ buttons: 0, mods: 0, keys: [4, 5, 6, 7, 8, 9, 10, 11, 12] }, 99999, -99999, 500, -500);
    const v = new DataView(b.buffer);
    expect(v.getInt16(3, true)).toBe(32767);
    expect(v.getInt16(5, true)).toBe(-32768);
    expect(v.getInt8(7)).toBe(127);
    expect(v.getInt8(8)).toBe(-128);
    expect([...b.subarray(9)]).toEqual([4, 5, 6, 7, 8, 9, 10, 11]);
  });

  it("use the key's place, not its letter", () => {
    expect(HID_USAGE.KeyA).toBe(0x04);
    expect(HID_USAGE.KeyZ).toBe(0x1d);
    expect(HID_USAGE.Digit1).toBe(0x1e);
    expect(HID_USAGE.Digit0).toBe(0x27);
    expect(HID_USAGE.Escape).toBe(0x29);
    expect(HID_USAGE.F1).toBe(0x3a);
    expect(HID_USAGE.F12).toBe(0x45);
    expect(HID_USAGE.ArrowUp).toBe(0x52);
    expect(HID_USAGE.Numpad0).toBe(0x62);
  });

  it("turn wheel deltas into notches, up positive, keeping the rest", () => {
    expect(wheelNotches(0, 100, 0)).toEqual({ notches: -1, carry: 0 });
    expect(wheelNotches(0, -250, 0).notches).toBe(2);
    const a = wheelNotches(0, 60, 0);
    expect(a.notches === 0 || Object.is(a.notches, -0)).toBe(true);
    expect(wheelNotches(a.carry, 60, 0).notches).toBe(-1);
    expect(wheelNotches(0, 3, 1).notches).toBe(-1);
    expect(wheelNotches(0, 1, 2).notches).toBe(-1);
  });
});

describe("routing", () => {
  it("sends each input to exactly one sink", () => {
    expect(routeFor("controller", PLAY)).toEqual({ keys: "pad", mouse: "pad" });
    expect(routeFor("native", PLAY)).toEqual({ keys: "native", mouse: "native" });
    expect(routeFor("mixed", PLAY)).toEqual({ keys: "pad", mouse: "native" });
  });

  it("sends the mouse to the PS5 only while it is locked to the picture", () => {
    const free = { ...PLAY, pointerLocked: false };
    expect(routeFor("native", free)).toEqual({ keys: "native", mouse: "none" });
    expect(routeFor("mixed", free)).toEqual({ keys: "pad", mouse: "none" });
    // mouse aim reads the lock itself, as before
    expect(routeFor("controller", free).mouse).toBe("pad");
  });

  it("sends the mouse over the picture where nothing can capture it", () => {
    const hover = { ...PLAY, pointerLocked: false, hoverFallback: true };
    expect(routeFor("native", hover).mouse).toBe("native");
    expect(routeFor("mixed", hover).mouse).toBe("native");
    expect(routeFor("native", { ...hover, appFirst: true }).mouse).toBe("none");
  });

  it("gives nothing to the game while the app comes first, or on a page that does not own the session", () => {
    for (const m of ["controller", "native", "mixed"] as KbmMode[]) {
      expect(routeFor(m, { ...PLAY, appFirst: true })).toEqual({ keys: "none", mouse: "none" });
      expect(routeFor(m, { ...PLAY, owner: false })).toEqual({ keys: "none", mouse: "none" });
    }
  });

  it("falls back to Controller while the console cannot take the remembered mode", () => {
    expect(effectiveMode("native", null)).toBe("controller");
    expect(effectiveMode("native", NONE)).toBe("controller");
    expect(effectiveMode("native", { mouse: true, keyboard: false })).toBe("controller");
    expect(effectiveMode("native", BOTH)).toBe("native");
    expect(effectiveMode("mixed", { mouse: true, keyboard: false })).toBe("mixed");
    expect(effectiveMode("mixed", NONE)).toBe("controller");
    expect(effectiveMode("controller", BOTH)).toBe("controller");
  });

  it("says why a mode cannot be picked", () => {
    expect(modeUnavailable("controller", null)).toBeNull();
    expect(modeUnavailable("native", NONE)).toBe("no_mouse");
    expect(modeUnavailable("native", { mouse: true, keyboard: false })).toBe("no_keyboard");
    expect(modeUnavailable("mixed", { mouse: true, keyboard: false })).toBeNull();
  });

  it("asks for the devices a mode uses, and the hotkey skips what the console cannot take", () => {
    expect(devicesFor("controller")).toEqual(NONE);
    expect(devicesFor("native")).toEqual(BOTH);
    expect(devicesFor("mixed")).toEqual({ mouse: true, keyboard: false });
    expect(nextMode("controller", BOTH)).toBe("native");
    expect(nextMode("native", BOTH)).toBe("mixed");
    expect(nextMode("mixed", BOTH)).toBe("controller");
    expect(nextMode("controller", NONE)).toBe("controller");
    expect(nextMode("controller", { mouse: true, keyboard: false })).toBe("mixed");
  });

  it("reserves Ctrl+Alt+M, and never the command palette's Ctrl+K", () => {
    const k = { code: "KeyM", ctrlKey: true, altKey: true, metaKey: false, shiftKey: false };
    expect(isCycleHotkey(k)).toBe(true);
    expect(isCycleHotkey({ ...k, altKey: false })).toBe(false);
    expect(isCycleHotkey({ ...k, code: "KeyK" })).toBe(false);
  });
});

/** A sender over fake timers; what it sent, as hex reports or control objects. */
function fakeSender() {
  const sent: Array<string | object> = [];
  let timers: Array<{ id: number; fn: () => void; ms: number; repeat: boolean }> = [];
  let n = 1;
  const s = new KbmSender({
    send: (m) => {
      sent.push(m instanceof Uint8Array ? hex(m.subarray(1)) : m);
      return true;
    },
    setTimer: (fn, ms) => (timers.push({ id: n, fn, ms, repeat: false }), n++),
    clearTimer: (id) => (timers = timers.filter((t) => t.id !== id)),
    setRepeat: (fn, ms) => (timers.push({ id: n, fn, ms, repeat: true }), n++),
    clearRepeat: (id) => (timers = timers.filter((t) => t.id !== id)),
  });
  const runOnce = () => {
    const due = timers.filter((t) => !t.repeat);
    timers = timers.filter((t) => t.repeat);
    due.forEach((t) => t.fn());
  };
  const beat = () => timers.filter((t) => t.repeat).forEach((t) => t.fn());
  return { s, sent, runOnce, beat, timers: () => timers };
}

const rep = (buttons: number, mods: number, dx: number, dy: number, keys: number[] = []) =>
  hex(encodeReport({ buttons, mods, keys }, dx, dy).subarray(1));

describe("KbmSender", () => {
  it("sends keys and buttons at once, and batches motion", () => {
    const f = fakeSender();
    f.s.key("KeyW", true);
    f.s.key("ShiftLeft", true);
    f.s.move(3, 4);
    f.s.move(2, -1);
    expect(f.timers()[0].ms).toBe(MOVE_BATCH_MS);
    expect(f.sent).toEqual([rep(0, 0, 0, 0, [0x1a]), rep(0, 0x02, 0, 0, [0x1a])]);
    f.runOnce();
    expect(f.sent[f.sent.length - 1]).toBe(rep(0, 0x02, 5, 3, [0x1a]));
    // a click carries the motion gathered so far, and its release follows
    f.s.move(1, 1);
    f.s.button(0, true);
    f.s.button(0, false);
    expect(f.sent.slice(-2)).toEqual([rep(1, 0x02, 1, 1, [0x1a]), rep(0, 0x02, 0, 0, [0x1a])]);
    // a key it has no usage for is not taken
    expect(f.s.key("Fn", true)).toBe(false);
    // repeats of a held key send nothing new
    const before = f.sent.length;
    f.s.key("KeyW", true);
    expect(f.sent.length).toBe(before);
  });

  it("splits motion too big for one report", () => {
    const f = fakeSender();
    f.s.move(40000, 0);
    f.s.flush();
    expect(f.sent).toEqual([rep(0, 0, 32767, 0), rep(0, 0, 7233, 0)]);
  });

  it("lets go of everything once, and only when something is held", () => {
    const f = fakeSender();
    f.s.releaseAll();
    expect(f.sent).toEqual([]);
    f.s.key("KeyA", true);
    f.s.button(2, true);
    f.s.move(9, 9); // not sent yet: dropped with the release
    f.s.releaseAll();
    expect(f.sent[f.sent.length - 1]).toBe(rep(0, 0, 0, 0));
    f.runOnce();
    expect(f.sent[f.sent.length - 1]).toBe(rep(0, 0, 0, 0));
    expect(f.s.holding).toBe(false);
  });

  it("asks for the devices, beats every 500 ms, and turns them off", () => {
    const f = fakeSender();
    f.s.setWant({ mouse: true, keyboard: false });
    expect(f.sent).toEqual([{ kbm: { on: true, mouse: true, keyboard: false } }]);
    expect(f.timers().some((t) => t.repeat && t.ms === 500)).toBe(true);
    f.beat();
    expect(f.sent.length).toBe(2);
    // the same wish again is not a new message
    f.s.setWant({ mouse: true, keyboard: false });
    expect(f.sent.length).toBe(2);
    f.s.key("KeyA", true);
    f.s.dispose();
    expect(f.sent.slice(-2)).toEqual([rep(0, 0, 0, 0), { kbm: { on: false } }]);
    expect(f.timers()).toEqual([]);
  });

  it("reads the engine's answer", () => {
    expect(readKbmStatus({ on: true, mouse: false, keyboard: false, err: "vinput_unsupported", reason: null })).toEqual({
      on: true,
      mouse: false,
      keyboard: false,
      err: "vinput_unsupported",
      reason: null,
    });
    expect(readKbmStatus("x")).toBeNull();
  });
});

describe("the stream bar's mode button", () => {
  const st = (on: boolean, err: string | null) => ({ on, mouse: false, keyboard: false, err, reason: null });

  it("is off only when no keyboard & mouse mode can be picked, with the console's reason", () => {
    expect(kbmBlocked({ ...BOTH, supported: true })).toBeNull();
    // Mixed alone (mouse, no keyboard) still leaves a choice.
    expect(kbmBlocked({ mouse: true, keyboard: false, supported: true })).toBeNull();
    expect(kbmBlocked(null)).toBe("unavailable");
    expect(kbmBlocked({ ...NONE, supported: false, reason: "helper_old" })).toBe("helper_old");
    expect(kbmBlocked({ ...NONE, supported: true, reason: "vinput_needs_loader" })).toBe("needs_loader");
    expect(kbmBlocked({ ...NONE, supported: true, reason: "vinput_companion_failed" })).toBe("companion_failed");
    expect(kbmBlocked({ ...NONE, supported: true, reason: "vinput_unsupported" })).toBe("unavailable");
    expect(kbmBlocked({ ...NONE, supported: true, reason: "unreachable" })).toBe("unavailable");
    // A reason does not turn off devices the console says it has.
    expect(kbmBlocked({ ...BOTH, supported: true, reason: "vinput_needs_loader" })).toBeNull();
  });

  it("has the i18n key and English for every reason", () => {
    expect(KBM_BLOCK_TEXT.needs_loader).toEqual({ key: "rp_kbm_needs_loader", en: "Needs a payload loader on port 9021" });
    expect(KBM_BLOCK_TEXT.companion_failed.key).toBe("rp_kbm_companion_failed");
    expect(KBM_BLOCK_TEXT.helper_old.key).toBe("rp_kbm_helper_old");
    expect(KBM_BLOCK_TEXT.unavailable.key).toBe("rp_kbm_unavailable");
  });

  it("spins while the console's helper starts", () => {
    const caps = { ...BOTH, supported: true, companion: "idle" };
    expect(kbmStarting(caps, null)).toBe(false);
    expect(kbmStarting(caps, st(true, "vinput_starting"))).toBe(true);
    // A starting error with the devices off is old news.
    expect(kbmStarting(caps, st(false, "vinput_starting"))).toBe(false);
    expect(kbmStarting(caps, st(true, "vinput_needs_loader"))).toBe(false);
    expect(kbmStarting({ ...caps, companion: "starting" }, null)).toBe(true);
    expect(kbmStarting({ ...caps, companion: "running" }, st(true, null))).toBe(false);
    expect(kbmStarting(null, null)).toBe(false);
  });

  it("shows the mode's icon, or the spinner, and why it is off", () => {
    const caps = { ...BOTH, supported: true, companion: "running" };
    for (const m of ["controller", "native", "mixed"] as KbmMode[])
      expect(kbmButtonView(m, caps, null)).toEqual({ mode: m, icon: m, blocked: null });
    expect(kbmButtonView("native", caps, st(true, "vinput_starting"))).toEqual({
      mode: "native",
      icon: "spinner",
      blocked: null,
    });
    expect(kbmButtonView("controller", { ...NONE, supported: true, reason: "vinput_needs_loader" }, null)).toEqual({
      mode: "controller",
      icon: "controller",
      blocked: "needs_loader",
    });
  });
});
