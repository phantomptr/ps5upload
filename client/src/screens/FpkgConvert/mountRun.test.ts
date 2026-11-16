import { describe, expect, it } from "vitest";

import { mountablePackage, mountRunKey, runMount, type MountRun } from "./mountRun";

describe("Convert's mount run", () => {
  it("offers a mount for a kept .pkg only", () => {
    expect(mountablePackage("/out/Game.pkg")).toBe(true);
    expect(mountablePackage("/out/Game.PKG")).toBe(true);
    expect(mountablePackage("/out/Game.ffpfsc")).toBe(false);
    expect(mountablePackage("/out/Game")).toBe(false);
    expect(mountablePackage(null)).toBe(false);
  });

  it("keys the result to the console and the run", () => {
    const done = { phase: "done" as const, source: "/g", packagePath: "/out/a.pkg" };
    const k = mountRunKey("10.0.0.2", done);
    expect(mountRunKey("10.0.0.2", done)).toBe(k);
    expect(mountRunKey("10.0.0.3", done)).not.toBe(k);
    expect(mountRunKey("10.0.0.2", { ...done, packagePath: "/out/b.pkg" })).not.toBe(k);
    expect(mountRunKey("10.0.0.2", { phase: "running", startedMs: 1 })).not.toBe(k);
    expect(mountRunKey("10.0.0.2", { phase: "idle" })).not.toBe(k);
  });

  it("clears busy and shows the error when placing throws", async () => {
    const seen: MountRun[] = [];
    await runMount(
      () => Promise.reject(new Error("engine gone")),
      () => true,
      (r) => seen.push(r),
    );
    expect(seen).toEqual([
      { busy: true, result: null },
      { busy: false, result: { ok: false, message: "engine gone" } },
    ]);
  });

  it("drops a late result for a run that is no longer shown", async () => {
    const seen: MountRun[] = [];
    await runMount(
      async () => ({ ok: true, message: "done" }),
      () => false,
      (r) => seen.push(r),
    );
    expect(seen).toEqual([{ busy: true, result: null }]);
  });
});
