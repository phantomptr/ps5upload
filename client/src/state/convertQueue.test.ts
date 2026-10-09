import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { hydrateConvertQueue, setConvertRunner, useConvertQueue, type ConvertRunner } from "./convertQueue";

const flush = async () => {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
};

describe("the Convert queue", () => {
  let builds: { source: string; resolve: (r: { ok: boolean; packagePath?: string; message?: string }) => void }[];
  let handoffs: { pkg: string; resolve: (r: { ok: boolean; message?: string }) => void }[];
  beforeEach(() => {
    builds = [];
    handoffs = [];
    const runner: ConvertRunner = {
      build: (item) => new Promise((resolve) => builds.push({ source: item.source, resolve })),
      handOff: (_item, pkg) => new Promise((resolve) => handoffs.push({ pkg, resolve })),
      cancel: vi.fn(async () => {}),
    };
    setConvertRunner(runner);
    useConvertQueue.setState({ items: [], running: false });
  });

  const add = (source: string, then: "keep" | "stream" = "stream") =>
    useConvertQueue.getState().add({ source, outputDir: "/out", compression: "balanced", then, host: "10.0.0.2" });

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
});
