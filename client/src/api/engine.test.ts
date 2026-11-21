import { afterEach, describe, expect, it, vi } from "vitest";

import { engineApi } from "./engine";

describe("engineApi.ping", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("cancels the /api/version response body on 200 OK and returns true", async () => {
    const res = new Response('{"version":"12.6.16"}', { status: 200 });
    const cancelSpy = vi.spyOn(res.body!, "cancel");
    const fetchMock = vi.fn(async (_url: string, _init?: RequestInit) => res);
    vi.stubGlobal("fetch", fetchMock);

    await expect(engineApi.ping()).resolves.toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toMatch(/\/api\/version$/);
    expect(cancelSpy).toHaveBeenCalledTimes(1);
  });

  it("cancels the /api/version response body on non-404 error status and returns false", async () => {
    const res = new Response("error", { status: 500 });
    const cancelSpy = vi.spyOn(res.body!, "cancel");
    const fetchMock = vi.fn(async (_url: string, _init?: RequestInit) => res);
    vi.stubGlobal("fetch", fetchMock);

    await expect(engineApi.ping()).resolves.toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(cancelSpy).toHaveBeenCalledTimes(1);
  });

  it("cancels both /api/version (404) and fallback /api/jobs response bodies", async () => {
    const versionRes = new Response("not found", { status: 404 });
    const jobsRes = new Response("[]", { status: 200 });
    const versionCancel = vi.spyOn(versionRes.body!, "cancel");
    const jobsCancel = vi.spyOn(jobsRes.body!, "cancel");
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(versionRes)
      .mockResolvedValueOnce(jobsRes);
    vi.stubGlobal("fetch", fetchMock);

    await expect(engineApi.ping()).resolves.toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(String(fetchMock.mock.calls[1][0])).toMatch(/\/api\/jobs$/);
    expect(versionCancel).toHaveBeenCalledTimes(1);
    expect(jobsCancel).toHaveBeenCalledTimes(1);
  });

  it("returns false when fetch rejects", async () => {
    vi.stubGlobal("fetch", async () => {
      throw new TypeError("Failed to fetch");
    });

    await expect(engineApi.ping()).resolves.toBe(false);
  });
});
