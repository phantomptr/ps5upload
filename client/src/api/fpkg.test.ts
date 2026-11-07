import { describe, expect, it, vi } from "vitest";

vi.mock("../lib/invokeLogged", () => ({ invoke: vi.fn(async () => ({ ok: true })) }));
import { invoke } from "../lib/invokeLogged";
import { fpkg } from "./fpkg";

describe("fpkg.deletePackage", () => {
  it("asks the engine to delete exactly that package", async () => {
    await fpkg.deletePackage("/out/PPSA01234.pkg");
    expect(invoke).toHaveBeenCalledWith("fpkg_delete", { path: "/out/PPSA01234.pkg" });
  });
});

describe("fpkg.estimate", () => {
  it("asks for the estimates on their own, apart from the check", async () => {
    await fpkg.estimate("/games/a");
    expect(invoke).toHaveBeenCalledWith("fpkg_estimate", { source: "/games/a" });
  });
});

describe("fpkg.rebuildImageAmprIndex", () => {
  // A second Rebuild of the same image found `<name>-edited` taken and failed (409).
  it("numbers the edited copy when an earlier rebuild already made one", async () => {
    await fpkg.rebuildImageAmprIndex("/games/G.exfat");
    expect(invoke).toHaveBeenCalledWith("exfat_build", {
      source: "/games/G.exfat",
      edits: [],
      rebuildAmprIndex: true,
      keepBoth: true,
    });
  });
});

describe("fpkg.buildImage", () => {
  // A queued game is built with the AMPR emulator choice it was added with, not whatever is set
  // when its turn comes; with none of its own, the current setting.
  it("sends the build's own AMPR emulator choice over the setting", async () => {
    const { useConvertPrefs } = await import("../state/convertPrefs");
    useConvertPrefs.setState({ addAmprEmu: true, amprEmuBuild: "ampr_emu" });
    await fpkg.buildImage("/games/G", "/out", "exfat", false, undefined, undefined, false, undefined, undefined, false, {
      add: false,
      build: "ampr-emu-ng",
    });
    expect(invoke).toHaveBeenLastCalledWith(
      "exfat_build",
      expect.objectContaining({ addAmprEmu: false, amprEmuBuild: "ampr-emu-ng" }),
    );
    await fpkg.buildImage("/games/G", "/out");
    expect(invoke).toHaveBeenLastCalledWith(
      "exfat_build",
      expect.objectContaining({ addAmprEmu: true, amprEmuBuild: "ampr_emu" }),
    );
  });
});
