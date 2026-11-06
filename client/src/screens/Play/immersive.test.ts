import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn(async (..._a: unknown[]) => undefined);
vi.mock("../../lib/invokeLogged", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock("../../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../../lib/platform", () => ({ isAndroid: () => true, isIOS: () => false, isMobile: () => true }));

import {
  lockLandscape,
  resetAndroidUiForTest,
  setWindowFullscreen,
  unlockOrientation,
} from "./immersive";

const calls = () => invoke.mock.calls.map((c) => c[1]);

describe("Remote Play full screen in the Android app", () => {
  beforeEach(() => {
    invoke.mockClear();
    invoke.mockImplementation(async () => undefined);
    resetAndroidUiForTest();
  });

  it("hides the system bars, locks landscape, and gives both back on leaving", async () => {
    await setWindowFullscreen(true);
    lockLandscape();
    await setWindowFullscreen(false);
    expect(invoke.mock.calls.map((c) => c[0])).toEqual([
      "android_system_ui",
      "android_system_ui",
      "android_system_ui",
    ]);
    expect(calls()).toEqual([
      { immersive: true, landscape: false },
      { immersive: true, landscape: true },
      { immersive: false, landscape: false },
    ]);
  });

  it("asks the native side once for repeated requests (a touch on the picture asks again)", async () => {
    await setWindowFullscreen(true);
    await setWindowFullscreen(true);
    await setWindowFullscreen(true);
    expect(calls()).toEqual([{ immersive: true, landscape: false }]);
  });

  it("unlocking the orientation keeps the bars hidden", async () => {
    await setWindowFullscreen(true);
    lockLandscape();
    unlockOrientation();
    await Promise.resolve();
    expect(calls()[calls().length - 1]).toEqual({ immersive: true, landscape: false });
  });

  it("asks again after a failed call", async () => {
    invoke.mockImplementationOnce(async () => {
      throw new Error("no window");
    });
    await setWindowFullscreen(true);
    await setWindowFullscreen(true);
    expect(calls()).toEqual([
      { immersive: true, landscape: false },
      { immersive: true, landscape: false },
    ]);
  });
});
