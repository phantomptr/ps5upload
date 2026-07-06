import { describe, expect, it, vi } from "vitest";
import {
  appendStep,
  DEFAULT_AUTO_LOADER,
  isPayloadPath,
  isRepoStep,
  movePlaylistDown,
  movePlaylistUp,
  moveStepDown,
  moveStepUp,
  patchPlaylist,
  removePlaylist,
  removeStep,
  resolveRepoStepPath,
  sanitiseSleepMs,
  type Playlist,
  type PlaylistStep,
  type RepoResolveDeps,
} from "./playlistOps";

const step = (path: string, sleepMs = 0): PlaylistStep => ({ path, sleepMs });

const playlist = (id: string, steps: PlaylistStep[] = []): Playlist => ({
  id,
  name: id,
  steps,
  continueOnFailure: false,
  createdAt: 0,
  updatedAt: 0,
});

describe("patchPlaylist", () => {
  it("merges the patch and bumps updatedAt", () => {
    const list = [playlist("p1")];
    const next = patchPlaylist(list, "p1", { name: "renamed" });
    expect(next[0].name).toBe("renamed");
    expect(next[0].updatedAt).toBeGreaterThan(0);
  });
  it("returns same reference when id missing", () => {
    const list = [playlist("p1")];
    expect(patchPlaylist(list, "missing", { name: "x" })).toBe(list);
  });
});

describe("appendStep", () => {
  it("adds the step to the end", () => {
    const list = [playlist("p1", [step("/a")])];
    const next = appendStep(list, "p1", step("/b", 500));
    expect(next[0].steps.map((s) => s.path)).toEqual(["/a", "/b"]);
    expect(next[0].steps[1].sleepMs).toBe(500);
  });
});

describe("removeStep", () => {
  it("drops the indexed step", () => {
    const list = [playlist("p1", [step("/a"), step("/b"), step("/c")])];
    const next = removeStep(list, "p1", 1);
    expect(next[0].steps.map((s) => s.path)).toEqual(["/a", "/c"]);
  });
  it("returns same reference when index out of range", () => {
    const list = [playlist("p1", [step("/a")])];
    expect(removeStep(list, "p1", 9)).toBe(list);
    expect(removeStep(list, "p1", -1)).toBe(list);
  });
});

describe("moveStepUp / moveStepDown", () => {
  it("moves up", () => {
    const list = [playlist("p1", [step("/a"), step("/b")])];
    expect(moveStepUp(list, "p1", 1)[0].steps.map((s) => s.path)).toEqual([
      "/b",
      "/a",
    ]);
  });
  it("moves down", () => {
    const list = [playlist("p1", [step("/a"), step("/b")])];
    expect(moveStepDown(list, "p1", 0)[0].steps.map((s) => s.path)).toEqual([
      "/b",
      "/a",
    ]);
  });
  it("up no-op at top", () => {
    const list = [playlist("p1", [step("/a"), step("/b")])];
    expect(moveStepUp(list, "p1", 0)).toBe(list);
  });
  it("down no-op at bottom", () => {
    const list = [playlist("p1", [step("/a"), step("/b")])];
    expect(moveStepDown(list, "p1", 1)).toBe(list);
  });
});

describe("removePlaylist", () => {
  it("removes by id", () => {
    const list = [playlist("p1"), playlist("p2")];
    expect(removePlaylist(list, "p1").map((p) => p.id)).toEqual(["p2"]);
  });
  it("returns same reference when id missing", () => {
    const list = [playlist("p1")];
    expect(removePlaylist(list, "missing")).toBe(list);
  });
});

describe("movePlaylistUp / movePlaylistDown", () => {
  it("moves a playlist up", () => {
    const list = [playlist("p1"), playlist("p2"), playlist("p3")];
    expect(movePlaylistUp(list, "p2").map((p) => p.id)).toEqual([
      "p2",
      "p1",
      "p3",
    ]);
  });
  it("moves a playlist down", () => {
    const list = [playlist("p1"), playlist("p2"), playlist("p3")];
    expect(movePlaylistDown(list, "p2").map((p) => p.id)).toEqual([
      "p1",
      "p3",
      "p2",
    ]);
  });
  it("up is a no-op (same ref) at the top", () => {
    const list = [playlist("p1"), playlist("p2")];
    expect(movePlaylistUp(list, "p1")).toBe(list);
  });
  it("down is a no-op (same ref) at the bottom", () => {
    const list = [playlist("p1"), playlist("p2")];
    expect(movePlaylistDown(list, "p2")).toBe(list);
  });
  it("returns same reference when id missing", () => {
    const list = [playlist("p1"), playlist("p2")];
    expect(movePlaylistUp(list, "missing")).toBe(list);
    expect(movePlaylistDown(list, "missing")).toBe(list);
  });
  it("does not bump updatedAt (reorder is not an edit)", () => {
    const list = [playlist("p1"), playlist("p2")];
    const next = movePlaylistDown(list, "p1");
    expect(next.find((p) => p.id === "p1")?.updatedAt).toBe(0);
  });
});

describe("sanitiseSleepMs", () => {
  it("coerces strings to integers", () => {
    expect(sanitiseSleepMs("1500")).toBe(1500);
    expect(sanitiseSleepMs("3.7")).toBe(3);
  });
  it("rejects negatives and NaN", () => {
    expect(sanitiseSleepMs(-1)).toBe(0);
    expect(sanitiseSleepMs("oops")).toBe(0);
    expect(sanitiseSleepMs(Number.POSITIVE_INFINITY)).toBe(0);
  });
});

describe("isPayloadPath", () => {
  it("accepts known payload extensions, case-insensitively", () => {
    for (const p of [
      "/a/b/loader.elf",
      "C:\\dev\\x.BIN",
      "hen.js",
      "cheats.lua",
      "tool.JAR",
    ]) {
      expect(isPayloadPath(p)).toBe(true);
    }
  });
  it("rejects non-payloads — notably .pkg (different install flow)", () => {
    for (const p of [
      "/games/base.pkg",
      "/some/folder",
      "readme.txt",
      "noext",
      "archive.zip",
    ]) {
      expect(isPayloadPath(p)).toBe(false);
    }
  });
});

describe("DEFAULT_AUTO_LOADER", () => {
  it("is opt-in (disabled, no playlist) so it never auto-sends unasked", () => {
    expect(DEFAULT_AUTO_LOADER).toEqual({
      enabled: false,
      playlistId: null,
      bringUpPlaylistId: null,
    });
  });
});

describe("isRepoStep", () => {
  it("is true only when payloadId is a non-empty string", () => {
    expect(isRepoStep({ payloadId: "kstuff-echostretch" })).toBe(true);
    expect(isRepoStep({ payloadId: "custom-abc123" })).toBe(true);
    expect(isRepoStep({ payloadId: "" })).toBe(false);
    expect(isRepoStep({ payloadId: "   " })).toBe(false);
    expect(isRepoStep({ payloadId: undefined })).toBe(false);
  });
});

describe("resolveRepoStepPath", () => {
  const mkDeps = (over: Partial<RepoResolveDeps> = {}): RepoResolveDeps => ({
    localInventory: async () => [],
    latestRelease: async () => ({ tag: "v1", picked_asset_url: "https://x/p.elf" }),
    download: async () => ({ path: "/cache/p.elf" }),
    ...over,
  });

  it("uses the cached copy when present and preferCached (no network)", async () => {
    const latestRelease = vi.fn();
    const download = vi.fn();
    const deps = mkDeps({
      localInventory: async () => [
        { payload_id: "kstuff", path: "/cache/kstuff.elf", version: "v1" },
      ],
      latestRelease,
      download,
    });
    const path = await resolveRepoStepPath("kstuff", deps, true);
    expect(path).toBe("/cache/kstuff.elf");
    expect(latestRelease).not.toHaveBeenCalled();
    expect(download).not.toHaveBeenCalled();
  });

  it("fetches + downloads on a cache miss", async () => {
    const download = vi.fn(async () => ({ path: "/cache/new.elf" }));
    const deps = mkDeps({
      localInventory: async () => [],
      latestRelease: async () => ({ tag: "v2", picked_asset_url: "https://x/new.elf" }),
      download,
    });
    const path = await resolveRepoStepPath("kstuff", deps, true);
    expect(path).toBe("/cache/new.elf");
    expect(download).toHaveBeenCalledWith("kstuff", "https://x/new.elf", "v2");
  });

  it("skips re-download when the cached version matches the latest tag", async () => {
    const download = vi.fn();
    const deps = mkDeps({
      localInventory: async () => [
        { payload_id: "k", path: "/cache/k.elf", version: "v3" },
      ],
      latestRelease: async () => ({ tag: "v3", picked_asset_url: "https://x/k.elf" }),
      download,
    });
    // preferCached=false forces the release check, but same version → no download.
    const path = await resolveRepoStepPath("k", deps, false);
    expect(path).toBe("/cache/k.elf");
    expect(download).not.toHaveBeenCalled();
  });

  it("falls back to a cached copy when the network fetch throws", async () => {
    const deps = mkDeps({
      localInventory: async () => [
        { payload_id: "k", path: "/cache/k.elf", version: "v1" },
      ],
      latestRelease: async () => {
        throw new Error("rate limited");
      },
    });
    const path = await resolveRepoStepPath("k", deps, false);
    expect(path).toBe("/cache/k.elf");
  });

  it("throws when there's no asset AND no cached copy", async () => {
    const deps = mkDeps({
      localInventory: async () => [],
      latestRelease: async () => ({ tag: "v1", picked_asset_url: "" }),
    });
    await expect(resolveRepoStepPath("k", deps, false)).rejects.toThrow(
      /no downloadable payload asset/,
    );
  });

  it("throws when the fetch fails AND nothing is cached", async () => {
    const deps = mkDeps({
      localInventory: async () => [],
      latestRelease: async () => {
        throw new Error("offline");
      },
    });
    await expect(resolveRepoStepPath("k", deps, true)).rejects.toThrow(/offline/);
  });
});
