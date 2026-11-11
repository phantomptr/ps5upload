import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// The pipeline is faked: start/buildImage finish at once and leave a done result.
const calls: { kind: "pkg" | "image"; args: unknown[] }[] = [];
let pipeline: { phase: string; packagePath?: string; message?: string } = { phase: "idle" };
vi.mock("../state/fpkgConversion", () => ({
  useFpkgConversion: {
    getState: () => ({
      pipeline,
      start: async (...args: unknown[]) => {
        calls.push({ kind: "pkg", args });
        pipeline = { phase: "done", packagePath: "/out/Game.pkg" };
      },
      buildImage: async (...args: unknown[]) => {
        calls.push({ kind: "image", args });
        pipeline = { phase: "done", packagePath: "/out/Game.ffpfsc" };
      },
      cancel: async () => {},
      attachRunning: async () => {},
    }),
    subscribe: () => () => {},
  },
}));
const lib = vi.hoisted(() => ({ installStream: vi.fn(), uploadInstall: vi.fn() }));
vi.mock("../state/pkgLibrary", () => ({ pkgLibraryStore: () => ({ getState: () => lib }) }));

import { fpkg } from "../api/fpkg";
import { handOff, installConvertRunner } from "./convertQueueRunner";
import { useConvertQueue, type ConvertItem } from "../state/convertQueue";

const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
};

describe("the Convert queue's runner (bug: a queued image came out as a .pkg)", () => {
  beforeEach(() => {
    calls.length = 0;
    pipeline = { phase: "idle" };
    installConvertRunner();
    useConvertQueue.setState({ items: [], running: false });
  });

  it("builds a queued compressed image as that image, never a package", async () => {
    useConvertQueue.getState().add({
      source: "/games/Game-app",
      compression: "smallest",
      build: { kind: "image", format: "ffpfs", compress: true },
      then: "keep",
      host: null,
    });
    await useConvertQueue.getState().start();
    await flush();
    expect(calls).toHaveLength(1);
    expect(calls[0].kind).toBe("image");
    // source, outputDir, compress, format, no upload, no LZ4 packs, no password, no keep-both,
    // and no choices of its own: the current settings apply
    expect(calls[0].args).toEqual([
      "/games/Game-app",
      undefined,
      true,
      "ffpfs",
      undefined,
      undefined,
      undefined,
      undefined,
      { fullVerify: undefined, imageLevel: undefined },
    ]);
    const item = useConvertQueue.getState().items[0];
    expect(item.status).toBe("done");
    expect(item.packagePath).toBe("/out/Game.ffpfsc");
  });

  it("keeps the LZ4 asset-pack choice of a queued .exfat", async () => {
    useConvertQueue.getState().add({
      source: "/games/Ampr-app",
      compression: "balanced",
      build: { kind: "image", format: "exfat", compress: false, lz4: { profileToml: "[x]" } },
      then: "keep",
      host: null,
    });
    await useConvertQueue.getState().start();
    await flush();
    expect(calls[0].args[5]).toEqual({ profileToml: "[x]" });
  });

  it("sends a queued image on to the PS5 when asked", async () => {
    useConvertQueue.getState().add({
      source: "/games/Game-app",
      compression: "balanced",
      build: { kind: "image", format: "exfat", compress: false },
      imageDest: { volume: "/mnt/ext0", subpath: "homebrew" },
      then: "upload",
      host: "10.0.0.2",
      deleteAfterInstall: true,
    });
    await useConvertQueue.getState().start();
    await flush();
    expect(calls[0].args[4]).toEqual({ host: "10.0.0.2", volume: "/mnt/ext0", subpath: "homebrew", deleteAfter: true });
  });

  it("still builds a package for an item queued as one (and for older items with no choice)", async () => {
    useConvertQueue.getState().add({ source: "/games/Old-app", compression: "balanced", then: "keep", host: null });
    await useConvertQueue.getState().start();
    await flush();
    expect(calls[0].kind).toBe("pkg");
  });

  it("leaves the next game Waiting while a build started by hand runs", async () => {
    pipeline = { phase: "running" };
    useConvertQueue.getState().add({ source: "/games/Next-app", compression: "balanced", then: "keep", host: null });
    void useConvertQueue.getState().start();
    await flush();
    expect(calls).toHaveLength(0);
    expect(useConvertQueue.getState().items[0].status).toBe("pending");
    useConvertQueue.getState().stop();
  });

  it("#669: a queued game is built with its own choices, not whatever is set now", async () => {
    useConvertQueue.getState().add({
      source: "/games/Own-app",
      outputDir: "  /mine/out  ",
      keepBoth: true,
      compression: "stored",
      build: { kind: "image", format: "ffpkg", compress: true },
      fullVerify: true,
      imageLevel: "smallest",
      then: "keep",
      host: null,
    });
    await useConvertQueue.getState().start();
    await flush();
    expect(calls[0].args).toEqual([
      "/games/Own-app",
      "/mine/out",
      true,
      "ffpkg",
      undefined,
      undefined,
      undefined,
      true,
      { fullVerify: true, imageLevel: "smallest" },
    ]);
  });

  it("a queued image keeps the AMPR emulator choice it was added with", async () => {
    useConvertQueue.getState().add({
      source: "/games/Emu-app",
      compression: "balanced",
      build: { kind: "image", format: "exfat", compress: false },
      addAmprEmu: false,
      amprEmuBuild: "ampr-emu-ng",
      then: "keep",
      host: null,
    });
    await useConvertQueue.getState().start();
    await flush();
    expect(calls[0].args[8]).toEqual({
      fullVerify: undefined,
      imageLevel: undefined,
      addAmprEmu: false,
      amprEmuBuild: "ampr-emu-ng",
    });
  });

  it("#669: a game changed to an image while it waited still uploads, to the destination set now", async () => {
    useConvertQueue.getState().add({
      source: "/games/Changed-app",
      compression: "balanced",
      build: { kind: "image", format: "exfat", compress: false },
      then: "upload",
      host: "10.0.0.2",
    });
    await useConvertQueue.getState().start();
    await flush();
    const plan = calls[0].args[4] as { host: string; subpath: string } | undefined;
    expect(plan?.host).toBe("10.0.0.2");
    expect(plan?.subpath).toBeTruthy();
  });
});

describe("handing a built package to the console (delete after install)", () => {
  const item = { host: "10.0.0.2", then: "install", deleteAfterInstall: true } as unknown as ConvertItem;
  const del = vi.spyOn(fpkg, "deletePackage");
  const deletes = () => del.mock.calls;
  beforeEach(() => {
    del.mockReset();
    del.mockResolvedValue({ ok: true });
    lib.installStream.mockReset();
    lib.uploadInstall.mockReset();
  });

  it("deletes the package once its install is confirmed", async () => {
    lib.installStream.mockResolvedValue({ ok: true });
    expect(await handOff(item, "/out/G.pkg")).toEqual({ ok: true, message: undefined });
    expect(deletes()).toEqual([["/out/G.pkg"]]);
  });

  it("keeps the package when the PS5 took the install but it could not be confirmed", async () => {
    lib.installStream.mockResolvedValue({ ok: true, unverified: true, message: "Check the PS5. The package was kept." });
    expect(await handOff(item, "/out/G.pkg")).toEqual({
      ok: true,
      unverified: true,
      message: "Check the PS5. The package was kept.",
    });
    expect(deletes()).toEqual([]);
    // The upload route reports the same way.
    lib.uploadInstall.mockResolvedValue({ ok: true, unverified: true, message: "kept" });
    await handOff({ ...item, then: "upload" } as ConvertItem, "/out/G.pkg");
    expect(deletes()).toEqual([]);
  });

  it("keeps the package when the install failed", async () => {
    lib.installStream.mockResolvedValue({ ok: false, message: "refused" });
    expect(await handOff(item, "/out/G.pkg")).toEqual({ ok: false, message: "refused" });
    expect(deletes()).toEqual([]);
  });
});
