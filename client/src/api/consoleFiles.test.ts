import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchConsoleStat, importLocalCheat, minionSet } from "./consoleFiles";
vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine" }));
afterEach(() => vi.unstubAllGlobals());
function reply(body: object, status = 200) {
  const fetcher = vi
    .fn()
    .mockImplementation(
      async (_input: RequestInfo | URL, _init?: RequestInit) =>
        new Response(JSON.stringify(body), { status }),
    );
  vi.stubGlobal("fetch", fetcher);
  return fetcher;
}
describe("paired console controls", () => {
  it("reads and sets Minion without changing LAN access", async () => {
    const fetcher = reply({
      supported: true,
      tile: true,
      tile_busy: true,
      tile_failed: false,
    });
    await minionSet("10.0.0.2", { tile: true });
    expect(fetcher.mock.calls[0][0]).toBe("http://engine/api/ps5/minion/tile");
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
      addr: "10.0.0.2",
      tile: true,
    });
    await minionSet("10.0.0.2", {});
    expect(JSON.parse(fetcher.mock.calls[1][1].body)).not.toHaveProperty(
      "tile",
    );
  });
  it("passes the helper's tile result through when it sends one", async () => {
    reply({
      supported: true,
      tile: false,
      tile_busy: false,
      tile_failed: false,
      tile_result: "hidden",
    });
    expect(await minionSet("h", { tile: false })).toMatchObject({
      tile: false,
      tile_result: "hidden",
    });
    reply({ supported: true, tile: false, tile_busy: false, tile_failed: false });
    expect(await minionSet("h", {})).not.toHaveProperty("tile_result");
  });
  it("passes why adding the tile failed through, and drops an empty reason", async () => {
    reply({
      supported: true,
      tile: true,
      tile_busy: false,
      tile_failed: true,
      tile_result: "failed",
      tile_error: "not_listed",
    });
    expect(await minionSet("h", {})).toMatchObject({ tile_failed: true, tile_error: "not_listed" });
    reply({ supported: true, tile: true, tile_busy: false, tile_failed: false, tile_error: "" });
    expect(await minionSet("h", {})).not.toHaveProperty("tile_error");
  });
  it("turns Minion off and on and reports whether the helper can", async () => {
    const fetcher = reply({
      supported: true,
      tile: true,
      enabled: false,
      tile_busy: true,
      tile_failed: false,
      tile_result: "",
    });
    expect(await minionSet("10.0.0.2", { enabled: false })).toMatchObject({
      enabled: false,
      tile: true,
      tile_busy: true,
    });
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({
      addr: "10.0.0.2",
      enabled: false,
    });
    // A helper without the switch sends no `enabled`: the screen then keeps only the tile buttons.
    reply({ supported: true, tile: true, tile_busy: false, tile_failed: false });
    expect(await minionSet("h")).not.toHaveProperty("enabled");
  });
  it("feature detects old helpers and rejects malformed settings", async () => {
    reply({ supported: false });
    expect(await minionSet("h", {})).toEqual({ supported: false });
    reply({ supported: true });
    await expect(minionSet("h", {})).rejects.toThrow("Invalid Minion");
  });
  it("rejects invalid stats and surfaces helper errors", async () => {
    reply({ kind: "file", mode: "0644" });
    await expect(fetchConsoleStat("h", "/data/x")).rejects.toThrow(
      "Invalid file stat",
    );
    reply({ error: "permission denied" }, 502);
    await expect(fetchConsoleStat("h", "/data/x")).rejects.toThrow(
      "permission denied",
    );
  });
  it("imports via the engine and requires a confirmed destination", async () => {
    const fetcher = reply({
      ok: true,
      path: "/data/ps5upload/cheats/json/PPSA00001.json",
    });
    expect((await importLocalCheat("h", "/tmp/PPSA00001.json")).path).toContain(
      "PPSA00001.json",
    );
    expect(JSON.parse(fetcher.mock.calls[0][1].body).path).toBe(
      "/tmp/PPSA00001.json",
    );
    reply({ ok: false, error: "already exists" });
    await expect(importLocalCheat("h", "/tmp/PPSA00001.json")).rejects.toThrow(
      "already exists",
    );
  });
});
