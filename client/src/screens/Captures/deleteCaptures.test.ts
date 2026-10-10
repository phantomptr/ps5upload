import { describe, expect, it, vi } from "vitest";

import { captureDeletePaths, deleteCaptures } from "./deleteCaptures";

describe("captureDeletePaths", () => {
  it("adds the thumbnail of a full-size screenshot", () => {
    expect(captureDeletePaths("/user/av_contents/photo/1/1/b/shot.jxr")).toEqual([
      "/user/av_contents/photo/1/1/b/shot.jxr",
      "/user/av_contents/thumbnails/photo/1/1/b/shot.jxr.jxr",
    ]);
  });

  it("leaves thumbnails, jpegs and video clips alone", () => {
    for (const p of [
      "/user/av_contents/thumbnails/photo/1/1/b/shot.jxr.jxr",
      "/user/av_contents/photo/1/1/b/shot.jpg",
      "/user/av_contents/video/1/1/b/clip.webm",
    ]) {
      expect(captureDeletePaths(p)).toEqual([p]);
    }
  });
});

describe("deleteCaptures", () => {
  it("deletes each capture and its thumbnail, and reports what is gone", async () => {
    const deleter = vi.fn(async (_p: string) => {});
    const r = await deleteCaptures(
      ["/user/av_contents/photo/a.jxr", "/user/av_contents/video/c.webm"],
      deleter,
    );
    expect(deleter.mock.calls.map((c) => c[0])).toEqual([
      "/user/av_contents/photo/a.jxr",
      "/user/av_contents/thumbnails/photo/a.jxr.jxr",
      "/user/av_contents/video/c.webm",
    ]);
    expect(r).toEqual({
      deleted: ["/user/av_contents/photo/a.jxr", "/user/av_contents/video/c.webm"],
      failed: [],
    });
  });

  it("a missing thumbnail is not a failure", async () => {
    const deleter = vi.fn(async (p: string) => {
      if (p.includes("thumbnails")) throw new Error("not found");
    });
    const r = await deleteCaptures(["/user/av_contents/photo/a.jxr"], deleter);
    expect(r.deleted).toEqual(["/user/av_contents/photo/a.jxr"]);
    expect(r.failed).toEqual([]);
  });

  it("keeps going past a failure and reports it", async () => {
    const deleter = vi.fn(async (p: string) => {
      if (p.endsWith("b.webm")) throw new Error("busy");
    });
    const r = await deleteCaptures(
      ["/user/av_contents/video/b.webm", "/user/av_contents/video/c.webm"],
      deleter,
    );
    expect(r.deleted).toEqual(["/user/av_contents/video/c.webm"]);
    expect(r.failed).toEqual([{ path: "/user/av_contents/video/b.webm", error: "busy" }]);
  });
});
