import { beforeEach, describe, expect, it, vi } from "vitest";

const install = vi.fn(async () => ({ ok: true }));
vi.mock("../../state/pkgLibrary", () => ({
  pkgLibraryStore: () => ({ getState: () => ({ install }) }),
}));

import { installStagedPkg, stagedPkgAction } from "./installStaged";

describe("installing a staged update or DLC from the game page", () => {
  beforeEach(() => install.mockClear());

  it("queues it through the package library, like Install Package", async () => {
    const confirm = vi.fn(async () => true);
    const r = await installStagedPkg({
      host: "192.168.86.100",
      entry: { path: "/data/pkg/gp/patch.pkg" },
      baseInstalled: true,
      confirmWithoutBase: confirm,
    });
    expect(r).toEqual({ ok: true });
    expect(install).toHaveBeenCalledWith("/data/pkg/gp/patch.pkg", "192.168.86.100");
    expect(confirm).not.toHaveBeenCalled();
  });

  it("asks first when the base game isn't on the console", async () => {
    const confirm = vi.fn(async () => false);
    const r = await installStagedPkg({
      host: "h",
      entry: { path: "/p.pkg" },
      baseInstalled: false,
      confirmWithoutBase: confirm,
    });
    expect(confirm).toHaveBeenCalledOnce();
    expect(r).toBeNull();
    expect(install).not.toHaveBeenCalled();
  });

  it("installs anyway when the user says so", async () => {
    await installStagedPkg({
      host: "h",
      entry: { path: "/p.pkg" },
      baseInstalled: false,
      confirmWithoutBase: async () => true,
    });
    expect(install).toHaveBeenCalledOnce();
  });

  it("offers no button while the package is queued or installing", () => {
    expect(stagedPkgAction({ status: "queued" })).toBe("busy");
    expect(stagedPkgAction({ status: "installing" })).toBe("busy");
    expect(stagedPkgAction({ status: "idle" })).toBe("install");
    expect(stagedPkgAction({ status: "idle", installedHere: true })).toBe("reinstall");
  });
});
