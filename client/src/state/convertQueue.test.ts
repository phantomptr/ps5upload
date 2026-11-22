import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { useUploadSettingsStore } from "./uploadSettings";
import {
  buildExtension,
  hydrateConvertQueue,
  imageBuildWith,
  packsOf,
  pipelinePhaseChanged,
  queueTags,
  savedItems,
  setConvertRunner,
  startAfterBuild,
  thenFor,
  useConvertQueue,
  withPacks,
  type ConvertBuild,
  type ConvertRunner,
} from "./convertQueue";

const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
};

describe("the Convert queue", () => {
  let builds: { source: string; resolve: (r: { ok: boolean; packagePath?: string; message?: string }) => void }[];
  let handoffs: { pkg: string; resolve: (r: { ok: boolean; unverified?: true; message?: string }) => void }[];
  // The hand-started build the runner waits for (ready); null: this computer is free.
  let manual: { done: () => void } | null;
  beforeEach(() => {
    builds = [];
    handoffs = [];
    manual = null;
    const runner: ConvertRunner = {
      build: (item) => new Promise((resolve) => builds.push({ source: item.source, resolve })),
      handOff: (_item, pkg) => new Promise((resolve) => handoffs.push({ pkg, resolve })),
      ready: () => (manual ? new Promise<void>((resolve) => (manual = { done: resolve })) : Promise.resolve()),
    };
    setConvertRunner(runner);
    useConvertQueue.setState({ items: [], running: false, armed: false });
  });

  const add = (source: string, then: "keep" | "stream" = "stream") =>
    useConvertQueue.getState().add({ source, outputDir: "/out", compression: "balanced", then, host: "10.0.0.2" });

  it("keeps a .rar's password in memory for the run, and never saves it", async () => {
    useConvertQueue.getState().add({ source: "/g/A.rar", password: "secret", compression: "balanced", then: "keep", host: null });
    const [item] = useConvertQueue.getState().items;
    expect(item.password).toBe("secret");
    expect(JSON.stringify(savedItems(useConvertQueue.getState().items))).not.toContain("secret");
  });

  it("offers a refused .rar again with a password, and starts the queue", async () => {
    useConvertQueue.getState().add({ source: "/g/A.rar", compression: "balanced", then: "keep", host: null });
    void useConvertQueue.getState().start();
    await flush();
    builds[0].resolve({ ok: false, message: "This archive is password-protected. Enter its password and start again." });
    await flush();
    const id = useConvertQueue.getState().items[0].id;
    expect(useConvertQueue.getState().items[0].status).toBe("failed");
    useConvertQueue.getState().retryWithPassword(id, "pw");
    await flush();
    expect(useConvertQueue.getState().items[0]).toMatchObject({ status: "running", password: "pw" });
    expect(builds).toHaveLength(2);
  });

  it("marks a cancelled build as cancelled, not failed, and goes on unless held", async () => {
    add("/g/A", "keep");
    add("/g/B", "keep");
    void useConvertQueue.getState().start();
    await flush();
    builds[0].resolve({ ok: false, cancelled: true } as never);
    await flush();
    const items = useConvertQueue.getState().items;
    expect(items[0].status).toBe("cancelled");
    expect(items[1].status).toBe("running");
  });

  it("Cancel and stop leaves the games behind it waiting", async () => {
    add("/g/A", "keep");
    add("/g/B", "keep");
    void useConvertQueue.getState().start();
    await flush();
    useConvertQueue.getState().holdNext();
    builds[0].resolve({ ok: false, cancelled: true } as never);
    await flush();
    const items = useConvertQueue.getState().items;
    expect(items[0].status).toBe("cancelled");
    expect(items[1].status).toBe("pending");
    expect(builds).toHaveLength(1);
  });

  it("an image with asset packs keeps its .ffpfsc compression and its format", () => {
    const remembered = { kind: "image", format: "ffpkg", compress: true } as const;
    expect(imageBuildWith(remembered, undefined)).toEqual(remembered);
    const packed = imageBuildWith(remembered, { installRuntime: false });
    expect(packed).toEqual({ ...remembered, lz4: { installRuntime: false } });
    expect(buildExtension(packed)).toBe(".ffpfsc");
    const plain = imageBuildWith({ ...remembered, compress: false }, { zstdLevel: 5 });
    expect(buildExtension(plain)).toBe(".ffpkg");
  });

  it("takes the same game for a second console, but not twice for one", () => {
    const q = useConvertQueue.getState();
    const item = { source: "/g/A", outputDir: "/out", compression: "balanced", then: "stream" } as const;
    expect(q.add({ ...item, host: "10.0.0.2" })).toBe(true);
    expect(q.add({ ...item, host: "10.0.0.2" })).toBe(false);
    expect(q.add({ ...item, host: "10.0.0.3" })).toBe(true);
  });

  it("builds one at a time, and starts the next while the first installs", async () => {
    add("/g/A");
    add("/g/B");
    void useConvertQueue.getState().start();
    await flush();
    expect(builds.map((b) => b.source)).toEqual(["/g/A"]);
    builds[0].resolve({ ok: true, packagePath: "/out/A.pkg" });
    await flush();
    // A is handed to the console queue; B builds meanwhile.
    expect(handoffs.map((h) => h.pkg)).toEqual(["/out/A.pkg"]);
    expect(builds.map((b) => b.source)).toEqual(["/g/A", "/g/B"]);
    expect(useConvertQueue.getState().items[0]).toMatchObject({ status: "installing", packagePath: "/out/A.pkg" });
    handoffs[0].resolve({ ok: true });
    await flush();
    expect(useConvertQueue.getState().items[0].status).toBe("done");
  });

  it("an install the PS5 took but nobody confirmed ends done with what to check", async () => {
    add("/g/A");
    void useConvertQueue.getState().start();
    await flush();
    builds[0].resolve({ ok: true, packagePath: "/out/A.pkg" });
    await flush();
    handoffs[0].resolve({ ok: true, unverified: true, message: "Check Downloads on the PS5. The package was kept." });
    await flush();
    expect(useConvertQueue.getState().items[0]).toMatchObject({
      status: "done",
      error: "Check Downloads on the PS5. The package was kept.",
    });
  });

  it("keeps a package only when asked, and records a failed build without stopping the queue", async () => {
    add("/g/A", "keep");
    add("/g/B", "keep");
    void useConvertQueue.getState().start();
    await flush();
    builds[0].resolve({ ok: false, message: "disk full" });
    await flush();
    builds[1].resolve({ ok: true, packagePath: "/out/B.pkg" });
    await flush();
    const [a, b] = useConvertQueue.getState().items;
    expect(a).toMatchObject({ status: "failed", error: "disk full" });
    expect(b).toMatchObject({ status: "done", packagePath: "/out/B.pkg" });
    expect(handoffs).toHaveLength(0);
    expect(useConvertQueue.getState().running).toBe(false);
  });

  it("refuses the same game twice", () => {
    expect(add("/g/A")).toBe(true);
    expect(add("/g/A")).toBe(false);
    expect(useConvertQueue.getState().items).toHaveLength(1);
  });

  it("puts a build interrupted by a restart back in line", () => {
    const restored = hydrateConvertQueue([
      { id: "c1", source: "/g/A", outputDir: "/out", compression: "balanced", then: "keep", host: null, status: "running" },
      { id: "c2", source: "/g/B", outputDir: "/out", compression: "balanced", then: "stream", host: "h", status: "installing", packagePath: "/out/B.pkg" },
    ]);
    expect(restored.map((i) => i.status)).toEqual(["pending", "handed"]);
  });

  it("stops after the current build, and removes a waiting one", async () => {
    add("/g/A", "keep");
    add("/g/B", "keep");
    void useConvertQueue.getState().start();
    await flush();
    useConvertQueue.getState().stop();
    useConvertQueue.getState().remove(useConvertQueue.getState().items[1].id);
    builds[0].resolve({ ok: true, packagePath: "/out/A.pkg" });
    await flush();
    expect(builds).toHaveLength(1);
    expect(useConvertQueue.getState().items).toHaveLength(1);
  });

  // #621: games added after Start are made by the same run, in the order added.
  it("takes games added while it runs, in order, without another Start", async () => {
    add("/g/A", "keep");
    void useConvertQueue.getState().start();
    await flush();
    add("/g/B", "keep");
    add("/g/C", "keep");
    builds[0].resolve({ ok: true, packagePath: "/out/A.pkg" });
    await flush();
    builds[1].resolve({ ok: true, packagePath: "/out/B.pkg" });
    await flush();
    builds[2].resolve({ ok: true, packagePath: "/out/C.pkg" });
    await flush();
    expect(builds.map((b) => b.source)).toEqual(["/g/A", "/g/B", "/g/C"]);
    expect(useConvertQueue.getState().items.map((i) => i.status)).toEqual(["done", "done", "done"]);
    expect(useConvertQueue.getState().running).toBe(false);
  });

  // #621: a game queued while a Convert started by hand runs is made after it, when "Start the
  // queue when you add something" is on; off, it waits for Start, as the console's queue does.
  it("starts after a hand-started build only when adding starts the queue", async () => {
    useUploadSettingsStore.setState({ queueAutoStart: false });
    add("/g/B", "keep");
    startAfterBuild(true);
    await flush();
    expect(useConvertQueue.getState().armed).toBe(false);

    useUploadSettingsStore.setState({ queueAutoStart: true });
    startAfterBuild(false);
    expect(useConvertQueue.getState().armed).toBe(false);
    startAfterBuild(true);
    expect(useConvertQueue.getState().armed).toBe(true);
    // Its result stays (its buttons with it) until it is put away.
    pipelinePhaseChanged("done");
    await flush();
    expect(builds).toHaveLength(0);
    pipelinePhaseChanged("idle");
    await flush();
    expect(builds.map((b) => b.source)).toEqual(["/g/B"]);
    expect(useConvertQueue.getState().armed).toBe(false);
    // A second add while it runs does not start a second run.
    add("/g/C", "keep");
    startAfterBuild(true);
    expect(useConvertQueue.getState().armed).toBe(false);
    await flush();
    expect(builds).toHaveLength(1);
    builds[0].resolve({ ok: true, packagePath: "/out/B.pkg" });
    await flush();
    expect(builds.map((b) => b.source)).toEqual(["/g/B", "/g/C"]);
  });

  // Review: the hand-started build failed — its message and Retry stay; the queue waits for Start.
  it("never starts by itself after a hand-started build that failed", async () => {
    useUploadSettingsStore.setState({ queueAutoStart: true });
    add("/g/B", "keep");
    startAfterBuild(true);
    pipelinePhaseChanged("failed");
    pipelinePhaseChanged("idle");
    await flush();
    expect(builds).toHaveLength(0);
    expect(useConvertQueue.getState().armed).toBe(false);
    expect(useConvertQueue.getState().items[0].status).toBe("pending");
  });

  it("starts at once when Start is pressed while it waits for a result to be put away", async () => {
    useUploadSettingsStore.setState({ queueAutoStart: true });
    add("/g/B", "keep");
    startAfterBuild(true);
    void useConvertQueue.getState().start();
    await flush();
    expect(builds.map((b) => b.source)).toEqual(["/g/B"]);
    expect(useConvertQueue.getState().armed).toBe(false);
    // Putting the result away later does not start a second run.
    pipelinePhaseChanged("idle");
    await flush();
    expect(builds).toHaveLength(1);
  });

  // Review: started while a hand-started build runs, the queue took its next game (Converting)
  // at once; it now waits first, so that game stays Waiting — removable, changeable, stoppable.
  it("waits for a hand-started build before taking the next game", async () => {
    manual = { done: () => {} };
    add("/g/A", "keep");
    add("/g/B", "keep");
    void useConvertQueue.getState().start();
    await flush();
    expect(useConvertQueue.getState().running).toBe(true);
    expect(useConvertQueue.getState().items.map((i) => i.status)).toEqual(["pending", "pending"]);
    const [a, b] = useConvertQueue.getState().items;
    expect(useConvertQueue.getState().update(a.id, { then: "stream" })).toBe(true);
    useConvertQueue.getState().remove(b.id);
    manual!.done();
    await flush();
    expect(builds.map((x) => x.source)).toEqual(["/g/A"]);
    expect(useConvertQueue.getState().items[0]).toMatchObject({ status: "running", then: "stream" });
  });

  it("Stop while it waits for a hand-started build takes nothing", async () => {
    manual = { done: () => {} };
    add("/g/A", "keep");
    void useConvertQueue.getState().start();
    await flush();
    useConvertQueue.getState().stop();
    manual!.done();
    await flush();
    expect(builds).toHaveLength(0);
    expect(useConvertQueue.getState().items[0].status).toBe("pending");
    expect(useConvertQueue.getState().running).toBe(false);
  });
});

describe("what happens after converting", () => {
  it("installs a package streamed or uploaded, but only uploads an image", () => {
    expect(thenFor("pkg", "stream", true)).toBe("stream");
    expect(thenFor("pkg", "upload", true)).toBe("upload");
    expect(thenFor("image", "stream", true)).toBe("upload");
    expect(thenFor("image", "keep", true)).toBe("keep");
  });

  it("only keeps a folder, and names it as one", () => {
    expect(thenFor("folder", "stream", true)).toBe("keep");
    expect(thenFor("folder", "upload", true)).toBe("keep");
    expect(buildExtension({ kind: "folder" })).toBe("folder");
  });

  it("keeps the result when no console answers", () => {
    expect(thenFor("pkg", "stream", false)).toBe("keep");
    expect(thenFor("image", "upload", false)).toBe("keep");
  });
});

describe("a queued game's asset packs (#905)", () => {
  const zstdImage: ConvertBuild = {
    kind: "image",
    format: "ffpkg",
    compress: true,
    lz4: { zstdLevel: 19, zstdRuntime: "pinned", profileToml: "[p]" },
  };

  it("reads packs from an image or a folder, whichever field holds them", () => {
    expect(packsOf(zstdImage)).toEqual({ kind: "zstd", level: 19, runtime: "pinned", profileToml: "[p]" });
    expect(packsOf({ kind: "image", format: "exfat", compress: false, lz4: { installRuntime: true } })).toEqual({
      kind: "lz4",
      installRuntime: true,
    });
    expect(packsOf({ kind: "folder", zstd: { level: 1 } })).toEqual({ kind: "zstd", level: 1 });
    expect(packsOf({ kind: "folder", lz4: { profileToml: "[q]" } })).toEqual({ kind: "lz4", profileToml: "[q]" });
    expect(packsOf({ kind: "image", format: "exfat", compress: false })).toEqual({ kind: "none" });
    expect(packsOf({ kind: "pkg" })).toEqual({ kind: "none" });
    expect(packsOf(undefined)).toEqual({ kind: "none" });
  });

  it("puts packs on and off without touching the format, and moves them between image and folder", () => {
    const plainImage: ConvertBuild = { kind: "image", format: "ffpkg", compress: true };
    expect(withPacks(plainImage, { kind: "zstd" })).toEqual({ ...plainImage, lz4: { zstdLevel: 5 } });
    expect(withPacks(zstdImage, { kind: "none" })).toEqual(plainImage);
    expect(withPacks({ kind: "folder" }, packsOf(zstdImage))).toEqual({
      kind: "folder",
      zstd: { level: 19, runtime: "pinned", profileToml: "[p]" },
    });
    expect(withPacks({ kind: "image", format: "exfat", compress: false }, { kind: "zstd", level: 1 })).toEqual({
      kind: "image",
      format: "exfat",
      compress: false,
      lz4: { zstdLevel: 1 },
    });
    // Packs and "write the packs out as plain files" are one choice.
    expect(
      withPacks({ kind: "image", format: "exfat", compress: false, unpackPacks: true }, { kind: "zstd" }),
    ).not.toHaveProperty("unpackPacks");
    expect(withPacks({ kind: "folder", unpackPacks: true }, { kind: "none" })).toEqual({
      kind: "folder",
      unpackPacks: true,
    });
    expect(withPacks({ kind: "pkg" }, { kind: "zstd" })).toEqual({ kind: "pkg" });
  });

  it("shows the key choices on the row", () => {
    const base = {
      id: "c1",
      source: "/g",
      compression: "balanced" as const,
      then: "keep" as const,
      host: null,
      status: "pending" as const,
    };
    expect(queueTags({ ...base, build: zstdImage, imageLevel: "smallest" })).toEqual([
      { kind: "zstd", level: 19 },
      { kind: "level", level: "smallest" },
    ]);
    expect(queueTags({ ...base, build: { kind: "image", format: "exfat", compress: false } })).toEqual([]);
    expect(queueTags({ ...base, build: { kind: "folder", lz4: {} } })).toEqual([{ kind: "lz4" }]);
    expect(queueTags({ ...base, build: { kind: "folder", unpackPacks: true } })).toEqual([{ kind: "unpack" }]);
    expect(queueTags({ ...base, build: { kind: "pkg" }, language: "en-US" })).toEqual([
      { kind: "language", code: "en-US" },
    ]);
  });

  it("keeps packs set while waiting through a restart, and the build gets them", async () => {
    const built: ConvertBuild[] = [];
    setConvertRunner({
      build: async (item) => {
        built.push(item.build!);
        return { ok: true, packagePath: "/out/G.ffpkg" };
      },
      handOff: async () => ({ ok: true }),
    });
    useConvertQueue.setState({ items: [], running: false, armed: false });
    const q = useConvertQueue.getState();
    q.add({
      source: "/games/G",
      compression: "balanced",
      build: { kind: "image", format: "ffpkg", compress: false },
      then: "keep",
      host: null,
    });
    const first = useConvertQueue.getState().items[0];
    // Set after it was queued, then the app restarts with the queue saved.
    expect(q.update(first.id, { build: withPacks(first.build!, { kind: "zstd", level: 19 }) })).toBe(true);
    const restored = hydrateConvertQueue(JSON.parse(JSON.stringify(savedItems(useConvertQueue.getState().items))));
    useConvertQueue.setState({ items: restored });
    await useConvertQueue.getState().start();
    await flush();
    expect(built).toEqual([{ kind: "image", format: "ffpkg", compress: false, lz4: { zstdLevel: 19 } }]);
  });
});
