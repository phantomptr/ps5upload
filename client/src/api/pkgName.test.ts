import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine.test:19113" }));

import { readPkgName, setPkgName } from "./pkgName";

const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), { status });

afterEach(() => vi.unstubAllGlobals());

describe("a package's name (#937)", () => {
  it("reads the names by the package's path, encoded", async () => {
    const names = { platform: "ps5", name: "A Game", entries: [], other_names: 0, max_bytes: 127 };
    const f = vi.fn().mockResolvedValue(reply(200, names));
    vi.stubGlobal("fetch", f);
    expect(await readPkgName("/games/My Game & more.pkg")).toEqual(names);
    expect(f.mock.calls[0][0]).toBe(
      "http://engine.test:19113/api/pkg/name?path=%2Fgames%2FMy%20Game%20%26%20more.pkg",
    );
  });

  it("changes the name with the path, the name and the language choice", async () => {
    const done = { platform: "ps5", previous: "A Game-x", name: "A Game", changed: 2, written_bytes: 2236 };
    const f = vi.fn().mockResolvedValue(reply(200, done));
    vi.stubGlobal("fetch", f);
    expect(await setPkgName("/games/a.pkg", "A Game", true)).toEqual(done);
    expect(f.mock.calls[0][0]).toBe("http://engine.test:19113/api/pkg/name");
    expect(f.mock.calls[0][1].method).toBe("POST");
    expect(JSON.parse(f.mock.calls[0][1].body)).toEqual({
      path: "/games/a.pkg",
      name: "A Game",
      all_languages: true,
    });
  });

  it("gives the engine's reason when it refuses", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(reply(409, { error: "a PS5 is installing from this package" })),
    );
    await expect(setPkgName("/games/a.pkg", "A Game", false)).rejects.toThrow("installing from this package");
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("not json", { status: 500 })));
    await expect(readPkgName("/games/a.pkg")).rejects.toThrow("HTTP 500");
    // An answer that is no list of names (an older engine) is an error, never an empty field.
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(reply(200, {})));
    await expect(readPkgName("/games/a.pkg")).rejects.toThrow("did not say");
  });
});
