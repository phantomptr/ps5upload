import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn(async (..._a: unknown[]) => undefined);
vi.mock("../../lib/invokeLogged", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock("../../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../../lib/platform", () => ({ isAndroid: () => false, isIOS: () => true, isMobile: () => true }));

import { canDocFullscreen, setWindowFullscreen } from "./immersive";

describe("Remote Play full screen in the iOS app", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("never asks WebKit for full screen: its close button covers L2, and the keyboard ends it", async () => {
    const requestFullscreen = vi.fn(async () => undefined);
    vi.stubGlobal("document", { documentElement: { requestFullscreen } });
    expect(canDocFullscreen()).toBe(false);
    await setWindowFullscreen(true);
    await setWindowFullscreen(false);
    expect(requestFullscreen).not.toHaveBeenCalled();
    // Only the status bar (the clock) is hidden and given back, natively (#722).
    expect(invoke.mock.calls).toEqual([
      ["set_status_bar_hidden", { hidden: true }],
      ["set_status_bar_hidden", { hidden: false }],
    ]);
  });
});
