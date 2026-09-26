import { describe, expect, it } from "vitest";

import {
  hostOf,
  loaderAddr,
  mgmtAddr,
  portOf,
  PS5_LOADER_PORT,
  PS5_MGMT_PORT,
  PS5_TRANSFER_PORT,
  transferAddr,
  withPort,
} from "./addr";

describe("port constants", () => {
  it("keeps Sony's expected loader port at 9021", () => {
    // Hardcoded in every PS5 homebrew loader; bumping this would
    // silently break every send-payload call site.
    expect(PS5_LOADER_PORT).toBe(9021);
  });
  it("keeps our payload transfer port at 9113", () => {
    expect(PS5_TRANSFER_PORT).toBe(9113);
  });
  it("keeps our payload mgmt port at 9114", () => {
    expect(PS5_MGMT_PORT).toBe(9114);
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

describe("named port helpers", () => {
  it("mgmtAddr targets :9114 from any input shape", () => {
    expect(mgmtAddr("192.168.1.50")).toBe("192.168.1.50:9114");
    expect(mgmtAddr("192.168.1.50:9113")).toBe("192.168.1.50:9114");
    expect(mgmtAddr("192.168.1.50:9114")).toBe("192.168.1.50:9114");
  });
  it("transferAddr targets :9113", () => {
    expect(transferAddr("192.168.1.50")).toBe("192.168.1.50:9113");
    expect(transferAddr("192.168.1.50:9114")).toBe("192.168.1.50:9113");
  });
  it("loaderAddr targets :9021", () => {
    expect(loaderAddr("192.168.1.50")).toBe("192.168.1.50:9021");
  });
});

describe("portOf", () => {
  it("extracts port from host:port", () => {
    expect(portOf("192.168.1.50:9114")).toBe(9114);
  });
  it("returns null when no port", () => {
    expect(portOf("192.168.1.50")).toBeNull();
  });
  it("returns null for non-numeric suffix", () => {
    expect(portOf("192.168.1.50:abc")).toBeNull();
  });
  it("rejects ports outside the valid range", () => {
    expect(portOf("192.168.1.50:0")).toBeNull();
    expect(portOf("192.168.1.50:99999")).toBeNull();
  });
  it("returns null for empty input", () => {
    expect(portOf("")).toBeNull();
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
    expect(mgmtAddr("2001:db8::5")).toBe("[2001:db8::5]:9114");
    expect(transferAddr("fe80::1")).toBe("[fe80::1]:9113");
    expect(loaderAddr("fe80::1")).toBe("[fe80::1]:9021");
  });
  it("re-brackets a bracketed IPv6 when swapping the port", () => {
    expect(mgmtAddr("[fe80::1]:9113")).toBe("[fe80::1]:9114");
  });
  it("leaves IPv4 unbracketed", () => {
    expect(mgmtAddr("192.168.1.50")).toBe("192.168.1.50:9114");
  });
  it("portOf reads a bracketed IPv6 port and null for a bare literal", () => {
    expect(portOf("[fe80::1]:9114")).toBe(9114);
    expect(portOf("[fe80::1]")).toBeNull();
    expect(portOf("fe80::1")).toBeNull();
  });
});
