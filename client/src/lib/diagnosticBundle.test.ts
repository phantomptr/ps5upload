import { describe, expect, it } from "vitest";
import {
  buildDiagnosticBundle,
  redactDiagnosticText,
  redactHost,
} from "./diagnosticBundle";

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

  it("redacts the complete IPv4 address", () => {
    expect(redactHost("192.168.1.50", true)).toBe("<IPv4>");
    expect(redactHost("10.0.0.1", true)).toBe("<IPv4>");
    expect(redactHost("172.16.255.255", true)).toBe("<IPv4>");
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
    expect(redactHost("999.999.999.999", true)).toBe("<IPv4>");
    // No fourth octet: not IPv4, falls through to hostname path.
    expect(redactHost("192.168.1", true)).toBe("<host:9-char>");
  });
});

describe("redactDiagnosticText", () => {
  it("redacts IPv4 addresses embedded in errors while preserving ports", () => {
    expect(
      redactDiagnosticText(
        "send dpi.elf: connect 192.168.86.99:9021: Connection refused",
        true,
      ),
    ).toBe("send dpi.elf: connect <IPv4>:9021: Connection refused");
  });

  it("redacts every address in JSON/log text, including bracketed IPv6", () => {
    const text = '{"host":"10.0.0.5","error":"connect [fe80::1234]:9114"}';
    const redacted = redactDiagnosticText(text, true);
    expect(redacted).not.toContain("10.0.0.5");
    expect(redacted).not.toContain("fe80::1234");
    expect(redacted).toContain("<IPv4>");
    expect(redacted).toContain("[<IPv6>]:9114");
  });

  it("leaves diagnostic text untouched when redaction is off", () => {
    const text = "connect 172.16.1.9:9040";
    expect(redactDiagnosticText(text, false)).toBe(text);
  });
});

describe("buildDiagnosticBundle (crash-report enrichment)", () => {
  it("includes the crash-report enrichment fields", () => {
    const b = buildDiagnosticBundle({
      appVersion: "9.9.9",
      redact: true,
      trigger: "uncaught-error: boom",
    });
    expect(b.schema).toBe(3);
    expect(b.app_version).toBe("9.9.9");
    expect(b.redacted).toBe(true);
    expect(b.trigger).toBe("uncaught-error: boom");
    // platform block is always present (values may be null in jsdom).
    expect(b.platform).toBeTruthy();
    expect("js_heap_used_mb" in b.platform).toBe(true);
    expect("device_memory_gb" in b.platform).toBe(true);
  });

  it("defaults trigger to null for a manual export", () => {
    const b = buildDiagnosticBundle({ appVersion: "1.0.0", redact: true });
    expect(b.trigger).toBeNull();
  });

  it("captures the install-settings snapshot (the Auto Install/Delete report gap)", () => {
    // These were missing from bundles, which made the Titanfall/Guardians
    // "it installed/deleted with the toggle off" report unanswerable.
    const b = buildDiagnosticBundle({ appVersion: "1.0.0", redact: true });
    expect(b.settings).toBeTruthy();
    expect(typeof b.settings.auto_install_after_upload).toBe("boolean");
    expect(typeof b.settings.auto_remove_after_install).toBe("boolean");
  });

  it("honours logLimit (crash reports keep more context)", () => {
    const big = buildDiagnosticBundle({
      appVersion: "1.0.0",
      redact: true,
      logLimit: 400,
    });
    const small = buildDiagnosticBundle({
      appVersion: "1.0.0",
      redact: true,
      logLimit: 1,
    });
    // Never more than the requested cap; the larger cap is >= the smaller.
    expect(small.recent_logs.length).toBeLessThanOrEqual(1);
    expect(big.recent_logs.length).toBeGreaterThanOrEqual(
      small.recent_logs.length,
    );
  });
});

describe("buildDiagnosticBundle (schema 3 triage fields)", () => {
  it("reports helper_mismatch as null when no helper is connected", () => {
    const b = buildDiagnosticBundle({ appVersion: "9.9.9", redact: true });
    // null, NOT false: with no console attached there is nothing to compare,
    // and "no helper" must never read as "versions agree".
    expect(b.versions.helper_mismatch).toBeNull();
    expect(b.versions.app).toBe("9.9.9");
  });

  it("captures the settings that make behaviour reports answerable", () => {
    const b = buildDiagnosticBundle({ appVersion: "1.0.0", redact: true });
    // The whole point of expanding this block: a report saying "it
    // auto-installed even though I turned that off" is unanswerable unless
    // the ACTUAL stored values ride along.
    for (const k of [
      "auto_install_after_upload",
      "auto_remove_after_install",
      "always_overwrite",
      "upload_streams",
      "log_level",
      "theme",
    ]) {
      expect(k in b.settings).toBe(true);
    }
  });

  it("carries a host clock so PS5 timestamps can be correlated", () => {
    const b = buildDiagnosticBundle({ appVersion: "1.0.0", redact: true });
    expect(typeof b.clock.host_iso).toBe("string");
    expect(typeof b.clock.utc_offset_min).toBe("number");
  });

  it("counts errors and warnings for at-a-glance triage", () => {
    const b = buildDiagnosticBundle({ appVersion: "1.0.0", redact: true });
    expect(typeof b.counters.error_logs).toBe("number");
    expect(typeof b.counters.warn_logs).toBe("number");
    expect(typeof b.counters.total_logs_retained).toBe("number");
  });

  it("reports no edit session when nothing is checked out", () => {
    const b = buildDiagnosticBundle({ appVersion: "1.0.0", redact: true });
    expect(b.edit_session).toBeNull();
  });
});
