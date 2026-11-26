import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine.test:19120" }));

import { driveApi } from "./meshDrive";

const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), { status });

afterEach(() => vi.unstubAllGlobals());

describe("leaving a drive, or deleting it", () => {
  it("says when the console did not answer: done here, and there once it is back", async () => {
    const f = vi.fn().mockResolvedValue(reply(200, { supported: true, ok: true, later: true, note: "x" }));
    vi.stubGlobal("fetch", f);
    expect(await driveApi.leave("10.0.0.2", "d1", "m1")).toEqual({ later: true });
    expect(f.mock.calls[0][0]).toBe("http://engine.test:19120/api/drive/leave");
    expect(JSON.parse(f.mock.calls[0][1].body)).toEqual({ addr: "10.0.0.2", drive: "d1", member: "m1" });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(reply(200, { supported: true, ok: true, later: true })));
    expect(await driveApi.remove("10.0.0.2", "d1")).toEqual({ later: true });
  });

  it("is plainly done when the console answered, or by an older engine", async () => {
    vi.stubGlobal("fetch", vi.fn().mockImplementation(async () => reply(200, { supported: true, ok: true })));
    expect(await driveApi.leave("10.0.0.2", "d1", "m1")).toEqual({ later: false });
    expect(await driveApi.remove("10.0.0.2", "d1")).toEqual({ later: false });
  });

  it("still fails when it did not happen", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(reply(502, { error: "10.0.0.2 does not answer" })));
    await expect(driveApi.remove("10.0.0.2", "d1")).rejects.toThrow("does not answer");
  });
});
