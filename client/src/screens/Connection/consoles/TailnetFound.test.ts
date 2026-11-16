import { describe, expect, it } from "vitest";
import { foundKind, type FoundConsole } from "../../../api/tailnet";
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
  kind: "ps5",
  ...over,
});

describe("what a tailnet find is", () => {
  it("a PS5: the engine says so, or (an older engine) its helper answered with its key", () => {
    expect(foundKind(found({}))).toBe("ps5");
    expect(foundKind({ kind: undefined, reachable: true, id: "ab" })).toBe("ps5");
  });

  it("a phone running PS5Upload: named …-ps5u, but nothing answers as a PS5", () => {
    const phone = found({ name: "pixel-9", tailnet_name: "pixel-9-ps5u", reachable: false, id: null });
    expect(foundKind(found({ ...phone, kind: "device", os: "android" }))).toBe("device");
    expect(foundKind(found({ ...phone, kind: "unknown" }))).toBe("unknown");
    // An older engine (no kind): no answer, no key, so not a PS5.
    expect(foundKind(found({ ...phone, kind: undefined }))).toBe("unknown");
  });

  it("a computer running PS5Upload: the same", () => {
    const mac = found({ name: "yunpengs-mbp", tailnet_name: "yunpengs-mbp-ps5u", reachable: false, id: null });
    expect(foundKind(found({ ...mac, kind: "device", os: "macOS" }))).toBe("device");
    expect(foundKind(found({ ...mac, kind: undefined }))).toBe("unknown");
  });
});

describe("Found on your Tailscale network", () => {
  it("never offers a phone or a computer, only a PS5", () => {
    const offer = toOffer(
      [
        found({ name: "pixel-9", tailnet_ip: "100.64.0.20", reachable: false, id: null, kind: "unknown" }),
        found({ name: "yunpengs-mbp", tailnet_ip: "100.64.0.21", reachable: false, id: null, kind: "device", os: "macOS" }),
        found({ name: "pixel-9a", tailnet_ip: "100.64.0.22", reachable: false, id: null, kind: undefined }),
        found({ name: "den", tailnet_ip: "100.64.0.9" }),
      ],
      [],
      new Set(),
    );
    expect(offer.map((c) => c.name)).toEqual(["den"]);
  });

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
