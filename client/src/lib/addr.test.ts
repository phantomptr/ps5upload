import { describe, expect, it } from "vitest";

import {
  hostOf,
  mgmtAddr,
  PS5_LOADER_PORT,
  PS5_AVA1_PORT,
  consoleAddr,
  transferAddr,
  withPort,
} from "./addr";

describe("port constants", () => {
  it("keeps Sony's expected loader port at 9021", () => {
    // Hardcoded in every PS5 homebrew loader; bumping this would
    // silently break every send-payload call site.
    expect(PS5_LOADER_PORT).toBe(9021);
  });
  it("keeps the helper's single AVA1 port at 9120", () => {
    expect(PS5_AVA1_PORT).toBe(9120);
  });
});

describe("consoleAddr", () => {
  it("consoleAddr_strips_any_port", () => {
    for (const shape of [
      "192.168.1.50",
      "192.168.1.50:9113",
      "192.168.1.50:9114",
      "192.168.1.50:9120",
      "192.168.1.50:9113:9114",
    ]) {
      expect(consoleAddr(shape)).toBe("192.168.1.50");
    }
    expect(consoleAddr("ps5.local:9120")).toBe("ps5.local");
    expect(consoleAddr("[fe80::1]:9120")).toBe("[fe80::1]:9120");
    expect(consoleAddr("")).toBe("");
  });
  it("makes the old mgmt and transfer helpers aliases of it", () => {
    expect(mgmtAddr("10.0.0.2:9113")).toBe("10.0.0.2");
    expect(transferAddr("10.0.0.2")).toBe("10.0.0.2");
  });
});

describe("hostOf", () => {
  it("returns bare IP from host:port", () => {
    expect(hostOf("192.168.1.50:9113")).toBe("192.168.1.50");
  });
  it("returns input unchanged when no colon", () => {
    expect(hostOf("192.168.1.50")).toBe("192.168.1.50");
  });
  it("handles DNS names", () => {
    expect(hostOf("ps5.local:9114")).toBe("ps5.local");
  });
  it("returns empty string for empty input", () => {
    expect(hostOf("")).toBe("");
  });
  it("defangs the pre-2.12 double-suffix footgun", () => {
    // toMgmtAddr-on-toMgmtAddr would have produced this. We
    // collapse to the LEFTMOST colon (the real host:port boundary)
    // so the result is still recoverable.
    expect(hostOf("192.168.1.50:9113:9114")).toBe("192.168.1.50");
  });
});

describe("withPort", () => {
  it("composes bare host + port", () => {
    expect(withPort("192.168.1.50", 9114)).toBe("192.168.1.50:9114");
  });
  it("strips an existing port suffix before recomposing", () => {
    // The whole point of the helper — accept any shape, produce
    // the canonical shape.
    expect(withPort("192.168.1.50:9113", 9114)).toBe("192.168.1.50:9114");
  });
  it("returns empty for empty input", () => {
    expect(withPort("", 9114)).toBe("");
  });
});

describe("named address helpers", () => {
  it("mgmtAddr and transferAddr are the bare host from any input shape", () => {
    expect(mgmtAddr("192.168.1.50")).toBe("192.168.1.50");
    expect(mgmtAddr("192.168.1.50:9113")).toBe("192.168.1.50");
    expect(transferAddr("192.168.1.50:9114")).toBe("192.168.1.50");
  });
});

describe("IPv6 addressing", () => {
  it("keeps a bare IPv6 literal intact in hostOf (no truncation)", () => {
    // Pre-fix this returned "fe80" — the first colon was treated as a
    // port separator, mangling every IPv6 mgmt/transfer address.
    expect(hostOf("fe80::1")).toBe("fe80::1");
    expect(hostOf("2001:db8::5")).toBe("2001:db8::5");
  });
  it("extracts the inner host from a bracketed IPv6", () => {
    expect(hostOf("[fe80::1]")).toBe("fe80::1");
    expect(hostOf("[fe80::1]:9114")).toBe("fe80::1");
  });
  it("brackets an IPv6 literal when composing host:port", () => {
    expect(withPort("fe80::1", 9114)).toBe("[fe80::1]:9114");
  });
  it("keeps an IPv6 console splittable for the engine (bracketed, port ignored)", () => {
    expect(consoleAddr("fe80::1")).toBe("[fe80::1]:9120");
    expect(consoleAddr("[fe80::1]:9113")).toBe("[fe80::1]:9120");
  });
  it("leaves IPv4 unbracketed and portless", () => {
    expect(mgmtAddr("192.168.1.50")).toBe("192.168.1.50");
  });
});
