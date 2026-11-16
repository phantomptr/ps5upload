import { describe, expect, it } from "vitest";
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
  RepoStepNotSaved,
  resolveRepoStepPath,
  sanitiseSleepMs,
  type Playlist,
  stepSendsOurHelper,
  playlistResendsOurHelper,
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
    ...over,
  });

  it("uses the copy saved in the library", async () => {
    const deps = mkDeps({
      localInventory: async () => [
        { payload_id: "kstuff", path: "/cache/kstuff.elf", version: "v1" },
      ],
    });
    expect(await resolveRepoStepPath("kstuff", deps)).toBe("/cache/kstuff.elf");
  });

  it("never downloads: with no saved copy the step says to pick the file", async () => {
    const deps = mkDeps({
      localInventory: async () => [{ payload_id: "other", path: "/cache/o.elf", version: "v1" }],
    });
    const err = await resolveRepoStepPath("kstuff", deps, "kstuff-lite").catch((e) => e);
    expect(err).toBeInstanceOf(RepoStepNotSaved);
    expect(err.payloadId).toBe("kstuff");
    expect(err.message).toBe("kstuff-lite isn't on this computer: pick the file");
  });

  it("treats an unreadable library as no saved copy", async () => {
    const deps = mkDeps({
      localInventory: async () => {
        throw new Error("io");
      },
    });
    await expect(resolveRepoStepPath("k", deps)).rejects.toBeInstanceOf(RepoStepNotSaved);
  });
});

describe("auto-loader self-send guard", () => {
  const step = (path: string, payloadId?: string): PlaylistStep =>
    ({ path, payloadId }) as PlaylistStep;

  /* Re-sending our own helper on a ready edge takes over the running
   * instance, drops the connection, and produces another ready edge. */
  it("recognises our own helper by name", () => {
    expect(stepSendsOurHelper(step("C:/tools/ps5upload.elf"))).toBe(true);
    expect(stepSendsOurHelper(step("/home/u/ps5upload-wake.elf"))).toBe(true);
    expect(stepSendsOurHelper(step("", "ps5upload"))).toBe(true);
  });

  /* "payload.elf" is what elfldr calls EVERY raw-streamed payload. Treating
   * it as ours would silently skip unrelated homebrew from a user's
   * playlist — the same trap the payload's own reaper avoids. */
  it("does not claim payloads that merely look similar", () => {
    expect(stepSendsOurHelper(step("/x/payload.elf"))).toBe(false);
    expect(stepSendsOurHelper(step("/x/kstuff.elf"))).toBe(false);
    expect(stepSendsOurHelper(step("/x/ps5uploader.elf"))).toBe(false);
    expect(stepSendsOurHelper(step("/x/shadowmountplus.elf"))).toBe(false);
  });

  /* The caller needs the offending step so the log can name it. */
  it("finds the offending step, or reports none", () => {
    expect(
      playlistResendsOurHelper([step("/x/kstuff.elf"), step("/x/ps5upload.elf")])
        ?.path,
    ).toBe("/x/ps5upload.elf");
    expect(playlistResendsOurHelper([step("/x/kstuff.elf")])).toBeNull();
  });
});
