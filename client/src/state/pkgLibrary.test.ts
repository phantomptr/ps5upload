import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// pkgLibrary pulls in the Tauri invoke bridge + the ps5 api at module
// load; stub both so importing the store doesn't touch a real backend.
// (Hoisted by vitest above the imports below — affects this whole file,
// but the pure titleIdFromContentId tests don't care.)
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../api/ps5", () => ({
  fsListDir: vi.fn(async () => []),
  fsDelete: vi.fn(async () => {}),
  fsMkdir: vi.fn(async () => {}),
}));

import { fsDelete, fsListDir } from "../api/ps5";
import {
  titleIdFromContentId,
  pkgLibraryStore,
  evictPkgLibraryStore,
  isFinishedPkg,
  pkgInstallMayNotLaunch,
  installedLastResult,
  PKG_MAY_NOT_LAUNCH_MESSAGE,
  type PkgEntry,
} from "./pkgLibrary";

describe("per-console store registry (eviction)", () => {
  it("returns the SAME store instance for a host until evicted", () => {
    const a1 = pkgLibraryStore("192.168.50.1");
    const a2 = pkgLibraryStore("192.168.50.1:9114"); // port stripped → same key
    expect(a2).toBe(a1);
  });

  it("hands out a FRESH store after eviction (stale state can't resurface)", () => {
    const host = "192.168.50.2";
    const before = pkgLibraryStore(host);
    // Dirty the transient state that a re-listing wouldn't clear.
    before.setState({ busyNotice: "installing…" });
    expect(before.getState().busyNotice).toBe("installing…");

    evictPkgLibraryStore(host);
    const after = pkgLibraryStore(host);
    expect(after).not.toBe(before);
    expect(after.getState().busyNotice).toBeNull();
  });

  it("evicting an unknown host is a no-op (no throw)", () => {
    expect(() => evictPkgLibraryStore("203.0.113.7")).not.toThrow();
  });
});

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

// ── may-not-launch surfacing (the 2.27.x FW-12 install fix) ─────────────────

describe("pkgInstallMayNotLaunch", () => {
  it("trusts the engine's explicit may_not_launch flag", () => {
    expect(pkgInstallMayNotLaunch({ may_not_launch: true })).toBe(true);
    expect(pkgInstallMayNotLaunch({ may_not_launch: false })).toBe(false);
    // Flag wins even if register_path would say otherwise.
    expect(
      pkgInstallMayNotLaunch({ may_not_launch: false, register_path: "appinst-local" }),
    ).toBe(false);
  });

  it("falls back to register_path for older engines without the flag", () => {
    // Only the unlaunchable last-resort path warns.
    expect(pkgInstallMayNotLaunch({ register_path: "appinst-local" })).toBe(true);
    // Every launchable tier does not.
    for (const rp of ["appinst", "shellui-rpc", "intdebug", "regular", "tier0-worker", "none", ""]) {
      expect(pkgInstallMayNotLaunch({ register_path: rp })).toBe(false);
    }
    // Nothing at all (very old engine) → no warning.
    expect(pkgInstallMayNotLaunch({})).toBe(false);
  });
});

describe("installedLastResult", () => {
  it("plain green success when launchable", () => {
    expect(installedLastResult(false)).toEqual({ ok: true, message: "Installed." });
  });
  it("amber warn with re-install guidance when may not launch", () => {
    const r = installedLastResult(true);
    expect(r.ok).toBe(true);
    expect(r.warn).toBe(true);
    expect(r.message).toBe(PKG_MAY_NOT_LAUNCH_MESSAGE);
    expect(r.message).toMatch(/Package Installer/);
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
  pkgLibraryStore(HOST).setState({ entries, error: null });
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
    pkgLibraryStore(HOST).setState({ entries: [], error: null });
  });

  it("deletes only the successfully-installed rows from the PS5", async () => {
    seed([
      entry({ name: "done1.pkg", lastResult: { ok: true, message: "Installed." } }),
      entry({ name: "pending.pkg" }),
      entry({ name: "failed.pkg", lastResult: { ok: false, message: "err" } }),
      entry({ name: "uploading.pkg", status: "uploading" }),
      entry({ name: "done2.pkg", lastResult: { ok: true, message: "Installed." } }),
    ]);

    await pkgLibraryStore(HOST).getState().clearFinished(HOST);

    const names = pkgLibraryStore(HOST)
      .getState()
      .entries.map((e) => e.name)
      .sort();
    expect(names).toEqual(["failed.pkg", "pending.pkg", "uploading.pkg"]);
    const deleted = mockedDelete.mock.calls.map((c) => c[1]).sort();
    expect(deleted).toEqual([`${DIR}/done1.pkg`, `${DIR}/done2.pkg`]);
  });

  it("is a no-op (no deletes) when nothing is finished", async () => {
    seed([entry({ name: "pending.pkg" })]);
    await pkgLibraryStore(HOST).getState().clearFinished(HOST);
    expect(mockedDelete).not.toHaveBeenCalled();
    expect(pkgLibraryStore(HOST).getState().entries).toHaveLength(1);
  });

  it("restores rows whose PS5 delete failed and surfaces an error", async () => {
    mockedDelete.mockImplementation(async (_addr: string, path: string) => {
      if (path.endsWith("done2.pkg")) throw new Error("EACCES");
    });
    seed([
      entry({ name: "done1.pkg", lastResult: { ok: true, message: "Installed." } }),
      entry({ name: "done2.pkg", lastResult: { ok: true, message: "Installed." } }),
    ]);

    await pkgLibraryStore(HOST).getState().clearFinished(HOST);

    const names = pkgLibraryStore(HOST).getState().entries.map((e) => e.name);
    expect(names).toEqual(["done2.pkg"]);
    expect(pkgLibraryStore(HOST).getState().error).toContain("Failed to delete 1");
  });

  it("does nothing without a host", async () => {
    seed([entry({ name: "done1.pkg", lastResult: { ok: true, message: "" } })]);
    await pkgLibraryStore(HOST).getState().clearFinished("  ");
    expect(mockedDelete).not.toHaveBeenCalled();
    expect(pkgLibraryStore(HOST).getState().entries).toHaveLength(1);
  });
});

describe("clearAll", () => {
  beforeEach(() => {
    mockedDelete.mockReset().mockResolvedValue(undefined);
  });
  afterEach(() => {
    pkgLibraryStore(HOST).setState({ entries: [], error: null });
  });

  it("deletes every idle row but never an in-flight one", async () => {
    seed([
      entry({ name: "idle1.pkg" }),
      entry({ name: "idle2.pkg", lastResult: { ok: true, message: "" } }),
      entry({ name: "uploading.pkg", status: "uploading" }),
      entry({ name: "installing.pkg", status: "installing" }),
      entry({ name: "queued.pkg", status: "queued" }),
    ]);

    await pkgLibraryStore(HOST).getState().clearAll(HOST);

    const names = pkgLibraryStore(HOST)
      .getState()
      .entries.map((e) => e.name)
      .sort();
    expect(names).toEqual(["installing.pkg", "queued.pkg", "uploading.pkg"]);
    expect(mockedDelete).toHaveBeenCalledTimes(2);
  });
});

// ── Base + update coexistence (the #3 bug fix) ──────────────────────────────

describe("refresh — base + update coexistence and badging", () => {
  const mockedList = vi.mocked(fsListDir);
  const CID = "EP9000-CUSA00207_00-BLOODBORNE000000";
  const file = (name: string, size: number) =>
    ({ name, kind: "file", size }) as Awaited<
      ReturnType<typeof fsListDir>
    >[number];
  const dir = (name: string) =>
    ({ name, kind: "dir", size: 0 }) as Awaited<
      ReturnType<typeof fsListDir>
    >[number];

  beforeEach(() => {
    mockedList.mockReset();
    pkgLibraryStore(HOST).setState({ entries: [], error: null, loading: false });
  });
  afterEach(() => {
    pkgLibraryStore(HOST).setState({ entries: [], error: null });
  });

  it("lists a base and its same-ContentID update as two distinct, badged rows", async () => {
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d.endsWith("/updates")) return [file(`${CID}.pkg`, 200)];
      if (d.endsWith("/dlc")) return [];
      // library root: the base + the two sub-dirs (which we must NOT treat
      // as packages).
      return [file(`${CID}.pkg`, 100), dir("updates"), dir("dlc")];
    });

    await pkgLibraryStore(HOST).getState().refresh("192.168.1.50");

    const entries = pkgLibraryStore(HOST).getState().entries;
    expect(entries).toHaveLength(2);
    const base = entries.find((e) => e.category === undefined);
    const update = entries.find((e) => e.category === "gp");
    // Same ContentID...
    expect(base?.contentId).toBe(CID);
    expect(update?.contentId).toBe(CID);
    // ...but DIFFERENT paths — neither overwrites the other (the bug fix).
    expect(base?.path).not.toBe(update?.path);
    expect(base?.path.endsWith(`/${CID}.pkg`)).toBe(true);
    expect(update?.path).toContain("/updates/");
    expect(pkgLibraryStore(HOST).getState().error).toBeNull();
  });

  it("tolerates missing updates/ + dlc/ sub-dirs (ENOENT), still lists the base", async () => {
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d.endsWith("/updates") || d.endsWith("/dlc")) {
        throw new Error("fs_list_dir_opendir_errno_2");
      }
      return [file(`${CID}.pkg`, 100)];
    });

    await pkgLibraryStore(HOST).getState().refresh("192.168.1.50");

    const entries = pkgLibraryStore(HOST).getState().entries;
    expect(entries).toHaveLength(1);
    expect(entries[0].category).toBeUndefined();
    expect(pkgLibraryStore(HOST).getState().error).toBeNull();
  });

  it("surfaces a real (non-ENOENT) error on the ROOT list without wiping the list", async () => {
    pkgLibraryStore(HOST).setState({
      entries: [
        {
          name: "x.pkg",
          path: "/lib/x.pkg",
          size: 1,
          contentId: "x",
          status: "idle",
        },
      ],
    });
    mockedList.mockImplementation(async (_addr: string, _d: string) => {
      throw new Error("connection refused");
    });

    await pkgLibraryStore(HOST).getState().refresh("192.168.1.50");

    // existing list preserved, error surfaced
    expect(pkgLibraryStore(HOST).getState().entries).toHaveLength(1);
    expect(pkgLibraryStore(HOST).getState().error).toBeTruthy();
  });
});
