import { beforeEach, describe, expect, it, vi } from "vitest";

const data = new Map<string, string>();
vi.stubGlobal("window", {
  localStorage: {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => data.set(key, value),
    removeItem: (key: string) => data.delete(key),
  },
});

describe("backport records", () => {
  beforeEach(() => data.clear());

  it("persists records per console and title", async () => {
    vi.resetModules();
    const { saveBackportRecord, loadBackportRecord } = await import("./backportRecords");
    saveBackportRecord("10.0.0.2", {
      titleId: "PPSA25411", setId: "PPSA32785", targetSource: "/games/t",
      copiedPaths: ["/games/t/fakelib/a.sprx"], replaced: [], stashDir: "/data/ps5upload/backport/PPSA25411", complete: true,
    });
    expect(loadBackportRecord("10.0.0.2", "PPSA25411")?.setId).toBe("PPSA32785");
    expect(loadBackportRecord("10.0.0.2", "PPSA25411")?.complete).toBe(true);
    expect(loadBackportRecord("10.0.0.3", "PPSA25411")).toBeNull();
  });

  it("fails closed on malformed persisted data", async () => {
    data.set("ps5upload.backports.v3", "{bad json");
    vi.resetModules();
    const { loadBackportRecord } = await import("./backportRecords");
    expect(loadBackportRecord("10.0.0.2", "PPSA25411")).toBeNull();
  });

  it("rejects legacy records that cannot prove the apply completed", async () => {
    data.set("ps5upload.backports.v3", JSON.stringify({
      "10.0.0.2:PPSA25411": {
        titleId: "PPSA25411", setId: "PPSA32785", targetSource: "/games/t", copiedPaths: [],
        replaced: [], stashDir: "/data/ps5upload/backport/PPSA25411",
      },
    }));
    vi.resetModules();
    const { loadBackportRecord } = await import("./backportRecords");
    expect(loadBackportRecord("10.0.0.2", "PPSA25411")).toBeNull();
  });

  it("rejects malformed library entries and copied paths outside the target", async () => {
    for (const record of [
      {
        titleId: "PPSA25411", setId: "PPSA32785", targetSource: "/games/t",
        copiedPaths: ["/data/unrelated.sprx"], replaced: [],
        stashDir: "/data/ps5upload/backport/PPSA25411", complete: false,
      },
      {
        titleId: "PPSA25411", setId: "PPSA32785", targetSource: "/games/t",
        copiedPaths: [], replaced: [{ name: "../escape.sprx", size: 1 }],
        stashDir: "/data/ps5upload/backport/PPSA25411", complete: false,
      },
    ]) {
      data.set("ps5upload.backports.v3", JSON.stringify({ "10.0.0.2:PPSA25411": record }));
      vi.resetModules();
      const { loadBackportRecord } = await import("./backportRecords");
      expect(loadBackportRecord("10.0.0.2", "PPSA25411")).toBeNull();
    }
  });
});
