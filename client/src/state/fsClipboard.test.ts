import { describe, expect, it } from "vitest";

import { useFsClipboardStore, type ClipboardItem } from "./fsClipboard";
import { useConnectionStore } from "./connection";
import { isAtOrUnder, notifyPathsGone } from "../lib/fsPathsGone";

const item = (path: string): ClipboardItem => ({
  path,
  name: path.split("/").pop() ?? path,
  size: 0,
});

describe("fsClipboard — per-console stash on switchToHost", () => {
  const clip = () => useFsClipboardStore.getState();
  // switchToHost reads the OLD host from the connection store (it runs before
  // connection.setHost), so set the connection host first to mirror the roster.
  const setActiveHost = (h: string) => useConnectionStore.setState({ host: h });

  it("keeps each console's cut/copy across a round-trip switch", () => {
    // Console A: cut a file.
    setActiveHost("172.16.0.10");
    clip().set("cut", [item("/data/a.bin")], "A");

    // Switch to B — A's clipboard is stashed, B starts empty.
    clip().switchToHost("172.16.0.20");
    expect(clip().items).toHaveLength(0);
    expect(clip().op).toBeNull();

    // B (connection follows), copy something there.
    setActiveHost("172.16.0.20");
    clip().set("copy", [item("/data/b.bin")], "B");

    // Back to A → A's cut restored.
    clip().switchToHost("172.16.0.10");
    expect(clip().op).toBe("cut");
    expect(clip().items[0]?.path).toBe("/data/a.bin");

    // A → B → B's copy restored (not lost).
    setActiveHost("172.16.0.10");
    clip().switchToHost("172.16.0.20");
    expect(clip().op).toBe("copy");
    expect(clip().items[0]?.path).toBe("/data/b.bin");
  });

  it("is a no-op for the same console (port-stripped)", () => {
    setActiveHost("172.16.9.9");
    clip().set("cut", [item("/keep.bin")], "X");
    clip().switchToHost("172.16.9.9:9114");
    expect(clip().items[0]?.path).toBe("/keep.bin");
  });
});

describe("fsClipboard — clearFor clears the clipboard of the console that pasted", () => {
  const clip = () => useFsClipboardStore.getState();
  const setActiveHost = (h: string) => useConnectionStore.setState({ host: h });

  it("a paste that ends on a hidden console leaves the selected console's clipboard alone", () => {
    setActiveHost("172.16.1.10");
    const cutOnA = [item("/data/a.bin")];
    clip().set("cut", cutOnA, "A");
    // The user switches to B while A's paste runs, and cuts something there.
    clip().switchToHost("172.16.1.20");
    setActiveHost("172.16.1.20");
    clip().set("cut", [item("/data/b.bin")], "B");

    clip().clearFor("172.16.1.10", cutOnA);
    expect(clip().items[0]?.path).toBe("/data/b.bin");

    // And A's own clipboard is gone: back on A there is nothing left to paste twice.
    clip().switchToHost("172.16.1.10");
    expect(clip().items).toHaveLength(0);
  });

  it("clears the selected console's clipboard when that is the one that pasted", () => {
    setActiveHost("172.16.2.10");
    const cut = [item("/data/a.bin")];
    clip().set("cut", cut, "A");
    clip().clearFor("172.16.2.10:9114", cut);
    expect(clip().op).toBeNull();
  });

  it("keeps a newer cut made while the paste ran", () => {
    setActiveHost("172.16.3.10");
    const pasted = [item("/data/old.bin")];
    clip().set("cut", pasted, "A");
    clip().set("cut", [item("/data/new.bin")], "A");
    clip().clearFor("172.16.3.10", pasted);
    expect(clip().items[0]?.path).toBe("/data/new.bin");
  });
});

describe("fsClipboard — staged items leave once pasted or gone (#936)", () => {
  const clip = () => useFsClipboardStore.getState();
  const setActiveHost = (h: string) => useConnectionStore.setState({ host: h });

  it("a paste that went through empties the clipboard, a copy as well as a cut", () => {
    setActiveHost("172.16.3.10");
    for (const op of ["copy", "cut"] as const) {
      clip().set(op, [item("/data/a.bin"), item("/data/b")], "/data");
      clip().drop("172.16.3.10", ["/data/a.bin", "/data/b"]);
      expect(clip().items).toEqual([]);
      expect(clip().op).toBeNull();
      expect(clip().sourceLabel).toBeNull();
    }
  });

  it("what failed to paste stays staged, with its op and source", () => {
    setActiveHost("172.16.3.10");
    clip().set("cut", [item("/data/a.bin"), item("/data/b.bin")], "/data");
    clip().drop("172.16.3.10:9113", ["/data/a.bin"]);
    expect(clip().items.map((i) => i.path)).toEqual(["/data/b.bin"]);
    expect(clip().op).toBe("cut");
    expect(clip().sourceLabel).toBe("/data");
  });

  it("a source deleted or moved away by anything leaves it, a folder taking what is inside", () => {
    setActiveHost("172.16.3.11");
    clip().set("copy", [item("/data/games/x.pkg"), item("/data/gamesX/y.pkg"), item("/data/z.bin")], "/data");
    notifyPathsGone("172.16.3.11:9113", ["/data/games/"]);
    // /data/gamesX is not inside /data/games.
    expect(clip().items.map((i) => i.path)).toEqual(["/data/gamesX/y.pkg", "/data/z.bin"]);
    // Something inside a staged folder going away leaves the folder staged.
    clip().set("copy", [item("/data/dir")], "/data");
    notifyPathsGone("172.16.3.11", ["/data/dir/inner.bin"]);
    expect(clip().items.map((i) => i.path)).toEqual(["/data/dir"]);
    notifyPathsGone("172.16.3.11", ["/data/dir"]);
    expect(clip().items).toEqual([]);
  });

  it("another console's paths leave this console's clipboard alone, and reach its own", () => {
    setActiveHost("172.16.3.20");
    clip().set("cut", [item("/data/a.bin")], "/data");
    clip().switchToHost("172.16.3.21");
    setActiveHost("172.16.3.21");
    clip().set("copy", [item("/data/a.bin")], "/data");
    // The first console's cut went through while the second is on show.
    notifyPathsGone("172.16.3.20", ["/data/a.bin"]);
    expect(clip().items.map((i) => i.path)).toEqual(["/data/a.bin"]);
    expect(clip().op).toBe("copy");
    clip().switchToHost("172.16.3.20");
    expect(clip().items).toEqual([]);
  });
});

describe("isAtOrUnder", () => {
  it("matches the path itself and what is inside it, not a sibling with a longer name", () => {
    expect(isAtOrUnder("/data/a", "/data/a")).toBe(true);
    expect(isAtOrUnder("/data/a/b", "/data/a/")).toBe(true);
    expect(isAtOrUnder("/data/ab", "/data/a")).toBe(false);
    expect(isAtOrUnder("/data", "/data/a")).toBe(false);
    expect(isAtOrUnder("/x", "/")).toBe(true);
  });
});
