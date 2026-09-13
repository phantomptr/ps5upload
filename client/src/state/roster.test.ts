import { describe, expect, it } from "vitest";
import {
  profileNameForAddr,
  profileNameForHost,
  reorderProfiles,
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
