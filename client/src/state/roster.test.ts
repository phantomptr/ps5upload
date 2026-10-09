import { describe, expect, it } from "vitest";
import {
  profileNameForAddr,
  profileNameForHost,
  reorderProfiles,
  selectConsoleByAddress,
  useRosterStore,
  type PS5Profile,
} from "./roster";

function profile(partial: Partial<PS5Profile> & { host: string }): PS5Profile {
  return {
    id: partial.id ?? `id-${partial.host}`,
    name: partial.name ?? "",
    host: partial.host,
  };
}

const roster: PS5Profile[] = [
  profile({ host: "192.168.1.10", name: "Living-room PS5" }),
  profile({ host: "192.168.1.20", name: "Bedroom Pro" }),
  profile({ host: "10.0.0.5", name: "" }), // named-less profile
];

describe("profileNameForHost / profileNameForAddr", () => {
  it("returns the friendly name for a known bare host", () => {
    expect(profileNameForHost("192.168.1.10", roster)).toBe("Living-room PS5");
    expect(profileNameForHost("192.168.1.20", roster)).toBe("Bedroom Pro");
  });

  it("strips the port off a transfer addr before matching", () => {
    // Queue items store `ip:9113` (transfer) or `ip:9114` (mgmt) — both
    // must resolve to the same console name.
    expect(profileNameForAddr("192.168.1.10:9113", roster)).toBe(
      "Living-room PS5",
    );
    expect(profileNameForAddr("192.168.1.20:9114", roster)).toBe("Bedroom Pro");
  });

  it("falls back to the bare host when no profile matches", () => {
    expect(profileNameForAddr("192.168.99.99:9113", roster)).toBe(
      "192.168.99.99",
    );
    expect(profileNameForHost("172.16.0.1", roster)).toBe("172.16.0.1");
  });

  it("falls back to the bare host when the matched profile has no name", () => {
    // 10.0.0.5 exists in the roster but with an empty name — show the IP,
    // never an empty chip.
    expect(profileNameForAddr("10.0.0.5:9113", roster)).toBe("10.0.0.5");
  });

  it("treats a whitespace-only name as unnamed", () => {
    const r = [profile({ host: "192.168.1.30", name: "   " })];
    expect(profileNameForAddr("192.168.1.30:9113", r)).toBe("192.168.1.30");
  });

  it("matches even when the profile host itself carries a port", () => {
    const r = [profile({ host: "192.168.1.40:9113", name: "Quirky" })];
    expect(profileNameForAddr("192.168.1.40:9114", r)).toBe("Quirky");
  });

  it("returns empty string for an empty addr (no crash)", () => {
    expect(profileNameForAddr("", roster)).toBe("");
  });

  it("returns the input host when roster is empty", () => {
    expect(profileNameForAddr("192.168.1.10:9113", [])).toBe("192.168.1.10");
  });
});

describe("reorderProfiles", () => {
  const abc = [
    profile({ host: "10.0.0.1", name: "A" }),
    profile({ host: "10.0.0.2", name: "B" }),
    profile({ host: "10.0.0.3", name: "C" }),
  ];
  const names = (list: PS5Profile[] | null) => list?.map((p) => p.name) ?? null;

  it("moves a console to a new position", () => {
    expect(names(reorderProfiles(abc, 2, 0))).toEqual(["C", "A", "B"]);
    expect(names(reorderProfiles(abc, 0, 2))).toEqual(["B", "C", "A"]);
  });

  it("returns null for a move that goes nowhere", () => {
    // Null, not a copy: the caller skips the state write, so a drag released
    // on the row it started from does not re-render or re-persist.
    expect(reorderProfiles(abc, 1, 1)).toBeNull();
  });

  it("returns null for out-of-range indices", () => {
    // A drag released outside the list must not relocate the row.
    expect(reorderProfiles(abc, -1, 0)).toBeNull();
    expect(reorderProfiles(abc, 0, 99)).toBeNull();
    expect(reorderProfiles(abc, 99, 0)).toBeNull();
  });

  it("does not mutate the input", () => {
    reorderProfiles(abc, 2, 0);
    expect(names(abc.slice())).toEqual(["A", "B", "C"]);
  });
});

describe("selectConsoleByAddress", () => {
  const reset = (profiles: PS5Profile[], active: string | null) =>
    useRosterStore.setState({ profiles, active_id: active });

  it("selects a console already in the roster instead of re-pointing the active one", () => {
    reset([profile({ id: "a", host: "192.168.0.5" }), profile({ id: "b", host: "192.168.0.6" })], "a");
    selectConsoleByAddress("192.168.0.6");
    const s = useRosterStore.getState();
    expect(s.active_id).toBe("b");
    expect(s.profiles.find((p) => p.id === "a")?.host).toBe("192.168.0.5");
  });

  it("re-points the active console at a new address", () => {
    reset([profile({ id: "a", host: "192.168.0.5" })], "a");
    selectConsoleByAddress(" 10.0.0.9 ");
    expect(useRosterStore.getState().profiles[0].host).toBe("10.0.0.9");
  });

  it("adds and selects a console on a fresh install", () => {
    reset([], null);
    selectConsoleByAddress("10.0.0.9");
    const s = useRosterStore.getState();
    expect(s.profiles).toHaveLength(1);
    expect(s.active_id).toBe(s.profiles[0].id);
    expect(s.profiles[0].host).toBe("10.0.0.9");
  });

  it("ignores an empty address", () => {
    reset([profile({ id: "a", host: "192.168.0.5" })], "a");
    selectConsoleByAddress("   ");
    expect(useRosterStore.getState().profiles[0].host).toBe("192.168.0.5");
  });
});

describe("the engine's saved console state follows the roster", () => {
  it("forgets a console when it is removed or moves to a new IP", async () => {
    const calls: { url: string; body: string }[] = [];
    const realFetch = globalThis.fetch;
    globalThis.fetch = (async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), body: String(init?.body ?? "") });
      return new Response("{}");
    }) as typeof fetch;
    try {
      useRosterStore.setState({ profiles: [], active_id: null });
      calls.length = 0;
      const a = useRosterStore.getState().add({ name: "Pro", host: "192.168.1.10" });
      useRosterStore.getState().add({ name: "Phat", host: "192.168.1.20" });
      useRosterStore.getState().remove(a);
      await Promise.resolve();
      const keeps = calls.filter((c) => c.url.endsWith("/api/console-snapshots/keep"));
      expect(JSON.parse(keeps[keeps.length - 1].body)).toEqual({ hosts: ["192.168.1.20"] });
      // A rename changes no host: nothing is sent.
      const before = keeps.length;
      const id = useRosterStore.getState().profiles[0].id;
      useRosterStore.getState().rename(id, "Old Phat");
      await Promise.resolve();
      expect(calls.filter((c) => c.url.endsWith("/api/console-snapshots/keep")).length).toBe(before);
    } finally {
      globalThis.fetch = realFetch;
    }
  });
});
