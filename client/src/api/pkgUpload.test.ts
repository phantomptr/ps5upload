import { afterEach, describe, expect, it, vi } from "vitest";

import { deleteBrowserPkgUpload, stageBrowserPkg } from "./pkgUpload";

vi.mock("../state/engine", () => ({
  getEngineUrl: () => "http://engine.test:19113",
}));

describe("browser package upload", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("stages the selected File as multipart and maps the engine response", async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({ url, init });
        return {
          ok: true,
          json: async () => ({
            upload_id: "f983a63c-e6f7-489c-b2d7-14d994eff321",
            path: "/tmp/ps5upload-pkg-upload/f983/game.pkg",
            filename: "game.pkg",
            size: 4,
          }),
        } as Response;
      }),
    );

    const file = new File(["test"], "game.pkg");
    await expect(stageBrowserPkg(file)).resolves.toEqual({
      uploadId: "f983a63c-e6f7-489c-b2d7-14d994eff321",
      path: "/tmp/ps5upload-pkg-upload/f983/game.pkg",
      filename: "game.pkg",
      size: 4,
    });
    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe("http://engine.test:19113/api/pkg/upload");
    expect(calls[0]?.init?.method).toBe("POST");
    expect(calls[0]?.init?.body).toBeInstanceOf(FormData);
    const sent = (calls[0]?.init?.body as FormData).get("pkg") as File;
    expect(sent.name).toBe("game.pkg");
    expect(sent.size).toBe(file.size);
  });

  it("deletes temporary staging by opaque upload id", async () => {
    const fetchMock = vi.fn(async () => ({
      ok: true,
      json: async () => ({ removed: true }),
    })) as unknown as typeof fetch;
    vi.stubGlobal("fetch", fetchMock);

    await deleteBrowserPkgUpload("abc/../123");

    expect(fetchMock).toHaveBeenCalledWith(
      "http://engine.test:19113/api/pkg/upload/abc%2F..%2F123",
      { method: "DELETE" },
    );
  });

  it("rejects malformed successful responses instead of leaking undefined paths", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: true, json: async () => ({}) } as Response)),
    );

    await expect(stageBrowserPkg(new File(["x"], "x.pkg"))).rejects.toThrow(
      "invalid package-upload response",
    );
  });
});
