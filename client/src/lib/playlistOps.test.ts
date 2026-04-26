import { describe, expect, it } from "vitest";
import {
  appendStep,
  moveStepDown,
  moveStepUp,
  patchPlaylist,
  removePlaylist,
  removeStep,
  sanitiseSleepMs,
  type Playlist,
  type PlaylistStep,
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
