import { describe, expect, it, vi } from "vitest";
import {
  applyPackInstall,
  planPackInstall,
  packFits,
  packRequiresBytes,
  undoPackInstall,
  PackInstallError,
  type BackportPack,
  type PackInstallRecord,
  type PackTransport,
} from "./backportPack";

const title = (titleId: string, source = `/data/homebrew/${titleId}-app`) => ({
  titleId,
  titleName: titleId,
  origin: "registered" as const,
  imageBacked: false,
  source,
  system: false,
});

/** Shaped like the real PPSA19534 pack: nine libraries, a pre-patched eboot,
 *  two sce_module replacements, and engine .prx files that are NOT libraries. */
const pack = (over: Partial<BackportPack> = {}): BackportPack => ({
  isPack: true,
  titleIdHint: "PPSA19534",
  libraries: [
    { relPath: "fakelib/libSceAgc.sprx", size: 326617 },
    { relPath: "fakelib/libkernel.sprx", size: 498384 },
  ],
  eboot: { relPath: "eboot.bin", size: 256107618 },
  sceModules: [{ relPath: "sce_module/libc.prx", size: 1875018 }],
  gamePrx: [{ relPath: "prx/akdelay.prx", size: 10653 }],
  sceSys: [{ relPath: "sce_sys/about/right.sprx", size: 12512 }],
  other: [{ relPath: "Engine.Render.Core2.PlatformPs5.retail.prx", size: 4506976 }],
  totalBytes: 258830802,
  ...over,
});

const nothingThere = {
  fakelib: [], sceModule: [], gamePrx: [], sceSys: [], eboot: false,
};

function recorder() {
  const calls: string[] = [];
  const transport: PackTransport = {
    mkdirConsole: async (p) => { calls.push(`mkdir ${p}`); },
    copyConsole: async (f, t) => { calls.push(`copy ${f} -> ${t}`); },
    uploadHost: async (f, t) => { calls.push(`upload ${f} -> ${t}`); },
    remove: async (p) => { calls.push(`remove ${p}`); },
  };
  return { calls, transport };
}

describe("planPackInstall", () => {
  it("maps each pack file to its place inside the title", () => {
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    expect(plan.copies.map((c) => `${c.from} -> ${c.to}`)).toEqual([
      "/dl/pack/fakelib/libSceAgc.sprx -> /data/homebrew/PPSA19534-app/fakelib/libSceAgc.sprx",
      "/dl/pack/fakelib/libkernel.sprx -> /data/homebrew/PPSA19534-app/fakelib/libkernel.sprx",
      "/dl/pack/sce_module/libc.prx -> /data/homebrew/PPSA19534-app/sce_module/libc.prx",
      "/dl/pack/prx/akdelay.prx -> /data/homebrew/PPSA19534-app/prx/akdelay.prx",
      "/dl/pack/sce_sys/about/right.sprx -> /data/homebrew/PPSA19534-app/sce_sys/about/right.sprx",
      "/dl/pack/eboot.bin -> /data/homebrew/PPSA19534-app/eboot.bin",
    ]);
  });

  it("puts the eboot last so a failure leaves the title still runnable", () => {
    // Swapping the eboot first and then failing on a library gives a game that
    // launches into a crash; this order leaves it exactly as it was.
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    expect(plan.copies[plan.copies.length - 1].to).toMatch(/eboot\.bin$/);
  });

  it("never treats non-library pack files as libraries", () => {
    // The engine classifies, but the plan must not smuggle `other` back in —
    // this is the bug that made the old importer build a poisoned set.
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    expect(plan.copies.some((c) => c.from.includes("Engine.Render"))).toBe(false);
  });

  it("stashes only files that are actually there", () => {
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", {
      fakelib: ["libkernel.sprx"],
      sceModule: [],
      gamePrx: [],
      sceSys: [],
      eboot: true,
    });
    expect(plan.stashed).toEqual([
      {
        live: "/data/homebrew/PPSA19534-app/fakelib/libkernel.sprx",
        stash: "/data/ps5upload/backport/PPSA19534/fakelib__libkernel.sprx",
      },
      {
        live: "/data/homebrew/PPSA19534-app/eboot.bin",
        stash: "/data/ps5upload/backport/PPSA19534/eboot.bin",
      },
    ]);
  });

  it("keeps fakelib and sce_module backups apart when names collide", () => {
    // Both folders can hold a `libc.prx`; a flat stash would have the second
    // overwrite the first and make undo restore the wrong bytes.
    const plan = planPackInstall(
      pack({
        libraries: [{ relPath: "fakelib/libc.prx", size: 1 }],
        sceModules: [{ relPath: "sce_module/libc.prx", size: 2 }],
        eboot: null,
      }),
      title("PPSA19534"),
      "/dl/pack",
      { fakelib: ["libc.prx"], sceModule: ["libc.prx"], gamePrx: [], sceSys: [], eboot: false },
    );
    const stashes = plan.stashed.map((s) => s.stash);
    expect(new Set(stashes).size).toBe(2);
  });

  it("installs the game's prx/ plugins — the bug a second pack exposed", () => {
    // PPSA29343 ships 26 Wwise plugins under prx/. The first classifier, built
    // from a pack that had none, put every one in `other` and never wrote them.
    const plan = planPackInstall(
      pack({
        gamePrx: [
          { relPath: "prx/akdelay.prx", size: 1 },
          { relPath: "prx/masteringsuite.prx", size: 2 },
        ],
      }),
      title("PPSA19534"), "/dl/pack", nothingThere,
    );
    const prx = plan.copies.filter((c) => c.to.includes("/prx/"));
    expect(prx.map((c) => c.to)).toEqual([
      "/data/homebrew/PPSA19534-app/prx/akdelay.prx",
      "/data/homebrew/PPSA19534-app/prx/masteringsuite.prx",
    ]);
  });

  it("flags a pack that names a different title", () => {
    const plan = planPackInstall(pack(), title("PPSA25411"), "/dl/pack", nothingThere);
    expect(plan.titleMismatch).toBe(true);
    // Still planned — the folder name is a string, not an authority. The UI
    // confirms rather than refuses.
    expect(plan.copies.length).toBeGreaterThan(0);
  });

  it("does not flag a mismatch when the pack names no title", () => {
    const plan = planPackInstall(
      pack({ titleIdHint: null }), title("PPSA25411"), "/dl/pack", nothingThere,
    );
    expect(plan.titleMismatch).toBe(false);
  });

  it("refuses a folder that is not a pack", () => {
    expect(() =>
      planPackInstall(pack({ isPack: false }), title("PPSA19534"), "/dl/x", nothingThere),
    ).toThrow(/not a backport pack/);
  });

  it("refuses a title with no source folder", () => {
    expect(() =>
      planPackInstall(pack(), title("PPSA19534", ""), "/dl/pack", nothingThere),
    ).toThrow(/no source folder/);
  });

  it("refuses a pack path that escapes its root", () => {
    // Defence in depth: the engine already rejects these, but a browser build
    // may be talking to an older engine.
    expect(() =>
      planPackInstall(
        pack({ libraries: [{ relPath: "../../etc/passwd", size: 1 }] }),
        title("PPSA19534"), "/dl/pack", nothingThere,
      ),
    ).toThrow(/Unsafe path/);
  });
});

describe("applyPackInstall", () => {
  it("backs up every original before writing anything", async () => {
    const { calls, transport } = recorder();
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", {
      fakelib: ["libkernel.sprx"], sceModule: [], gamePrx: [], sceSys: [], eboot: true,
    });
    await applyPackInstall(plan, transport);
    const firstUpload = calls.findIndex((c) => c.startsWith("upload"));
    const lastCopy = calls.map((c) => c.startsWith("copy")).lastIndexOf(true);
    expect(lastCopy).toBeLessThan(firstUpload);
  });

  it("records what landed so undo can reverse exactly that", async () => {
    const { transport } = recorder();
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    const record = await applyPackInstall(plan, transport);
    expect(record.complete).toBe(true);
    expect(record.copiedPaths).toHaveLength(6);
    expect(record.stashed).toEqual([]);
  });

  it("returns a usable record when it fails partway", async () => {
    const { transport } = recorder();
    let n = 0;
    transport.uploadHost = async () => {
      if (++n === 2) throw new Error("console full");
    };
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    // A half-installed title must still be undoable — that is the whole point
    // of carrying the record on the error.
    await expect(applyPackInstall(plan, transport)).rejects.toThrow(PackInstallError);
    try {
      await applyPackInstall(plan, transport);
    } catch (e) {
      const rec = (e as PackInstallError).record;
      expect(rec.complete).toBe(false);
      expect(rec.copiedPaths.length).toBeLessThan(6);
    }
  });

  it("never patches the SDK", async () => {
    // The pack eboot is already at the backport pair; patching it would
    // rewrite a correct field and invalidate the signature it shipped with.
    const { calls, transport } = recorder();
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    await applyPackInstall(plan, transport);
    expect(calls.some((c) => /patch|sdk/i.test(c))).toBe(false);
  });
});

describe("undoPackInstall", () => {
  const record = (over: Partial<PackInstallRecord> = {}): PackInstallRecord => ({
    titleId: "PPSA19534",
    targetSource: "/data/homebrew/PPSA19534-app",
    packRoot: "/dl/pack",
    copiedPaths: [
      "/data/homebrew/PPSA19534-app/fakelib/libSceAgc.sprx",
      "/data/homebrew/PPSA19534-app/eboot.bin",
    ],
    stashed: [
      {
        live: "/data/homebrew/PPSA19534-app/eboot.bin",
        stash: "/data/ps5upload/backport/PPSA19534/eboot.bin",
      },
    ],
    stashDir: "/data/ps5upload/backport/PPSA19534",
    complete: true,
    ...over,
  });

  it("removes additions and restores what it displaced", async () => {
    const { calls, transport } = recorder();
    await undoPackInstall(record(), transport);
    expect(calls).toContain("remove /data/homebrew/PPSA19534-app/fakelib/libSceAgc.sprx");
    expect(calls).toContain(
      "copy /data/ps5upload/backport/PPSA19534/eboot.bin -> /data/homebrew/PPSA19534-app/eboot.bin",
    );
  });

  it("restores the original eboot rather than deleting it", async () => {
    // Deleting a stashed path would leave the title with NO eboot at all.
    const { calls, transport } = recorder();
    await undoPackInstall(record(), transport);
    const removedThenRestored = calls.filter((c) => c.includes("eboot.bin"));
    expect(removedThenRestored[removedThenRestored.length - 1]).toMatch(/^copy /);
  });

  it("survives a stash folder that will not delete", async () => {
    const { transport } = recorder();
    transport.remove = vi.fn(async (p: string) => {
      if (p === "/data/ps5upload/backport/PPSA19534") throw new Error("busy");
    });
    await expect(undoPackInstall(record(), transport)).resolves.toBeUndefined();
  });
});

describe("free-space preflight", () => {
  it("charges for the backup copy as well as the new file", () => {
    // The stash is a copy, not a move: replacing a 256 MB eboot needs room
    // for two of them at once.
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", {
      fakelib: [], sceModule: [], gamePrx: [], sceSys: [], eboot: true,
    });
    expect(packRequiresBytes(plan)).toBe(plan.totalBytes + 256107618);
  });

  it("needs only the written bytes when nothing is displaced", () => {
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    expect(packRequiresBytes(plan)).toBe(plan.totalBytes);
  });

  it("never blocks when free space could not be read", () => {
    // A check that cannot run must not stop the user doing the thing.
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    expect(packFits(plan, null)).toBe(true);
  });

  it("fits exactly at the boundary, and not one byte below", () => {
    const plan = planPackInstall(pack(), title("PPSA19534"), "/dl/pack", nothingThere);
    const need = packRequiresBytes(plan);
    expect(packFits(plan, need)).toBe(true);
    expect(packFits(plan, need - 1)).toBe(false);
  });
});
