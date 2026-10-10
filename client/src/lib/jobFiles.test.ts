import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine.test" }));

import { fetchJobFiles } from "./jobFiles";

function stub(status: number, body: unknown) {
  const calls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      calls.push(url);
      return { ok: status < 400, status, json: async () => body };
    }),
  );
  return calls;
}

describe("fetchJobFiles", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("reads the job's file list from its own route", async () => {
    const calls = stub(200, {
      files: [
        { rel_path: "a.bin", size: 3 },
        { rel_path: 7, size: 1 },
      ],
    });
    expect(await fetchJobFiles("j 1")).toEqual([{ rel_path: "a.bin", size: 3 }]);
    expect(calls).toEqual(["http://engine.test/api/jobs/j%201/files"]);
  });

  it("is empty for an unknown job and null when the engine cannot be read", async () => {
    stub(404, {});
    expect(await fetchJobFiles("x")).toEqual([]);
    stub(500, {});
    expect(await fetchJobFiles("x")).toBeNull();
    stub(200, { nope: true });
    expect(await fetchJobFiles("x")).toBeNull();
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new Error("down"))));
    expect(await fetchJobFiles("x")).toBeNull();
  });
});
