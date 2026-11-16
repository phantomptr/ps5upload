import { describe, expect, it } from "vitest";

import type { PayloadLocalEntry } from "../api/ps5";
import { chainFileFor, createRunTokens, settleBusyRows } from "./firstRunPayloads";

const entry = (payload_id: string, version: string) =>
  ({ payload_id, version, path: `/c/${payload_id}.elf`, size: 1, mtime: 1 }) as PayloadLocalEntry;

describe("the wizard's payload files", () => {
  it("sends the file the user picked over the library's copy", () => {
    expect(chainFileFor(" /me/kstuff.elf ", [entry("kstuff", "v2")], "kstuff")).toEqual({
      path: "/me/kstuff.elf",
      from: "picked",
    });
  });

  it("falls back to the copy already in the library", () => {
    expect(chainFileFor(null, [entry("kstuff", "v1")], "kstuff")).toEqual({
      path: "/c/kstuff.elf",
      from: "library",
      version: "v1",
    });
  });

  it("has nothing to send when neither is there", () => {
    expect(chainFileFor("", [entry("smp", "v2")], "kstuff")).toBeNull();
  });
});

describe("one run of the chain at a time", () => {
  it("Cancel then Run again ends the first run for good", () => {
    const runs = createRunTokens();
    const first = runs.start();
    expect(first()).toBe(true);
    runs.cancel();
    const second = runs.start();
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it("a new run ends the one before without a Cancel", () => {
    const runs = createRunTokens();
    const first = runs.start();
    runs.start();
    expect(first()).toBe(false);
  });

  it("Cancel leaves no spinner", () => {
    const rows = [{ state: "ok" as const }, { state: "busy" as const }, { state: "idle" as const }];
    expect(settleBusyRows(rows, "idle").map((r) => r.state)).toEqual(["ok", "idle", "idle"]);
  });
});
