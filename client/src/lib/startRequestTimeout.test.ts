import { describe, expect, it } from "vitest";

import { fetchStart, isRequestTimeout, START_TIMEOUT_MS } from "./startRequestTimeout";

describe("isRequestTimeout", () => {
  it("knows a timeout from any other failure", () => {
    expect(isRequestTimeout("engine request failed: error sending request: operation timed out")).toBe(true);
    expect(isRequestTimeout("signal timed out")).toBe(true);
    expect(isRequestTimeout("TimeoutError")).toBe(true);
    expect(isRequestTimeout("engine HTTP 502: payload rejected FS_LIST")).toBe(false);
  });
});

describe("fetchStart", () => {
  it("gives a start request a time limit", async () => {
    let signal: AbortSignal | undefined;
    await fetchStart("x", "http://e/start", { method: "POST" }, async (_i, init) => {
      signal = init?.signal ?? undefined;
      return new Response("{}");
    });
    expect(signal).toBeDefined();
    expect(START_TIMEOUT_MS).toBe(60_000);
  });

  it("turns a timeout into an error that says what was being started", async () => {
    const hang = (_i: string, init?: RequestInit) =>
      new Promise<Response>((_r, reject) => {
        init?.signal?.addEventListener("abort", () => reject(init.signal!.reason));
      });
    await expect(fetchStart("the download", "http://e/start", {}, hang, 20)).rejects.toThrow(
      /did not answer within 0 seconds while starting the download/,
    );
  });

  it("passes any other failure through", async () => {
    await expect(
      fetchStart("x", "http://e/start", {}, async () => {
        throw new TypeError("Failed to fetch");
      }),
    ).rejects.toThrow("Failed to fetch");
  });
});
