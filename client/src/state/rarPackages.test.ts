import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/links", () => ({
  rarPackages: vi.fn(),
  pkgConsoleProbe: vi.fn(),
}));
vi.mock("../api/ps5", () => ({
  fetchVolumes: vi.fn(async () => []),
  fsMkdir: vi.fn(async () => {}),
  jobStatus: vi.fn(),
  startTransferRar: vi.fn(async () => "job-1"),
}));
const installFromConsolePath = vi.fn();
vi.mock("./pkgLibrary", () => ({
  pkgLibraryStore: () => ({ getState: () => ({ installFromConsolePath }) }),
}));

import { pkgConsoleProbe, rarPackages } from "../api/links";
import { jobStatus, startTransferRar } from "../api/ps5";
import {
  installRarPackages,
  isRarFirstVolume,
  orderForInstall,
  rarFolderName,
} from "./rarPackages";

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
    expect(rarFolderName("/x/My Game [v1].part1.rar")).toBe("rar_My_Game_v1");
    expect(rarFolderName("/x/My Game [v1].part1.rar")).toBe(rarFolderName("/y/My Game [v1].rar"));
    expect(rarFolderName("..rar")).toBe("rar_archive");
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
