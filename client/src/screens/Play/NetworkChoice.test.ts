import { describe, expect, it } from "vitest";
import { urlHost, splitByNetwork } from "./NetworkChoice";

describe("splitByNetwork", () => {
  const lan = "http://192.168.1.20:19113/join#i=abcdefgh";
  const lan2 = "http://10.0.0.5:19113/join#i=abcdefgh";
  const ts = "http://100.101.102.103:19113/join#i=abcdefgh";

  it("sets the Tailscale link apart from the home network's", () => {
    expect(splitByNetwork([lan, lan2, ts], "100.101.102.103")).toEqual({ lan: [lan, lan2], tailnet: ts });
  });

  it("has no Tailscale link without the engine naming one", () => {
    expect(splitByNetwork([lan, ts], null)).toEqual({ lan: [lan, ts], tailnet: null });
    expect(splitByNetwork([lan], "100.101.102.103")).toEqual({ lan: [lan], tailnet: null });
  });

  it("reads hosts of links and bare addresses", () => {
    expect(urlHost(ts)).toBe("100.101.102.103");
    expect(urlHost("10.0.0.5:19113")).toBe("10.0.0.5");
  });
});
