import { describe, expect, it } from "vitest";

import type { ImageEdit } from "../../api/fpkg";
import {
  baseName,
  crumbs,
  editKey,
  folderOf,
  joinPath,
  nameProblem,
  parentDir,
  pathAfter,
  pendingState,
  queueEdit,
} from "./browseEdits";

describe("image browser paths", () => {
  it("joins, splits and climbs paths inside an image", () => {
    expect(joinPath("", "eboot.bin")).toBe("eboot.bin");
    expect(joinPath("data/sub", "a.bin")).toBe("data/sub/a.bin");
    expect(parentDir("data/sub")).toBe("data");
    expect(parentDir("data")).toBe("");
    expect(parentDir("")).toBe("");
    expect(crumbs("data/sub")).toEqual([
      { name: "data", path: "data" },
      { name: "sub", path: "data/sub" },
    ]);
    expect(crumbs("")).toEqual([]);
  });

  it("names files on this computer on any OS", () => {
    expect(baseName("/Users/me/patch.bin")).toBe("patch.bin");
    expect(baseName("C:\\games\\mod\\")).toBe("mod");
    expect(folderOf("C:\\out\\eboot.bin")).toBe("C:\\out");
    expect(folderOf("/out/data")).toBe("/out");
  });
});

describe("queued changes for an edited copy", () => {
  const put = (path: string, from = `/local/${path}`): ImageEdit => ({ op: "put", path, from });
  const del = (path: string): ImageEdit => ({ op: "delete", path });

  it("marks what a change touches, the last change winning", () => {
    const edits = [del("data"), put("eboot.bin")];
    expect(pendingState(edits, "data")).toBe("deleted");
    expect(pendingState(edits, "data/sub/a.bin")).toBe("deleted");
    expect(pendingState(edits, "data2")).toBe(null);
    expect(pendingState(edits, "eboot.bin")).toBe("replaced");
    // A folder put back after its deletion is no longer deleted.
    expect(pendingState([del("data"), put("data")], "data/a.bin")).toBe(null);
  });

  it("drops changes a later one makes pointless", () => {
    let edits = queueEdit([], put("eboot.bin", "/a"));
    edits = queueEdit(edits, put("eboot.bin", "/b"));
    expect(edits).toEqual([put("eboot.bin", "/b")]);
    edits = queueEdit(edits, put("data/new.bin"));
    edits = queueEdit(edits, del("data"));
    expect(edits).toEqual([put("eboot.bin", "/b"), del("data")]);
  });

  const ren = (from: string, to: string): ImageEdit => ({ op: "rename", from, to });
  const mkdir = (path: string): ImageEdit => ({ op: "mkdir", path });

  it("marks a renamed entry and everything in a renamed folder", () => {
    const edits = [ren("data", "assets"), ren("eboot.bin", "boot.bin")];
    expect(pendingState(edits, "data")).toBe("renamed");
    expect(pendingState(edits, "data/sub/a.bin")).toBe("renamed");
    expect(pendingState(edits, "data2")).toBe(null);
    expect(pendingState(edits, "eboot.bin")).toBe("renamed");
    expect(pathAfter(edits, "data/sub/a.bin")).toBe("assets/sub/a.bin");
    expect(pathAfter(edits, "eboot.bin")).toBe("boot.bin");
    expect(pathAfter(edits, "data2/x")).toBe("data2/x");
    // Renamed twice, it is where the last rename left it.
    expect(pathAfter([ren("a", "b"), ren("b/x", "c")], "a/x")).toBe("c");
    // A name freed by a delete and taken by a rename holds something else now.
    expect(pendingState([del("a.bin"), ren("b.bin", "a.bin")], "a.bin")).toBe("replaced");
    // A folder made again after its deletion is no longer deleted; what was in it still is.
    expect(pendingState([del("data"), mkdir("data")], "data")).toBe(null);
    expect(pendingState([del("data"), mkdir("data")], "data/a.bin")).toBe("deleted");
  });

  it("keeps every change queued before a rename", () => {
    // The first put is the file the rename moved: dropping it would move the image's own.
    let edits = [put("eboot.bin", "/a"), ren("eboot.bin", "old.bin")];
    edits = queueEdit(edits, put("eboot.bin", "/b"));
    expect(edits).toEqual([put("eboot.bin", "/a"), ren("eboot.bin", "old.bin"), put("eboot.bin", "/b")]);
    edits = queueEdit(edits, put("eboot.bin", "/c"));
    expect(edits).toEqual([put("eboot.bin", "/a"), ren("eboot.bin", "old.bin"), put("eboot.bin", "/c")]);
    // A rename or a new folder is only ever added.
    expect(queueEdit([put("mods/x.bin")], mkdir("mods/empty"))).toEqual([put("mods/x.bin"), mkdir("mods/empty")]);
    expect(queueEdit([mkdir("mods")], del("mods"))).toEqual([mkdir("mods"), del("mods")]);
  });

  it("refuses a name that is empty or a path", () => {
    expect(nameProblem("  ")).toBe("empty");
    expect(nameProblem("a/b")).toBe("separator");
    expect(nameProblem("a\\b")).toBe("separator");
    expect(nameProblem("..")).toBe("separator");
    expect(nameProblem("mods v2")).toBe(null);
    expect(editKey(ren("a", "b"), 3)).toBe("rename:a:b:3");
    expect(editKey(del("a"), 0)).toBe("delete:a:0");
  });
});
