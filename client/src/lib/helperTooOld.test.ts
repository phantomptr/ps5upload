import { describe, expect, it } from "vitest";

import { powerFromCheck } from "./consolePower";
import { helperTooOldText, showsHelperTooOld } from "./helperTooOld";
import type { Translator } from "../state/lang";
import en from "../i18n/locales/en";

const tr = ((key: string, a?: unknown, b?: string) => {
  const table = en as unknown as Record<string, string>;
  return table[key] ?? (typeof a === "string" ? a : (b ?? key));
}) as Translator;

describe("an older helper is told apart from no helper (#877)", () => {
  it("only a down console whose probe found an older helper says so", () => {
    expect(showsHelperTooOld({ payloadStatus: "down", helperTooOld: true })).toBe(true);
    expect(showsHelperTooOld({ payloadStatus: "down", helperTooOld: false })).toBe(false);
    // An older engine never sends the flag: the existing "not running" text stays.
    expect(showsHelperTooOld({ payloadStatus: "down" })).toBe(false);
    // A stale flag never overrides a helper that answers now, or a first check in flight.
    expect(showsHelperTooOld({ payloadStatus: "up", helperTooOld: true })).toBe(false);
    expect(showsHelperTooOld({ payloadStatus: "unknown", helperTooOld: true })).toBe(false);
  });

  it("says an older helper runs and how to replace it, in English from en.ts", () => {
    const t = helperTooOldText(tr);
    expect(t.title).toBe("An older PS5 helper is running");
    expect(t.detail).toContain("can't use");
    expect(t.detail).toContain("Send the helper");
    expect(t.detail).toContain("update the copy it uses");
  });

  it("an older helper that answered means the console is awake", () => {
    expect(
      powerFromCheck({
        reachable: false,
        engineReachable: true,
        session: "down",
        reason: "helper_not_ava1",
        helperTooOld: true,
      } as Parameters<typeof powerFromCheck>[0]),
    ).toBe("awake");
    // Without the flag a refused helper port alone says nothing about power.
    expect(
      powerFromCheck({
        reachable: false,
        engineReachable: true,
        session: "down",
        reason: "helper_not_ava1",
      } as Parameters<typeof powerFromCheck>[0]),
    ).toBe("unknown");
  });
});
