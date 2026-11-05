import { afterEach, describe, expect, it, vi } from "vitest";

import { ENGINE_TIMEOUT_MS, engineFetch } from "./engineFetch";

describe("engineFetch", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("gives every request a limit, and leaves the rest of the request alone", async () => {
    const f = vi.fn(async (_url: string, _init?: RequestInit) => new Response("{}"));
    vi.stubGlobal("fetch", f);
    await engineFetch("http://e/x", { method: "POST", timeoutMs: 5 });
    const init = f.mock.calls[0][1]!;
    expect(init.method).toBe("POST");
    expect(init.signal).toBeInstanceOf(AbortSignal);
    expect("timeoutMs" in init).toBe(false);
  });

  it("says the engine did not answer when the limit runs out", async () => {
    vi.stubGlobal("fetch", async () => {
      throw new DOMException("timed out", "TimeoutError");
    });
    await expect(engineFetch("http://e/x")).rejects.toThrow(
      `did not answer within ${ENGINE_TIMEOUT_MS / 1000} seconds`,
    );
  });

  it("leaves other failures as they are", async () => {
    vi.stubGlobal("fetch", async () => {
      throw new TypeError("Failed to fetch");
    });
    await expect(engineFetch("http://e/x")).rejects.toThrow("Failed to fetch");
  });

  it("does not set a limit over a caller's own signal", async () => {
    const f = vi.fn(async (_url: string, _init?: RequestInit) => new Response("{}"));
    vi.stubGlobal("fetch", f);
    const signal = new AbortController().signal;
    await engineFetch("http://e/x", { signal });
    expect(f.mock.calls[0][1]!.signal).toBe(signal);
  });
});
