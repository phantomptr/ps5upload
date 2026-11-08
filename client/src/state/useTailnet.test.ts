import { describe, expect, it } from "vitest";
import type { AppTailnetStatus } from "../api/tailnet";
import { consoleOnTailnet, selfTailnet } from "./useTailnet";

const st = (over: Partial<AppTailnetStatus>): AppTailnetStatus =>
  ({ ok: true, enabled: false, state: "off", peers: [], consoles: [], ...over }) as AppTailnetStatus;

describe("useTailnet helpers", () => {
  it("finds a console by its home address, whatever port the address carries", () => {
    const s = st({ consoles: [{ host: "192.0.2.9", ip: "100.64.0.7", name: "ps5", via: "direct", latency_ms: 24.4 }] });
    expect(consoleOnTailnet(s, "192.0.2.9")).toEqual({ ip: "100.64.0.7", via: "direct", latencyMs: 24.4 });
    expect(consoleOnTailnet(s, "192.0.2.9:9021")?.ip).toBe("100.64.0.7");
    expect(consoleOnTailnet(s, "192.0.2.10")).toBeNull();
    expect(consoleOnTailnet(null, "192.0.2.9")).toBeNull();
  });

  it("reads anything but direct or relayed as the home network", () => {
    const s = st({ consoles: [{ host: "192.0.2.9", ip: "100.64.0.7", name: "", via: "weird", latency_ms: -1 }] });
    expect(consoleOnTailnet(s, "192.0.2.9")?.via).toBe("lan");
  });

  it("tells on, connecting, paused (signed in, off) and never set up apart", () => {
    expect(selfTailnet(st({ enabled: true, state: "running" }))).toBe("on");
    expect(selfTailnet(st({ enabled: true, state: "needs-login" }))).toBe("connecting");
    expect(selfTailnet(st({ enabled: false, signed_in: true }))).toBe("paused");
    expect(selfTailnet(st({ enabled: false }))).toBe("none");
    expect(selfTailnet(null)).toBe("none");
  });
});
