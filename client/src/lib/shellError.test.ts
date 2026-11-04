import { describe, expect, it } from "vitest";

import { shellErrorCode, shellErrorText } from "./shellError";

// What `useTr()` does with a key the catalogue has not got: the fallback, filled.
const tr = (_key: string, vars: Record<string, string | number> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (m, k: string) => (vars && k in vars ? String(vars[k]) : m));

describe("shellErrorText", () => {
  it("words a coded failure with its facts", () => {
    expect(shellErrorText('{"code":"update_http","status":403}', tr)).toBe(
      "The update server answered with error 403. Try again later.",
    );
    expect(shellErrorText('{"code":"update_no_bundle"}', tr)).toBe("This release has no download for your system.");
  });

  it("uses the words of the active language", () => {
    const fr = (key: string, vars: Record<string, string | number> | undefined) =>
      key === "update_err_http" ? `Erreur ${vars?.status}` : key;
    expect(shellErrorText('{"code":"update_http","status":500}', fr)).toBe("Erreur 500");
  });

  it("fills placeholders even from a translator that leaves them", () => {
    // The stores' non-hook helper returns a fallback as it is.
    const plain = (_k: string, _v: unknown, fallback: string) => fallback;
    expect(shellErrorText('{"code":"update_disk","detail":"disk full"}', plain)).toBe(
      "Couldn't save the download: disk full",
    );
  });

  it("gives the likely causes of the platform the engine failed on", () => {
    const text = (hint: string) =>
      shellErrorText(
        JSON.stringify({ code: "engine_not_ready", seconds: 30, hint, error: "refused", log: "/x/engine.log" }),
        tr,
      );
    expect(text("macos")).toContain("Little Snitch");
    expect(text("macos")).not.toContain("SmartScreen");
    expect(text("windows")).toContain("SmartScreen");
    expect(text("other")).toContain("within 30 seconds");
    expect(text("other")).toContain("/x/engine.log");
  });

  it("says when a rate limit lifts, or that it does not know", () => {
    const at = 1_800_000_000;
    expect(shellErrorText(JSON.stringify({ code: "update_rate_limited", reset_at: at }), tr)).toContain(
      new Date(at * 1000).toLocaleTimeString(),
    );
    expect(shellErrorText('{"code":"update_rate_limited","reset_at":0}', tr)).toContain("in a while");
  });

  it("shows anything else as it is", () => {
    for (const raw of [
      "engine HTTP 502: payload rejected",
      "",
      '{"code":"a_code_this_build_does_not_know"}',
      '{"error":"not ours"}',
      "{not json",
    ]) {
      expect(shellErrorText(raw, tr)).toBe(raw);
    }
  });
});

describe("shellErrorCode", () => {
  it("reads the code of a coded failure only", () => {
    expect(shellErrorCode('{"code":"update_cancelled"}')).toBe("update_cancelled");
    expect(shellErrorCode("update_cancelled")).toBeNull();
    expect(shellErrorCode('{"error":"x"}')).toBeNull();
  });
});
