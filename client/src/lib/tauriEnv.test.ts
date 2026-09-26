import { describe, expect, it, vi } from "vitest";

import { safeUnlisten } from "./tauriEnv";

/**
 * `safeUnlisten` swallows both synchronous throws and asynchronous
 * rejections from a Tauri unlisten function. These cases mirror what
 * Tauri 2's `_unlisten` does in the wild — synchronous TypeError
 * when the listener table is already gone (HMR reload), async
 * rejection of the underlying `invoke('plugin:event|unlisten', ...)`.
 *
 * Without this helper, the renderer ended up with unhandled rejections
 * surfacing as user-visible errors in the console (and on the
 * RootErrorBoundary in extreme cases). The bug appeared specifically
 * in the InstallPackage drag-drop cleanup; the same shape is in Upload.
 */
describe("safeUnlisten", () => {
  it("invokes the function once on the happy path", () => {
    const fn = vi.fn(() => undefined);
    safeUnlisten(fn);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("swallows synchronous throws", () => {
    const fn = vi.fn(() => {
      throw new TypeError(
        "undefined is not an object (evaluating 'listeners[eventId].handlerId')",
      );
    });
    expect(() => safeUnlisten(fn)).not.toThrow();
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("swallows asynchronous rejections via the returned Promise", async () => {
    // Capture any unhandled-rejection that would have surfaced if our
    // helper didn't attach a .catch. Vitest fails tests with
    // unhandled rejections by default, so this also acts as the
    // assertion: if the test passes, no unhandled rejection escaped.
    const promise = Promise.reject(new Error("async unregister failed"));
    const fn = vi.fn(() => promise);
    safeUnlisten(fn);
    expect(fn).toHaveBeenCalledTimes(1);
    // Wait a microtask for the promise rejection to settle. If the
    // helper failed to attach .catch, this would surface as an
    // unhandled rejection (Vitest fails the test).
    await new Promise((r) => setTimeout(r, 0));
  });

  it("handles thenable-but-not-Promise return values", async () => {
    // Some Tauri builds wrap responses in a custom thenable. As long
    // as it has a .catch method, we attach to it. (If it doesn't
    // have .catch, we treat the return value as non-promise and skip.)
    let caughtHandler: ((e: unknown) => void) | null = null;
    const thenable = {
      catch(handler: (e: unknown) => void) {
        caughtHandler = handler;
        return this;
      },
    };
    safeUnlisten(() => thenable);
    expect(caughtHandler).not.toBeNull();
    // Simulate the rejection — handler should run without throwing.
    expect(() => caughtHandler!(new Error("oops"))).not.toThrow();
  });

  it("returns silently when the function returns null/undefined/number", () => {
    expect(() => safeUnlisten(() => null)).not.toThrow();
    expect(() => safeUnlisten(() => undefined)).not.toThrow();
    expect(() => safeUnlisten(() => 42)).not.toThrow();
  });
});
