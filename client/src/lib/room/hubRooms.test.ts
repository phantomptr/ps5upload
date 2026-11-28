import { describe, expect, it } from "vitest";

import { fetchHubPreview, fetchRooms, fetchStatus, hubGet, hubHttpBase, readStatus, retryDelayMs } from "./hubRooms";
import { DEFAULT_QUERY } from "./rooms";

const res = (status: number, body: unknown, headers: Record<string, string> = {}) =>
  ({ status, json: async () => body, headers: new Headers(headers) }) as unknown as Response;

describe("hub rooms client", () => {
  it("waits Retry-After quietly on 429, with jitter", async () => {
    expect(retryDelayMs("3", null, () => 0)).toBe(3000);
    expect(retryDelayMs(null, { retry_s: 5 }, () => 1)).toBe(6500);
    expect(retryDelayMs(null, null, () => 0)).toBe(2000);
    const waits: number[] = [];
    let n = 0;
    const r = await hubGet("https://hub.example/rooms", {
      fetchImpl: async () => (n++ < 2 ? res(429, { why: "slow_down", retry_s: 1 }, { "retry-after": "1" }) : res(200, { rooms: [] })),
      wait: async (ms) => void waits.push(ms),
      random: () => 0,
    });
    expect(r?.status).toBe(200);
    expect(waits).toEqual([1000, 1000]);
    // Still limited after the tries: nothing, never an error for a person.
    const r2 = await hubGet("https://hub.example/rooms", { fetchImpl: async () => res(429, {}), wait: async () => {}, tries: 2 });
    expect(r2).toBeNull();
  });

  it("lists, maps 404 to an old hub and `off`", async () => {
    const src = { base: "https://hub.example", viaEngine: false };
    const urls: string[] = [];
    const ok = await fetchRooms(src, DEFAULT_QUERY, {
      fetchImpl: async (u) => {
        urls.push(String(u));
        return res(200, { rooms: [{ room: "A".repeat(22), name: "Couch", slots: [1, 4] }], next: null, proto: { min: 2, latest: 2 } });
      },
    });
    expect(urls[0]).toBe("https://hub.example/rooms?free=1&proto=2");
    expect(typeof ok === "object" && ok.rooms.length).toBe(1);
    expect(await fetchRooms(src, DEFAULT_QUERY, { fetchImpl: async () => res(404, {}) })).toBe("old_hub");
    expect(await fetchRooms(src, DEFAULT_QUERY, { fetchImpl: async () => res(200, { off: true, rooms: [] }) })).toBe("off");
    expect(await fetchRooms(src, DEFAULT_QUERY, { fetchImpl: async () => Promise.reject(new Error("x")) })).toBe("unreachable");
  });

  it("goes through the engine's proxy in the apps", async () => {
    const urls: string[] = [];
    await fetchStatus({ base: "http://127.0.0.1:19113", viaEngine: true }, ["B".repeat(22), "bad!"], {
      fetchImpl: async (u) => {
        urls.push(String(u));
        return res(200, { every_s: 60, rooms: {} });
      },
    });
    expect(urls).toEqual([`http://127.0.0.1:19113/api/join/rooms/status?r=${"B".repeat(22)}`]);
  });

  it("reads the friends lookup", () => {
    const s = readStatus({ every_s: 300, rooms: { ["C".repeat(22)]: { listed: false, locked: true, slots: [2, 4], pw: true, proto: 2, pk: "x" }, bad: {} } });
    expect(s.everyS).toBe(300);
    expect(Object.keys(s.rooms)).toEqual(["C".repeat(22)]);
    expect(s.rooms["C".repeat(22)]).toMatchObject({ locked: true, inRoom: 2, size: 4, pw: true });
  });

  it("preview misses", async () => {
    const src = { base: "https://hub.example", viaEngine: false };
    const room = "D".repeat(22);
    expect(await fetchHubPreview(src, room, { fetchImpl: async () => res(503, { why: "off" }) })).toBe("needs_direct");
    expect(await fetchHubPreview(src, room, { fetchImpl: async () => res(504, {}) })).toBe("no_answer");
    expect(await fetchHubPreview(src, room, { fetchImpl: async () => res(404, {}) })).toBe("no_room");
    expect(await fetchHubPreview(src, "x", {})).toBe("no_room");
  });

  it("hub http base", () => {
    expect(hubHttpBase("wss://hub.phantomptr.com/ws")).toBe("https://hub.phantomptr.com");
    expect(hubHttpBase("ws://127.0.0.1:9000")).toBe("http://127.0.0.1:9000");
    expect(hubHttpBase("nope")).toBeNull();
  });
});
