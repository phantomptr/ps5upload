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
    }),
    subscribe: () => () => {},
  },
}));
vi.mock("../state/pkgLibrary", () => ({ pkgLibraryStore: () => ({ getState: () => ({}) }) }));

import { installConvertRunner } from "./convertQueueRunner";
import { useConvertQueue } from "../state/convertQueue";

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
    // source, outputDir, compress, format, no upload
    expect(calls[0].args).toEqual(["/games/Game-app", undefined, true, "ffpfs", undefined]);
    const item = useConvertQueue.getState().items[0];
    expect(item.status).toBe("done");
    expect(item.packagePath).toBe("/out/Game.ffpfsc");
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
});
