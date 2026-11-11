import { describe, expect, it } from "vitest";

import { archiveFullyExtracted, listingIsCurrent, planPaste } from "./fsGuards";

describe("listingIsCurrent", () => {
  const now = { host: "172.16.0.10", path: "/data/b" };

  it("accepts a listing of the folder on screen", () => {
    expect(listingIsCurrent({ host: "172.16.0.10", path: "/data/b" }, now)).toBe(true);
  });

  it("drops the listing an operation started in another folder asks for when it ends", () => {
    // Delete began in /data/a; the user is in /data/b by the time it finishes.
    expect(listingIsCurrent({ host: "172.16.0.10", path: "/data/a" }, now)).toBe(false);
  });

  it("drops another console's listing of the same path", () => {
    expect(listingIsCurrent({ host: "172.16.0.20", path: "/data/b" }, now)).toBe(false);
  });

  it("treats no host and an empty host alike", () => {
    expect(listingIsCurrent({ host: null, path: "/" }, { host: "", path: "/" })).toBe(true);
  });
});

describe("archiveFullyExtracted", () => {
  it("is true only when nothing was left in the archive", () => {
    expect(archiveFullyExtracted({ skipped: 0, unsafe: 0, links: 0 })).toBe(true);
  });

  it("keeps the archive when files already there were skipped", () => {
    expect(archiveFullyExtracted({ skipped: 3, unsafe: 0, links: 0 })).toBe(false);
  });

  it("keeps the archive for unsafe paths and links", () => {
    expect(archiveFullyExtracted({ skipped: 0, unsafe: 1, links: 0 })).toBe(false);
    expect(archiveFullyExtracted({ skipped: 0, unsafe: 0, links: 1 })).toBe(false);
  });
});

describe("planPaste", () => {
  const it_ = (path: string, name = path.split("/").pop() ?? path) => ({ path, name });

  it("leaves out an item pasted into the folder it is already in", () => {
    const plan = planPaste([it_("/data/games/a.pkg"), it_("/data/other/b.pkg")], "/data/games");
    expect(plan.same.map((i) => i.path)).toEqual(["/data/games/a.pkg"]);
    expect(plan.ok.map((i) => i.path)).toEqual(["/data/other/b.pkg"]);
    expect(plan.intoItself).toEqual([]);
  });

  it("leaves out a folder pasted into itself or below itself", () => {
    const folder = it_("/data/games");
    expect(planPaste([folder], "/data/games").intoItself).toEqual([folder]);
    expect(planPaste([folder], "/data/games/sub/deeper").intoItself).toEqual([folder]);
  });

  it("does not mistake a sibling with the same prefix for the folder itself", () => {
    expect(planPaste([it_("/data/games")], "/data/games2").ok).toHaveLength(1);
  });

  it("handles the root and a trailing slash", () => {
    expect(planPaste([it_("/a.bin")], "/").same).toHaveLength(1);
    expect(planPaste([it_("/data/a.bin")], "/data/").same).toHaveLength(1);
  });

  it("lets a same-named item from another folder through (a real conflict, asked about)", () => {
    expect(planPaste([it_("/mnt/usb0/a.pkg")], "/data").ok).toHaveLength(1);
  });
});
