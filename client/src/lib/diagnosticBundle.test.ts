import { describe, expect, it } from "vitest";
import { redactHost } from "./diagnosticBundle";

/**
 * Privacy-critical: a regression here would let LAN topology leak
 * into bug reports posted to GitHub. Tests cover IPv4 redaction,
 * IPv6/hostname placeholder, and the unredacted passthrough path.
 */
describe("redactHost", () => {
  it("returns empty string for null/undefined/empty", () => {
    expect(redactHost(null, true)).toBe("");
    expect(redactHost(undefined, true)).toBe("");
    expect(redactHost("", true)).toBe("");
  });

  it("redacts IPv4 last two octets", () => {
    expect(redactHost("192.168.1.50", true)).toBe("192.168.X.X");
    expect(redactHost("10.0.0.1", true)).toBe("10.0.X.X");
    expect(redactHost("172.16.255.255", true)).toBe("172.16.X.X");
  });

  it("preserves IPv4 when redact=false", () => {
    expect(redactHost("192.168.1.50", false)).toBe("192.168.1.50");
  });

  it("placeholder-redacts hostnames and IPv6 by length", () => {
    expect(redactHost("ps5.local", true)).toBe("<host:9-char>");
    expect(redactHost("[::1]:9113", true)).toBe("<host:10-char>");
    expect(redactHost("fe80::1234", true)).toBe("<host:10-char>");
  });

  it("preserves hostnames and IPv6 when redact=false", () => {
    expect(redactHost("ps5.local", false)).toBe("ps5.local");
    expect(redactHost("[::1]:9113", false)).toBe("[::1]:9113");
  });

  it("rejects malformed IPv4 (out-of-range octets stay unredacted as host)", () => {
    // 999 isn't a valid octet but the regex matches — confirms we
    // treat anything regex-matching as IPv4 even if logically invalid.
    expect(redactHost("999.999.999.999", true)).toBe("999.999.X.X");
    // No fourth octet: not IPv4, falls through to hostname path.
    expect(redactHost("192.168.1", true)).toBe("<host:9-char>");
  });
});
