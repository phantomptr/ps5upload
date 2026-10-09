import { describe, expect, it, vi } from "vitest";

import { importWithReload, isChunkLoadError } from "./lazyWithReload";

function deps(start = 1_000_000) {
  let now = start;
  const store = new Map<string, string>();
  return {
    now: () => now,
    advance: (ms: number) => void (now += ms),
    get: (k: string) => store.get(k) ?? null,
    set: (k: string, v: string) => void store.set(k, v),
    reload: vi.fn(),
    note: vi.fn(),
  };
}

const chunkError = () => new TypeError("Importing a module script failed.");

describe("recognising a screen whose code could not be loaded", () => {
  it("knows the browsers' wordings", () => {
    expect(isChunkLoadError(new TypeError("Importing a module script failed."))).toBe(true); // Safari / WebKit
    expect(isChunkLoadError(new TypeError("Failed to fetch dynamically imported module: http://x/a.js"))).toBe(true); // Chromium
    expect(isChunkLoadError(new TypeError("error loading dynamically imported module"))).toBe(true); // Firefox
    expect(isChunkLoadError(new Error("Cannot read properties of undefined"))).toBe(false);
  });
});

describe("loading a screen on demand", () => {
  it("passes a successful load straight through", async () => {
    const d = deps();
    await expect(importWithReload(async () => "screen", d)).resolves.toBe("screen");
    expect(d.reload).not.toHaveBeenCalled();
  });

  it("reloads the page once when the screen's code is gone (an update, a dev server's new cache)", async () => {
    const d = deps();
    const pending = importWithReload(async () => {
      throw chunkError();
    }, d);
    await Promise.resolve();
    await Promise.resolve();
    expect(d.reload).toHaveBeenCalledTimes(1);
    // The page is going away: the load never settles into the crash screen.
    const settled = await Promise.race([pending.then(() => "settled", () => "settled"), new Promise((r) => setTimeout(() => r("pending"), 20))]);
    expect(settled).toBe("pending");
  });

  it("shows the error instead of reloading in a loop", async () => {
    const d = deps();
    void importWithReload(async () => {
      throw chunkError();
    }, d);
    await new Promise((r) => setTimeout(r, 0));
    d.advance(5_000);
    await expect(
      importWithReload(async () => {
        throw chunkError();
      }, d),
    ).rejects.toThrow("Importing a module script failed");
    expect(d.reload).toHaveBeenCalledTimes(1);
    // Much later, a new failure may reload again.
    d.advance(60_000);
    void importWithReload(async () => {
      throw chunkError();
    }, d);
    await new Promise((r) => setTimeout(r, 0));
    expect(d.reload).toHaveBeenCalledTimes(2);
  });

  it("leaves other errors alone", async () => {
    const d = deps();
    await expect(
      importWithReload(async () => {
        throw new Error("render bug");
      }, d),
    ).rejects.toThrow("render bug");
    expect(d.reload).not.toHaveBeenCalled();
  });
});

describe("a reload leaves a trace", () => {
  it("records why the page reloaded before reloading (#418)", async () => {
    const d = deps();
    void importWithReload(() => Promise.reject(chunkError()), d);
    await Promise.resolve();
    await Promise.resolve();
    expect(d.note).toHaveBeenCalledWith(expect.stringContaining("Importing a module script failed"));
    expect(d.note.mock.invocationCallOrder[0]).toBeLessThan(d.reload.mock.invocationCallOrder[0]);
  });
});
