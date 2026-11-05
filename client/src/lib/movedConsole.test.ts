import { describe, expect, it, vi } from "vitest";

import type { DiscoveredHost } from "../api/ps5";
import { findMovedConsole, movedCandidates, shouldLook, LOOK_EVERY_MS } from "./movedConsole";

const host = (ip: string, over: Partial<DiscoveredHost> = {}): DiscoveredHost => ({
  ip,
  hostname: null,
  all_ips: [ip],
  services: [],
  loader_port_open: true,
  payload_port_open: true,
  confidence: 90,
  likely_ps5: true,
  ...over,
});

describe("movedCandidates", () => {
  it("keeps PS5s running our helper at another address", () => {
    const got = movedCandidates(
      "192.168.137.2",
      [
        host("192.168.137.2"), // the old address itself
        host("192.168.2.36"),
        host("192.168.2.40", { payload_port_open: false }), // cannot prove who it is
        host("192.168.2.50"), // another console in the roster
      ],
      ["192.168.137.2", "192.168.2.50:9114"],
    );
    expect(got).toEqual(["192.168.2.36"]);
  });

  it("checks at most four", () => {
    const many = Array.from({ length: 9 }, (_, i) => host(`10.0.0.${i + 10}`));
    expect(movedCandidates("10.0.0.2", many, [])).toHaveLength(4);
  });
});

describe("findMovedConsole", () => {
  it("returns the first address the engine proves is the same console", async () => {
    const same = vi.fn(async (_o: string, n: string) =>
      n === "10.0.0.11" ? ("same" as const) : ("different" as const),
    );
    const got = await findMovedConsole("10.0.0.2", [], {
      discover: async () => [host("10.0.0.10"), host("10.0.0.11"), host("10.0.0.12")],
      sameConsole: same,
    });
    expect(got).toBe("10.0.0.11");
    expect(same).toHaveBeenCalledTimes(2); // stops at the first match
  });

  it("offers nothing when no answer proves it", async () => {
    const got = await findMovedConsole("10.0.0.2", [], {
      discover: async () => [host("10.0.0.10")],
      sameConsole: async () => "unknown",
    });
    expect(got).toBeNull();
  });

  it("treats a failed scan as nothing found", async () => {
    const got = await findMovedConsole("10.0.0.2", [], {
      discover: async () => {
        throw new Error("mDNS unavailable");
      },
      sameConsole: async () => "same",
    });
    expect(got).toBeNull();
  });
});

describe("shouldLook", () => {
  it("looks once the console is down, then not again for a while", () => {
    expect(shouldLook("down", undefined, 1_000)).toBe(true);
    expect(shouldLook("up", undefined, 1_000)).toBe(false);
    expect(shouldLook("unknown", undefined, 1_000)).toBe(false);
    expect(shouldLook("down", 1_000, 1_000 + LOOK_EVERY_MS - 1)).toBe(false);
    expect(shouldLook("down", 1_000, 1_000 + LOOK_EVERY_MS)).toBe(true);
  });
});

describe("looking for a console that is asleep", () => {
  it("does not scan the network for one that said it is in rest mode", () => {
    expect(shouldLook("down", undefined, 1_000, "rest")).toBe(false);
  });

  it("still looks for one that stopped answering: that is what a moved console does", () => {
    expect(shouldLook("down", undefined, 1_000, "unreachable")).toBe(true);
    expect(shouldLook("down", undefined, 1_000, "unknown")).toBe(true);
  });
});
