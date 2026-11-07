import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => true }));
const captured = vi.hoisted(() => ({ on: false }));
vi.mock("./pointerCapture", () => ({ isCaptured: () => captured.on }));

import { installAccidentalReloadGuard } from "./preventAccidentalReload";

// No DOM here: a document that keeps its keydown listeners.
const keydown: Array<(e: unknown) => void> = [];
vi.stubGlobal("document", {
  addEventListener: (type: string, fn: (e: unknown) => void) => {
    if (type === "keydown") keydown.push(fn);
  },
});
// The guard is production only.
vi.stubEnv("DEV", false);
installAccidentalReloadGuard();

function press(init: { key: string; ctrlKey?: boolean }) {
  const e = { ctrlKey: false, metaKey: false, ...init, preventDefault: vi.fn(), stopPropagation: vi.fn() };
  for (const fn of keydown) fn(e);
  return { prevented: e.preventDefault.mock.calls.length > 0, stopped: e.stopPropagation.mock.calls.length > 0 };
}

describe("the reload guard", () => {
  afterEach(() => {
    captured.on = false;
  });
  it("blocks Ctrl+R and keeps it from the page", () => {
    expect(press({ key: "r", ctrlKey: true })).toEqual({ prevented: true, stopped: true });
  });
  it("blocks the reload but lets a captured stream have Ctrl+R and F5 (a PC game's bindings)", () => {
    captured.on = true;
    expect(press({ key: "r", ctrlKey: true })).toEqual({ prevented: true, stopped: false });
    expect(press({ key: "F5" })).toEqual({ prevented: true, stopped: false });
  });
});
