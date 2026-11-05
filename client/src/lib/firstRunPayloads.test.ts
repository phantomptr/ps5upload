import { describe, expect, it } from "vitest";

import type { PayloadLocalEntry } from "../api/ps5";
import { cachedCopyOfRelease, createRunTokens, settleBusyRows } from "./firstRunPayloads";

const entry = (payload_id: string, version: string) =>
  ({ payload_id, version, path: `/c/${payload_id}.elf`, size: 1, mtime: 1 }) as PayloadLocalEntry;

describe("the wizard's cached payloads", () => {
  it("uses a cached copy of the release asked for", () => {
    expect(cachedCopyOfRelease([entry("kstuff", "v2")], "kstuff", "v2")?.path).toBe("/c/kstuff.elf");
  });

  it("does not send an older cached copy as the latest", () => {
    expect(cachedCopyOfRelease([entry("kstuff", "v1")], "kstuff", "v2")).toBeNull();
  });

  it("does not trust a copy with no recorded version", () => {
    expect(cachedCopyOfRelease([entry("kstuff", "unknown")], "kstuff", "v2")).toBeNull();
  });

  it("ignores another payload's copy", () => {
    expect(cachedCopyOfRelease([entry("smp", "v2")], "kstuff", "v2")).toBeNull();
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
