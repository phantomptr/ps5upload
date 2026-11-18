import { describe, expect, it } from "vitest";

import { childToReveal } from "./revealChild";

describe("childToReveal", () => {
  it("going up one level gives the folder left", () => {
    expect(childToReveal("/data/games/CUSA1", "/data/games")).toBe("CUSA1");
  });
  it("going up several levels gives the child on the way", () => {
    expect(childToReveal("/data/games/CUSA1/sce_sys", "/data")).toBe("games");
  });
  it("works from the root", () => {
    expect(childToReveal("/data", "/")).toBe("data");
    expect(childToReveal("/mnt/usb0/x", "/")).toBe("mnt");
  });
  it("ignores trailing and doubled separators", () => {
    expect(childToReveal("/data/games/", "/data/")).toBe("games");
    expect(childToReveal("/data//games", "/data")).toBe("games");
  });
  it("works with Windows paths", () => {
    expect(childToReveal("C:\\Users\\me\\Games", "C:\\Users")).toBe("me");
    expect(childToReveal("C:\\Users", "C:\\")).toBe("Users");
  });
  it("is null going into a folder, sideways, or to the same folder", () => {
    expect(childToReveal("/data", "/data/games")).toBeNull();
    expect(childToReveal("/data/games", "/data/gamesX")).toBeNull();
    expect(childToReveal("/data/gamesX", "/data/games")).toBeNull();
    expect(childToReveal("/data", "/data")).toBeNull();
    expect(childToReveal("/data/a", "/mnt")).toBeNull();
    expect(childToReveal(null, "/data")).toBeNull();
    expect(childToReveal("/data", null)).toBeNull();
  });
});
