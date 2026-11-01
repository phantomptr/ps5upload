import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/links", () => ({
  rarPackages: vi.fn(),
  pkgConsoleProbe: vi.fn(),
}));
vi.mock("../api/ps5", async (orig) => ({
  volumeAllocatableBytes: (await orig<typeof import("../api/ps5")>()).volumeAllocatableBytes,
  volumeLikelyFitsBytes: (await orig<typeof import("../api/ps5")>()).volumeLikelyFitsBytes,
  volumeForPath: (await orig<typeof import("../api/ps5")>()).volumeForPath,
  fetchVolumes: vi.fn(async () => []),
  fsMkdir: vi.fn(async () => {}),
  fsDelete: vi.fn(async () => {}),
  fsListDir: vi.fn(async () => {
    throw new Error("missing");
  }),
  jobStatus: vi.fn(),
  startTransferRar: vi.fn(async () => "job-1"),
  startTransferZip: vi.fn(async () => "job-z"),
  startTransfer7z: vi.fn(async () => "job-7"),
}));
const installFromConsolePath = vi.fn();
vi.mock("./pkgLibrary", () => ({
  pkgLibraryStore: () => ({ getState: () => ({ installFromConsolePath }) }),
}));

import { pkgConsoleProbe, rarPackages } from "../api/links";
import {
  fetchVolumes,
  fsDelete,
  fsListDir,
  jobStatus,
  startTransfer7z,
  startTransferRar,
  startTransferZip,
  type Volume,
} from "../api/ps5";
import {
  archiveFolderName,
  archiveKindOf,
  archiveNeedBytes,
  archiveSpaceCheck,
  findArchiveLeftovers,
  isArchiveFolderName,
  leftoverArchiveFolders,
  shouldRemoveUnpacked,
  volumeForConsoleDir,
  installRarPackages,
  isArchiveFirstVolume,
  isRarFirstVolume,
  orderForInstall,
  siblingsOf,
} from "./rarPackages";
import { splitPkgBytes } from "../lib/splitPkgSize";

const HOST = "10.0.0.5";

function probeAs(map: Record<string, { category: string; title_id: string; app_ver?: string }>) {
  vi.mocked(pkgConsoleProbe).mockImplementation(async (_h, path) => {
    const hit = Object.entries(map).find(([k]) => path.endsWith(k));
    if (!hit) throw new Error("no header");
    return {
      total_size: 1,
      filename: "",
      content_id: "",
      title: "",
      platform: "",
      package_type: "",
      app_ver: "",
      ...hit[1],
    };
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(startTransferRar).mockResolvedValue("job-1");
  vi.mocked(jobStatus).mockResolvedValue({ status: "done" } as never);
  installFromConsolePath.mockResolvedValue({ ok: true });
  vi.mocked(fetchVolumes).mockResolvedValue([]);
  vi.mocked(fsDelete).mockResolvedValue(undefined);
  vi.mocked(fsListDir).mockRejectedValue(new Error("missing"));
});

const vol = (path: string, free: number, extra: Partial<Volume> = {}): Volume => ({
  path,
  fs_type: "ufs",
  total_bytes: free * 2,
  free_bytes: free,
  writable: true,
  ...extra,
});

describe("helpers", () => {
  it("knows the first volume of a RAR set", () => {
    expect(isRarFirstVolume("/a/Game.rar")).toBe(true);
    expect(isRarFirstVolume("C:\\a\\Game.part1.rar")).toBe(true);
    expect(isRarFirstVolume("Game.part01.rar")).toBe(true);
    expect(isRarFirstVolume("Game.part2.rar")).toBe(false);
    expect(isRarFirstVolume("Game.zip")).toBe(false);
  });

  it("names the console folder after the archive, deterministically", () => {
    expect(archiveFolderName("/x/My Game [v1].part1.rar")).toBe("rar_My_Game_v1");
    expect(archiveFolderName("/x/My Game [v1].part1.rar")).toBe(archiveFolderName("/y/My Game [v1].rar"));
    expect(archiveFolderName("..rar")).toBe("rar_archive");
  });

  it("orders base, then patch (older first), then DLC", () => {
    const e = (entry: string, category: string, appVer = "", titleId = "T1") => ({
      entry,
      category,
      appVer,
      titleId,
    });
    const order = orderForInstall([
      e("dlc.pkg", "ac"),
      e("p2.pkg", "gp", "01.10"),
      e("base.pkg", "gd"),
      e("p1.pkg", "gp", "01.02"),
    ]).map((x) => x.entry);
    expect(order).toEqual(["base.pkg", "p1.pkg", "p2.pkg", "dlc.pkg"]);
  });
});

describe("installRarPackages", () => {
  it("unpacks only packages, then installs the base before its patch", async () => {
    vi.mocked(rarPackages).mockResolvedValue([
      { path: "Update/Patch.pkg", size: 10 },
      { path: "Game/Base.pkg", size: 20 },
    ]);
    probeAs({
      "Update/Patch.pkg": { category: "gp", title_id: "PPSA1", app_ver: "01.02" },
      "Game/Base.pkg": { category: "gd", title_id: "PPSA1" },
    });
    const r = await installRarPackages({ host: HOST, archivePath: "/x/Multi.rar" });
    expect(r.ok).toBe(true);
    // The unpack asked for the package allow-list, into the console's package folder.
    const call = vi.mocked(startTransferRar).mock.calls[0];
    expect(call[1]).toBe("/user/data/ps5upload/pkg_library/rar_Multi");
    expect(call[5]).toEqual(["!*.pkg"]);
    expect(installFromConsolePath.mock.calls.map((c) => c[0])).toEqual([
      "/user/data/ps5upload/pkg_library/rar_Multi/Game/Base.pkg",
      "/user/data/ps5upload/pkg_library/rar_Multi/Update/Patch.pkg",
    ]);
    expect(r.outcomes.map((o) => o.status)).toEqual(["installed", "installed"]);
    // Each queue row is sized from the archive listing.
    expect(installFromConsolePath.mock.calls.map((c) => c[2])).toEqual([{ sizeBytes: 20 }, { sizeBytes: 10 }]);
    // Every package installed: the unpacked folder is deleted, once, after the last install.
    expect(vi.mocked(fsDelete).mock.calls.map((c) => c[1])).toEqual([
      "/user/data/ps5upload/pkg_library/rar_Multi",
    ]);
    expect(vi.mocked(fsDelete).mock.invocationCallOrder[0]).toBeGreaterThan(
      Math.max(...installFromConsolePath.mock.invocationCallOrder),
    );
    expect(r.removed).toBe(true);
  });

  it("sizes a split set's lead with its parts from the same archive folder", () => {
    const all = [
      { path: "G/Big_0.pkg", size: 100 },
      { path: "G/Big_1.pkg", size: 50 },
      { path: "Other/Big_1.pkg", size: 999 },
      { path: "G/sub/Big_2.pkg", size: 999 },
    ];
    const sibs = siblingsOf("G/Big_0.pkg", all);
    expect(sibs).toEqual([{ name: "Big_1.pkg", size: 50 }]);
    expect(splitPkgBytes("Big_0.pkg", 100, sibs)).toBe(150);
  });

  it("skips a patch whose base failed, and leaves every file in place", async () => {
    vi.mocked(rarPackages).mockResolvedValue([
      { path: "b.pkg", size: 1 },
      { path: "p.pkg", size: 1 },
      { path: "other.pkg", size: 1 },
    ]);
    probeAs({
      "b.pkg": { category: "gd", title_id: "PPSA1" },
      "p.pkg": { category: "gp", title_id: "PPSA1", app_ver: "01.01" },
      "other.pkg": { category: "gd", title_id: "PPSA2" },
    });
    installFromConsolePath.mockImplementation(async (path: string) =>
      path.endsWith("/b.pkg") ? { ok: false, message: "Sony refused it." } : { ok: true },
    );
    const r = await installRarPackages({ host: HOST, archivePath: "/x/M.rar" });
    expect(r.ok).toBe(false);
    // The patch never reached the installer; the unrelated base still did.
    expect(installFromConsolePath.mock.calls.map((c) => c[0])).toEqual([
      "/user/data/ps5upload/pkg_library/rar_M/b.pkg",
      "/user/data/ps5upload/pkg_library/rar_M/other.pkg",
    ]);
    const by = Object.fromEntries(r.outcomes.map((o) => [o.entry, o]));
    expect(by["b.pkg"].status).toBe("failed");
    expect(by["b.pkg"].message).toContain("left at");
    expect(by["p.pkg"].status).toBe("skipped");
    expect(by["other.pkg"].status).toBe("installed");
    expect(r.message).toContain("still in /user/data/ps5upload/pkg_library/rar_M");
    expect(fsDelete).not.toHaveBeenCalled();
  });

  it("does not install a file that is not a readable package", async () => {
    vi.mocked(rarPackages).mockResolvedValue([{ path: "bad.pkg", size: 1 }]);
    probeAs({});
    const r = await installRarPackages({ host: HOST, archivePath: "/x/M.rar" });
    expect(r.ok).toBe(false);
    expect(installFromConsolePath).not.toHaveBeenCalled();
    expect(r.outcomes[0].status).toBe("failed");
  });

  it("asks for the password when the archive needs one, before unpacking anything", async () => {
    vi.mocked(rarPackages).mockRejectedValue(new Error("rar_password_required"));
    const r = await installRarPackages({ host: HOST, archivePath: "/x/M.rar" });
    expect(r.ok).toBe(false);
    expect(r.password).toBe("required");
    expect(startTransferRar).not.toHaveBeenCalled();
  });

  it("reports an archive with no packages", async () => {
    vi.mocked(rarPackages).mockResolvedValue([]);
    const r = await installRarPackages({ host: HOST, archivePath: "/x/M.rar" });
    expect(r.ok).toBe(false);
    expect(r.message).toContain("no .pkg");
    expect(startTransferRar).not.toHaveBeenCalled();
  });

  it("keeps the unpacked files and installs nothing when the unpack fails", async () => {
    vi.mocked(rarPackages).mockResolvedValue([{ path: "a.pkg", size: 1 }]);
    vi.mocked(jobStatus).mockResolvedValue({
      status: "failed",
      error: "out of space",
      error_reason: "insufficient_space",
    } as never);
    const r = await installRarPackages({ host: HOST, archivePath: "/x/M.rar" });
    expect(r.ok).toBe(false);
    expect(r.message).toBe("out of space");
    expect(installFromConsolePath).not.toHaveBeenCalled();
  });
});

describe("space and clean-up", () => {
  it("adds up the packages' sizes", () => {
    expect(archiveNeedBytes([{ path: "a", size: 5 }, { path: "b", size: 7 }, { path: "c", size: -1 }])).toBe(12);
  });

  it("finds internal storage as /user or /data, and an external drive by prefix", () => {
    const d = "/user/data/ps5upload/pkg_library/rar_X";
    expect(volumeForConsoleDir([vol("/data", 1), vol("/", 9)], d)?.path).toBe("/data");
    expect(volumeForConsoleDir([vol("/user", 2), vol("/data", 1)], d)?.path).toBe("/user");
    expect(volumeForConsoleDir([vol("/mnt/ext0", 3)], "/mnt/ext0/ps5upload/pkg_library/zip_Y")?.path).toBe("/mnt/ext0");
    // Only the root would match: unknown, not a guess.
    expect(volumeForConsoleDir([vol("/", 9)], "/mnt/usb0/x")).toBeNull();
  });

  it("refuses only what certainly cannot fit; past the estimate it warns", () => {
    const d = "/user/data/ps5upload/pkg_library/rar_X";
    const v = [vol("/user", 100, { allocatable_bytes: 100, likely_fits_bytes: 80 })];
    expect(archiveSpaceCheck(v, d, 50).kind).toBe("fits");
    expect(archiveSpaceCheck(v, d, 90).kind).toBe("tight");
    expect(archiveSpaceCheck(v, d, 100).kind).toBe("tight");
    expect(archiveSpaceCheck(v, d, 101)).toEqual({ kind: "no_room", need: 101, room: 100, drive: "/user" });
    // What an earlier run already unpacked there is not counted twice.
    expect(archiveSpaceCheck(v, d, 150, 60)).toMatchObject({ kind: "tight", need: 90 });
    // Unknown free space never refuses.
    expect(archiveSpaceCheck(null, d, 1e15).kind).toBe("unknown");
    expect(archiveSpaceCheck([], d, 1e15).kind).toBe("unknown");
  });

  it("refuses a run that cannot fit before unpacking anything", async () => {
    vi.mocked(rarPackages).mockResolvedValue([{ path: "a.pkg", size: 500 }]);
    vi.mocked(fetchVolumes).mockResolvedValue([vol("/user", 100)]);
    const r = await installRarPackages({ host: HOST, archivePath: "/x/M.rar" });
    expect(r.ok).toBe(false);
    expect(r.space?.kind).toBe("no_room");
    expect(r.dest).toBe("/user/data/ps5upload/pkg_library/rar_M");
    expect(startTransferRar).not.toHaveBeenCalled();
  });

  it("removes the folder only when every package installed", () => {
    expect(shouldRemoveUnpacked([])).toBe(false);
    expect(shouldRemoveUnpacked([{ status: "installed" }, { status: "installed" }])).toBe(true);
    expect(shouldRemoveUnpacked([{ status: "installed" }, { status: "failed" }])).toBe(false);
    expect(shouldRemoveUnpacked([{ status: "installed" }, { status: "skipped" }])).toBe(false);
  });

  it("still reports the installs when deleting the folder fails", async () => {
    vi.mocked(rarPackages).mockResolvedValue([{ path: "a.pkg", size: 1 }]);
    probeAs({ "a.pkg": { category: "gd", title_id: "PPSA1" } });
    vi.mocked(fsDelete).mockRejectedValue(new Error("fs_delete_failed"));
    vi.useFakeTimers();
    const p = installRarPackages({ host: HOST, archivePath: "/x/M.rar" });
    await vi.runAllTimersAsync();
    const r = await p;
    vi.useRealTimers();
    expect(r.ok).toBe(true);
    expect(r.removed).toBeUndefined();
    expect(r.removeError).toContain("fs_delete_failed");
    expect(r.message).toContain("/user/data/ps5upload/pkg_library/rar_M");
    expect(fsDelete).toHaveBeenCalledTimes(3);
  });

  it("knows its own folders and skips those in use", () => {
    expect(isArchiveFolderName(archiveFolderName("/x/Game.part1.rar"))).toBe(true);
    expect(isArchiveFolderName("zip_A")).toBe(true);
    expect(isArchiveFolderName("7z_A")).toBe(true);
    expect(isArchiveFolderName("Game.pkg")).toBe(false);
    expect(isArchiveFolderName("updates")).toBe(false);
    const entries = [
      { name: "rar_A", kind: "dir" },
      { name: "rar_B", kind: "dir" },
      { name: "rar_C.pkg", kind: "file" },
      { name: "dlc", kind: "dir" },
    ];
    expect(leftoverArchiveFolders("/L", entries, (p) => p === "/L/rar_B")).toEqual(["/L/rar_A"]);
  });

  it("finds leftovers on every package library, with their sizes", async () => {
    vi.mocked(fetchVolumes).mockResolvedValue([vol("/user", 100), vol("/mnt/ext0", 100)]);
    vi.mocked(fsListDir).mockImplementation(async (_a, path) => {
      if (path === "/user/data/ps5upload/pkg_library")
        return [
          { name: "rar_A", kind: "dir", size: 0 },
          { name: "x.pkg", kind: "file", size: 9 },
        ];
      if (path === "/user/data/ps5upload/pkg_library/rar_A")
        return [
          { name: "a.pkg", kind: "file", size: 5 },
          { name: "sub", kind: "dir", size: 0 },
        ];
      if (path === "/user/data/ps5upload/pkg_library/rar_A/sub") return [{ name: "b.pkg", kind: "file", size: 7 }];
      throw new Error("missing");
    });
    expect(await findArchiveLeftovers(HOST, () => false)).toEqual([
      { path: "/user/data/ps5upload/pkg_library/rar_A", bytes: 12 },
    ]);
  });
});

describe("archives of any kind", () => {
  it("tells a ZIP, a 7z and a RAR by name", () => {
    expect(archiveKindOf("/x/Game.ZIP")).toBe("zip");
    expect(archiveKindOf("C:\\x\\game.7z")).toBe("7z");
    expect(archiveKindOf("/x/game.part01.rar")).toBe("rar");
    expect(archiveKindOf("/x/game.pkg")).toBeNull();
    // A split 7z is not something the engine reads.
    expect(archiveKindOf("/x/game.7z.001")).toBeNull();
  });

  it("only a RAR has parts to choose between", () => {
    expect(isArchiveFirstVolume("/x/a.zip")).toBe(true);
    expect(isArchiveFirstVolume("/x/a.7z")).toBe(true);
    expect(isArchiveFirstVolume("/x/a.part1.rar")).toBe(true);
    expect(isArchiveFirstVolume("/x/a.part2.rar")).toBe(false);
    expect(isArchiveFirstVolume("/x/a.pkg")).toBe(false);
  });

  it("names the console folder after the archive and its kind", () => {
    expect(archiveFolderName("/x/My Game.zip")).toBe("zip_My_Game");
    expect(archiveFolderName("/x/My Game.7z")).toBe("7z_My_Game");
    expect(archiveFolderName("/x/Multi.part01.rar")).toBe("rar_Multi");
  });

  it("unpacks a ZIP's packages with the ZIP reader, and a 7z's with the 7z reader", async () => {
    vi.mocked(rarPackages).mockResolvedValue([{ path: "Base.pkg", size: 20 }]);
    probeAs({ "Base.pkg": { category: "gd", title_id: "PPSA1" } });

    let r = await installRarPackages({ host: HOST, archivePath: "/x/Bundle.zip" });
    expect(r.ok).toBe(true);
    expect(vi.mocked(startTransferRar)).not.toHaveBeenCalled();
    let call = vi.mocked(startTransferZip).mock.calls[0];
    expect(call[0]).toBe("/x/Bundle.zip");
    expect(call[1]).toBe("/user/data/ps5upload/pkg_library/zip_Bundle");
    expect(call[4]).toEqual(["!*.pkg"]);

    r = await installRarPackages({ host: HOST, archivePath: "/x/Bundle.7z" });
    expect(r.ok).toBe(true);
    call = vi.mocked(startTransfer7z).mock.calls[0];
    expect(call[1]).toBe("/user/data/ps5upload/pkg_library/7z_Bundle");
    expect(call[4]).toEqual(["!*.pkg"]);
    expect(vi.mocked(startTransferRar)).not.toHaveBeenCalled();
  });
});
