import { describe, expect, it } from "vitest";
import {
  profileNameForAddr,
  profileNameForHost,
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
