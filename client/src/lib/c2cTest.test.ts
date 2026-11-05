import { describe, expect, it } from "vitest";

import { parseConnectionTest } from "../api/ava1";
import { testLines } from "./c2cTest";

const keys = (raw: unknown) => testLines(parseConnectionTest(raw), "Pro", "Phat").map((l) => l.key);

describe("Test connection verdicts", () => {
  it("say in one line per leg what the consoles and this device can reach", () => {
    const t = parseConnectionTest({
      direct: { verdict: "reachable", ms: 4, target: "192.0.2.99:9120" },
      relay: { from: { verdict: "ok" }, to: { verdict: "ok" } },
      deep: { ok: true, ms: 812 },
    });
    const lines = testLines(t, "Pro", "Phat");
    expect(lines.map((l) => [l.key, l.tone])).toEqual([
      ["c2c_test_reachable", "good"],
      ["c2c_test_relay_ok", "good"],
      ["c2c_test_deep_ok", "good"],
    ]);
    expect(lines[0].fallback).toBe("Pro reaches Phat at 192.0.2.99:9120 (4 ms): a direct send can connect.");
    expect(lines[2].vars).toEqual({ to: "Phat", ms: 812 });
  });

  it("name what failed", () => {
    expect(keys({ direct: { verdict: "refused", target: "127.0.0.1:19120" } })).toEqual(["c2c_test_refused"]);
    expect(keys({ direct: { verdict: "timeout" } })).toEqual(["c2c_test_timeout"]);
    expect(keys({ direct: { verdict: "unreachable" } })).toEqual(["c2c_test_unreachable"]);
    expect(keys({ direct: { verdict: "bad_address", detail: "x" } })).toEqual(["c2c_test_bad_address"]);
    expect(keys({ direct: { verdict: "helper_too_old" } })).toEqual(["c2c_test_helper_old"]);
    expect(keys({ direct: { verdict: "something new", detail: "?" } })).toEqual(["c2c_test_error"]);
    // Only the leg that failed is named.
    const relay = testLines(
      parseConnectionTest({ relay: { from: { verdict: "ok" }, to: { verdict: "not_paired", detail: "pair it" } } }),
      "Pro",
      "Phat",
    );
    expect(relay).toHaveLength(1);
    expect(relay[0]).toMatchObject({ key: "c2c_test_relay_bad", vars: { name: "Phat", detail: "pair it" } });
    expect(keys({ deep: { ok: false, detail: "the key is not the one expected" } })).toEqual(["c2c_test_deep_bad"]);
    // A relay-only test has no direct leg; a direct-only one no relay legs.
    expect(keys({ direct: null, relay: { from: { verdict: "ok" }, to: { verdict: "ok" } } })).toEqual(["c2c_test_relay_ok"]);
  });
});
