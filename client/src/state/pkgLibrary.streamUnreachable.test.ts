import { describe, expect, it } from "vitest";
import { describeLinkDownload, originOf, streamUnreachableMessage } from "./pkgLibrary";

/* #327: a stream the PS5 never fetched from, on a network with no proxy, was
 * reported as a proxy problem. Zero requests without a proxy error means the
 * console could not reach this computer, so the message leads with that. */
describe("streamUnreachableMessage", () => {
  it("names the address the console was given and the usual causes", () => {
    const msg = streamUnreachableMessage("0x80431068", "http://192.168.1.20:19113");
    expect(msg).toContain("never reached this computer at http://192.168.1.20:19113");
    expect(msg).toContain("0x80431068");
    expect(msg).toContain("firewall");
    expect(msg).toContain("VPN");
    expect(msg).toContain("Upload & install");
  });

  it("still reads well without an address", () => {
    expect(streamUnreachableMessage("0x80431068", null)).toContain(
      "never reached this computer to fetch",
    );
  });
});

describe("originOf", () => {
  it("keeps only scheme, host and port", () => {
    expect(originOf("http://192.168.1.20:19113/pkg-host/abc/X.pkg")).toBe(
      "http://192.168.1.20:19113",
    );
  });

  it("returns null for nothing or garbage", () => {
    expect(originOf(undefined)).toBeNull();
    expect(originOf("not a url")).toBeNull();
  });
});


/* "Download through this computer" showed nothing until the download finished. */
describe("describeLinkDownload", () => {
  it("shows percent, bytes, speed and time left", () => {
    const line = describeLinkDownload(512 * 1024 * 1024, 1024 * 1024 * 1024, 64 * 1024 * 1024);
    expect(line).toContain("Downloading to this computer — 50%");
    expect(line).toMatch(/at .*\/s/);
    expect(line).toContain("·");
  });

  it("copes with an unknown size and no rate yet", () => {
    expect(describeLinkDownload(1000, 0, 0)).toBe("Downloading to this computer — 1000 B");
  });
});
