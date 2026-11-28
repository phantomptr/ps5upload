import { beforeEach, describe, expect, it, vi } from "vitest";

const disk = vi.hoisted(() => ({ items: new Map<string, string>() }));
vi.mock("../../lib/safeStorage", async (orig) => ({
  ...(await orig<typeof import("../../lib/safeStorage")>()),
  safeGetItem: (k: string) => disk.items.get(k) ?? null,
  safeSetItem: (k: string, v: string) => void disk.items.set(k, v),
}));

import { keepAccountId, keptAccountId, keptIdLost, sameAccountId } from "./accountIdMemory";

describe("account IDs this app set (#971)", () => {
  beforeEach(() => disk.items.clear());

  it("keeps the ID per console and user", () => {
    keepAccountId("192.0.2.10", 0x10000001, "0xab12", "Ava");
    expect(keptAccountId("192.0.2.10", 0x10000001)).toEqual({ id: "0xab12", name: "Ava" });
    expect(keptAccountId("192.0.2.11", 0x10000001)).toBeNull();
    expect(keptAccountId("192.0.2.10", 0x10000002)).toBeNull();
    // Nothing kept for no user or a bad ID.
    keepAccountId("192.0.2.10", 0, "0x1", "x");
    keepAccountId("192.0.2.10", 5, "nope", "x");
    expect(keptAccountId("192.0.2.10", 5)).toBeNull();
  });

  it("one console whatever port its address was written with", () => {
    // Accounts keeps it under the helper's address; Play › Set up asks with the host as typed.
    keepAccountId("192.0.2.10", 0x10000001, "0xab12", "Ava");
    expect(keptAccountId("192.0.2.10:9021", 0x10000001)).toEqual({ id: "0xab12", name: "Ava" });
    keepAccountId("192.0.2.12:9114", 7, "0xcd", "Bo");
    expect(keptAccountId("192.0.2.12", 7)).toEqual({ id: "0xcd", name: "Bo" });
  });

  it("survives storage it cannot read", () => {
    disk.items.set("ps5upload.account_ids.v1", "{not json");
    expect(keptAccountId("192.0.2.10", 1)).toBeNull();
    disk.items.set("ps5upload.account_ids.v1", JSON.stringify({ "192.0.2.10|1": { id: 7 } }));
    expect(keptAccountId("192.0.2.10", 1)).toBeNull();
  });

  it("compares the 8 bytes, not the spelling", () => {
    expect(sameAccountId("0x00000000000000ab", "0xAB")).toBe(true);
    expect(sameAccountId("0xab", "171")).toBe(true);
    expect(sameAccountId("0xab", "0xac")).toBe(false);
    // Zero and absent are both no ID.
    expect(sameAccountId("0x0000000000000000", undefined)).toBe(true);
  });

  it("says the ID is lost when the slot reads none or another one", () => {
    const kept = { id: "0xab12", name: "Ava" };
    expect(keptIdLost(kept, "0x0000000000000000")).toBe(true);
    expect(keptIdLost(kept, undefined)).toBe(true);
    expect(keptIdLost(kept, "0x000000000000ab12")).toBe(false);
    expect(keptIdLost(null, "0x0")).toBe(false);
  });
});
