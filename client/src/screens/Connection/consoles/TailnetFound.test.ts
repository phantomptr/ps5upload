import { describe, expect, it } from "vitest";
import type { FoundConsole } from "../../../api/tailnet";
import { toOffer } from "./TailnetFound";

const found = (over: Partial<FoundConsole>): FoundConsole => ({
  name: "office",
  tailnet_name: "office-ps5u",
  tailnet_ip: "100.64.0.9",
  lan_host: null,
  id: "ab",
  reachable: true,
  via: "builtin",
  online: true,
  paired: false,
  hosts: [],
  listed: null,
  error: null,
  ...over,
});

describe("Found on your Tailscale network", () => {
  it("offers an online console that isn't on the list", () => {
    expect(toOffer([found({})], [], new Set()).map((c) => c.name)).toEqual(["office"]);
  });

  it("leaves out offline, listed and put-aside consoles", () => {
    const profiles = [{ host: "192.168.1.120" }, { host: "100.64.0.12:9021" }];
    const offer = toOffer(
      [
        found({ online: false }),
        found({ tailnet_ip: "100.64.0.10", listed: { id: "p1", name: "Den", host: "192.168.1.9" } }),
        found({ tailnet_ip: "100.64.0.11", lan_host: "192.168.1.120" }),
        found({ tailnet_ip: "100.64.0.12" }),
        found({ tailnet_ip: "100.64.0.13" }),
        found({ tailnet_ip: "100.64.0.14", name: "kept" }),
      ],
      profiles,
      new Set(["100.64.0.13"]),
    );
    expect(offer.map((c) => c.name)).toEqual(["kept"]);
  });
});
