import { describe, expect, it } from "vitest";

import { shellErrorCode, shellErrorText, windowsBlock } from "./shellError";

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

  it("says why the engine inside a phone app is not answering (#950)", () => {
    const failed = JSON.stringify({
      code: "engine_in_app_failed",
      detail: "in-process engine failed to bind 0.0.0.0:19113: Address already in use",
    });
    expect(shellErrorText(failed, tr)).toBe(
      "The engine inside the app didn't start: in-process engine failed to bind 0.0.0.0:19113: Address already in use",
    );
    const blocked = shellErrorText(
      JSON.stringify({ code: "engine_in_app_blocked", detail: "No route to host (os error 65)" }),
      tr,
    );
    expect(blocked).toContain("this device blocked the app's connection to it (No route to host (os error 65))");
    expect(blocked).toContain("Try again");
    expect(shellErrorCode(failed)).toBe("engine_in_app_failed");
  });

  it("says when a rate limit lifts, or that it does not know", () => {
    const at = 1_800_000_000;
    expect(shellErrorText(JSON.stringify({ code: "update_rate_limited", reset_at: at }), tr)).toContain(
      new Date(at * 1000).toLocaleTimeString(),
    );
    expect(shellErrorText('{"code":"update_rate_limited","reset_at":0}', tr)).toContain("in a while");
  });

  it("says when Windows refused to run the engine, and keeps the raw error (#914)", () => {
    const sac =
      "engine failed to start: spawning engine: C:\\Users\\x\\AppData\\Local\\com.phantomptr.ps5upload\\engine\\ps5upload-engine.exe: An Application Control policy has blocked this file. (os error 4551)";
    const text = shellErrorText(sac, tr);
    expect(text).toContain("Smart App Control");
    expect(text).toContain("administrator");
    expect(text).toContain("(os error 4551)");
    expect(windowsBlock(sac)).toBe("app_control");
    expect(windowsBlock("spawning engine: x: This program is blocked by group policy. (os error 1260)")).toBe(
      "app_control",
    );
    expect(windowsBlock("spawning engine: x: Operation did not complete successfully because the file contains a virus or potentially unwanted software. (os error 225)")).toBe("virus");
    expect(shellErrorText("spawning engine: x (os error 225)", tr)).toContain("quarantine");
    // Other numbers are not a block, and the RAR helper's refusal is already worded by the engine.
    expect(windowsBlock("spawning engine: x (os error 2250)")).toBeNull();
    expect(windowsBlock("Access is denied. (os error 5)")).toBeNull();
    expect(windowsBlock("RAR extraction is blocked: ... ps5upload-unrar ... (os error 4551)")).toBeNull();
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
