import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// pkgLibrary pulls in the Tauri invoke bridge + the ps5 api at module
// load; stub both so importing the store doesn't touch a real backend.
// (Hoisted by vitest above the imports below — affects this whole file,
// but the pure titleIdFromContentId tests don't care.)
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../api/ps5", () => ({
  fsListDir: vi.fn(async () => []),
  fsDelete: vi.fn(async () => {}),
}));

import { fsDelete } from "../api/ps5";
import {
  titleIdFromContentId,
  usePkgLibrary,
  isFinishedPkg,
  type PkgEntry,
} from "./pkgLibrary";

describe("titleIdFromContentId", () => {
  it("extracts the title id from a standard ContentID", () => {
    expect(titleIdFromContentId("EP9000-CUSA00207_00-BLOODBORNE000000")).toBe(
      "CUSA00207",
    );
    expect(titleIdFromContentId("IV0002-PPSA01234_00-SOMEGAME00000000")).toBe(
      "PPSA01234",
    );
  });

  it("handles homebrew/region prefixes that still carry a title id", () => {
    expect(titleIdFromContentId("UP1234-PLAS10000_00-XYZ")).toBe("PLAS10000");
  });

  it("returns null when there is no well-formed title id", () => {
    expect(titleIdFromContentId("")).toBeNull();
    expect(titleIdFromContentId("HB0000-HOMEBREW_00-X")).toBeNull(); // not 4+5
    expect(titleIdFromContentId("HB0000-12345678_00-X")).toBeNull(); // all digits
    expect(titleIdFromContentId("justastring")).toBeNull();
    expect(titleIdFromContentId("AB-CD-EF")).toBeNull();
  });

  it("requires exactly four letters then five digits", () => {
    // FAKE00001 = FAKE (4) + 00001 (5) → valid shape.
    expect(titleIdFromContentId("HB0000-FAKE00001_00-X")).toBe("FAKE00001");
    // too few digits
    expect(titleIdFromContentId("HB0000-CUSA0020_00-X")).toBeNull();
    // lowercase letters not accepted
    expect(titleIdFromContentId("HB0000-cusa00207_00-X")).toBeNull();
  });
});

// ── Library cleanup (clearFinished / clearAll / isFinishedPkg) ──────────────

const mockedDelete = vi.mocked(fsDelete);
const HOST = "192.168.1.50";
const DIR = "/user/data/ps5upload/pkg_library";

function entry(p: Partial<PkgEntry> & { name: string }): PkgEntry {
  return {
    name: p.name,
    path: p.path ?? `${DIR}/${p.name}`,
    size: p.size ?? 1000,
    contentId: p.contentId ?? p.name.replace(/\.pkg$/, ""),
    status: p.status ?? "idle",
    title: p.title,
    titleId: p.titleId,
    bytes: p.bytes,
    totalBytes: p.totalBytes,
    lastResult: p.lastResult,
  };
}

function seed(entries: PkgEntry[]) {
  usePkgLibrary.setState({ entries, error: null });
}

describe("isFinishedPkg", () => {
  it("is true only for idle rows whose last install succeeded", () => {
    expect(
      isFinishedPkg(entry({ name: "a.pkg", lastResult: { ok: true, message: "" } })),
    ).toBe(true);
    expect(
      isFinishedPkg(entry({ name: "b.pkg", lastResult: { ok: false, message: "x" } })),
    ).toBe(false);
    expect(isFinishedPkg(entry({ name: "c.pkg" }))).toBe(false);
    expect(
      isFinishedPkg(
        entry({
          name: "d.pkg",
          status: "installing",
          lastResult: { ok: true, message: "" },
        }),
      ),
    ).toBe(false);
  });
});

describe("clearFinished", () => {
  beforeEach(() => {
    mockedDelete.mockReset().mockResolvedValue(undefined);
  });
  afterEach(() => {
    usePkgLibrary.setState({ entries: [], error: null });
  });

  it("deletes only the successfully-installed rows from the PS5", async () => {
    seed([
      entry({ name: "done1.pkg", lastResult: { ok: true, message: "Installed." } }),
      entry({ name: "pending.pkg" }),
      entry({ name: "failed.pkg", lastResult: { ok: false, message: "err" } }),
      entry({ name: "uploading.pkg", status: "uploading" }),
      entry({ name: "done2.pkg", lastResult: { ok: true, message: "Installed." } }),
    ]);

    await usePkgLibrary.getState().clearFinished(HOST);

    const names = usePkgLibrary
      .getState()
      .entries.map((e) => e.name)
      .sort();
    expect(names).toEqual(["failed.pkg", "pending.pkg", "uploading.pkg"]);
    const deleted = mockedDelete.mock.calls.map((c) => c[1]).sort();
    expect(deleted).toEqual([`${DIR}/done1.pkg`, `${DIR}/done2.pkg`]);
  });

  it("is a no-op (no deletes) when nothing is finished", async () => {
    seed([entry({ name: "pending.pkg" })]);
    await usePkgLibrary.getState().clearFinished(HOST);
    expect(mockedDelete).not.toHaveBeenCalled();
    expect(usePkgLibrary.getState().entries).toHaveLength(1);
  });

  it("restores rows whose PS5 delete failed and surfaces an error", async () => {
    mockedDelete.mockImplementation(async (_addr: string, path: string) => {
      if (path.endsWith("done2.pkg")) throw new Error("EACCES");
    });
    seed([
      entry({ name: "done1.pkg", lastResult: { ok: true, message: "Installed." } }),
      entry({ name: "done2.pkg", lastResult: { ok: true, message: "Installed." } }),
    ]);

    await usePkgLibrary.getState().clearFinished(HOST);

    const names = usePkgLibrary.getState().entries.map((e) => e.name);
    expect(names).toEqual(["done2.pkg"]);
    expect(usePkgLibrary.getState().error).toContain("Failed to delete 1");
  });

  it("does nothing without a host", async () => {
    seed([entry({ name: "done1.pkg", lastResult: { ok: true, message: "" } })]);
    await usePkgLibrary.getState().clearFinished("  ");
    expect(mockedDelete).not.toHaveBeenCalled();
    expect(usePkgLibrary.getState().entries).toHaveLength(1);
  });
});

describe("clearAll", () => {
  beforeEach(() => {
    mockedDelete.mockReset().mockResolvedValue(undefined);
  });
  afterEach(() => {
    usePkgLibrary.setState({ entries: [], error: null });
  });

  it("deletes every idle row but never an in-flight one", async () => {
    seed([
      entry({ name: "idle1.pkg" }),
      entry({ name: "idle2.pkg", lastResult: { ok: true, message: "" } }),
      entry({ name: "uploading.pkg", status: "uploading" }),
      entry({ name: "installing.pkg", status: "installing" }),
      entry({ name: "queued.pkg", status: "queued" }),
    ]);

    await usePkgLibrary.getState().clearAll(HOST);

    const names = usePkgLibrary
      .getState()
      .entries.map((e) => e.name)
      .sort();
    expect(names).toEqual(["installing.pkg", "queued.pkg", "uploading.pkg"]);
    expect(mockedDelete).toHaveBeenCalledTimes(2);
  });
});
