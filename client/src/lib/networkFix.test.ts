import { describe, expect, it } from "vitest";
import type { NetDiag } from "../api/ps5";
import { networkFixOffers } from "./networkFix";

const diag = (over: Partial<NetDiag> = {}): NetDiag => ({
  adapter: "Ethernet 3",
  local_ip: "192.168.88.1",
  category: "public",
  firewall_enabled: true,
  allowed_by_rule: false,
  ...over,
});

describe("networkFixOffers", () => {
  it("offers both fixes for a Public cable with no rule", () => {
    expect(networkFixOffers(diag())).toEqual({ makePrivate: true, allowProfile: "public" });
  });
  it("offers only the rule for a Private network without one", () => {
    expect(networkFixOffers(diag({ category: "private" }))).toEqual({
      makePrivate: false,
      allowProfile: "private",
    });
  });
  it("offers nothing when Windows is not the blocker", () => {
    expect(networkFixOffers(diag({ firewall_enabled: false }))).toEqual({ makePrivate: false, allowProfile: null });
    expect(networkFixOffers(diag({ allowed_by_rule: true }))).toEqual({ makePrivate: false, allowProfile: null });
  });
  it("offers nothing without a diagnosis or for an unidentified or domain network", () => {
    expect(networkFixOffers(null)).toEqual({ makePrivate: false, allowProfile: null });
    expect(networkFixOffers(undefined)).toEqual({ makePrivate: false, allowProfile: null });
    expect(networkFixOffers(diag({ category: "unknown" })).allowProfile).toBeNull();
    expect(networkFixOffers(diag({ category: "domain" })).allowProfile).toBeNull();
  });
  it("still offers the fixes when a field was not read", () => {
    expect(networkFixOffers(diag({ firewall_enabled: null, allowed_by_rule: null })).makePrivate).toBe(true);
  });
});
