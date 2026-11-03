import { describe, expect, it, vi } from "vitest";

const store = new Map<string, string>();
vi.mock("./safeStorage", () => ({
  safeGetItem: (k: string) => store.get(k) ?? null,
  safeSetItem: (k: string, v: string) => void store.set(k, v),
}));

import { loadCorner, miniPlayerVisible, nearestCorner, saveCorner } from "./miniPlayer";

const base = {
  streaming: true,
  phase: "playing",
  pathname: "/games",
  onScreen: false,
  pip: null,
  androidPip: false,
  enabled: true,
  dismissed: false,
};

describe("miniPlayerVisible", () => {
  it("shows the stream on another screen", () => {
    expect(miniPlayerVisible(base)).toBe(true);
    // Still connecting: it shows, with the progress.
    expect(miniPlayerVisible({ ...base, phase: "connecting" })).toBe(true);
  });
  it("never while the Play screen shows the stream", () => {
    expect(miniPlayerVisible({ ...base, pathname: "/play" })).toBe(false);
    expect(miniPlayerVisible({ ...base, onScreen: true })).toBe(false);
  });
  it("never while a picture-in-picture window or Android's has the picture", () => {
    for (const pip of ["popup", "document", "video"]) expect(miniPlayerVisible({ ...base, pip })).toBe(false);
    expect(miniPlayerVisible({ ...base, androidPip: true })).toBe(false);
  });
  it("not without a stream, once it ended, or while another window plays it", () => {
    expect(miniPlayerVisible({ ...base, streaming: false })).toBe(false);
    expect(miniPlayerVisible({ ...base, phase: null })).toBe(false);
    expect(miniPlayerVisible({ ...base, phase: "ended" })).toBe(false);
    expect(miniPlayerVisible({ ...base, phase: "moved" })).toBe(false);
  });
  it("not when turned off in Settings or closed with ×", () => {
    expect(miniPlayerVisible({ ...base, enabled: false })).toBe(false);
    expect(miniPlayerVisible({ ...base, dismissed: true })).toBe(false);
  });
});

describe("corners", () => {
  it("snaps to the corner nearest to the drop", () => {
    expect(nearestCorner(10, 10, 400, 800)).toBe("tl");
    expect(nearestCorner(390, 10, 400, 800)).toBe("tr");
    expect(nearestCorner(10, 790, 400, 800)).toBe("bl");
    expect(nearestCorner(390, 790, 400, 800)).toBe("br");
  });
  it("remembers the corner, bottom-right until one is chosen", () => {
    expect(loadCorner()).toBe("br");
    saveCorner("tl");
    expect(loadCorner()).toBe("tl");
    store.set("ps5upload.rp_mini_corner.v1", "middle");
    expect(loadCorner()).toBe("br");
  });
});
