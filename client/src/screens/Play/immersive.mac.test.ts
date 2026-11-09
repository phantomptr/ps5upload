import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn(async (..._a: unknown[]): Promise<unknown> => "webview");
vi.mock("../../lib/invokeLogged", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock("../../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../../lib/platform", () => ({
  isAndroid: () => false,
  isIOS: () => false,
  isMobile: () => false,
  isMacOrLinuxDesktop: () => true,
}));

import { focusForControllers, focusTakenFrom } from "./immersive";

describe("the Mac app gives the web view the focus for controllers (#720)", () => {
  beforeEach(() => invoke.mockClear());

  it("asks the app, and says what had the focus before", async () => {
    invoke.mockResolvedValueOnce("TaoView");
    expect(await focusForControllers()).toBe("TaoView");
    expect(invoke.mock.calls).toEqual([["focus_webview"]]);
  });

  it("an app that does not answer is not an error", async () => {
    invoke.mockRejectedValueOnce(new Error("the window did not answer"));
    expect(await focusForControllers()).toBeNull();
  });

  it("the log names who had the focus only when the page did not", () => {
    expect(focusTakenFrom("webview")).toBeNull();
    expect(focusTakenFrom("")).toBeNull();
    expect(focusTakenFrom(null)).toBeNull();
    expect(focusTakenFrom("none")).toBe("none");
    expect(focusTakenFrom("TaoView")).toBe("TaoView");
  });
});
