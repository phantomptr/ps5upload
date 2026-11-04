import { describe, expect, it } from "vitest";

import {
  freeKeepBothName,
  keepBothName,
  kindsOf,
  landedOfList,
  namesPreview,
  planFsUpload,
  sendOrder,
  type FsConflict,
  type FsConflictDecision,
  type FsExistingEntry,
  type FsPickedItem,
} from "./fsUploadPlan";

const file = (src: string): FsPickedItem => ({ src, kind: "file" });
const folder = (src: string): FsPickedItem => ({ src, kind: "folder" });
const there = (name: string, kind = "file", size = 10): FsExistingEntry => ({
  name,
  kind,
  size,
});

/** Answers each conflict from a queue, recording what was asked. */
function answers(...queue: (FsConflictDecision | null)[]) {
  const asked: FsConflict[] = [];
  const decide = async (c: FsConflict) => {
    asked.push(c);
    return queue.length ? (queue.shift() as FsConflictDecision | null) : null;
  };
  return { asked, decide };
}

const tx = () => "t".repeat(32);

describe("planFsUpload", () => {
  it("two or more local files become ONE list step; nothing to ask without conflicts", async () => {
    const a = answers();
    const plan = await planFsUpload(
      [file("/pc/a.bin"), file("/pc/b.bin"), file("/pc/c.bin")],
      [there("other.txt")],
      a.decide,
      tx,
    );
    expect(a.asked).toEqual([]);
    expect(plan?.steps).toEqual([
      {
        kind: "list",
        txId: tx(),
        files: [
          { src: "/pc/a.bin", name: "a.bin" },
          { src: "/pc/b.bin", name: "b.bin" },
          { src: "/pc/c.bin", name: "c.bin" },
        ],
      },
    ]);
  });

  it("a single file keeps the single-file job", async () => {
    const plan = await planFsUpload([file("C:\\pc\\a.bin")], [], answers().decide, tx);
    expect(plan?.steps).toEqual([{ kind: "one", src: "C:\\pc\\a.bin", name: "a.bin", txId: tx() }]);
  });

  it("folders stay their own job, in pick order around the list", async () => {
    const plan = await planFsUpload(
      [folder("/pc/Game"), file("/pc/a.bin"), folder("/pc/Mods/"), file("/pc/b.bin")],
      [],
      answers().decide,
      tx,
    );
    expect(plan?.steps.map((s) => (s.kind === "list" ? `list:${s.files.length}` : s.name))).toEqual([
      "Game",
      "list:2",
      "Mods",
    ]);
  });

  it("files on a saved server never go in the list (it reads local disk only)", async () => {
    const plan = await planFsUpload(
      [file("remote://srv1/x/a.bin"), file("/pc/b.bin"), file("/pc/c.bin")],
      [],
      answers().decide,
      tx,
    );
    expect(plan?.steps.map((s) => s.kind)).toEqual(["one", "list"]);
    expect(plan?.steps[0]).toMatchObject({ src: "remote://srv1/x/a.bin" });
  });

  it("Skip leaves the file out; Keep both renames it; Replace overwrites (remembering the old size)", async () => {
    const a = answers(
      { choice: "skip", all: false },
      { choice: "keep", all: false },
      { choice: "replace", all: false },
    );
    const plan = await planFsUpload(
      [file("/pc/s.bin"), file("/pc/k.txt"), file("/pc/r.bin"), file("/pc/new.bin")],
      [there("s.bin"), there("k.txt"), there("k (2).txt"), there("r.bin", "file", 77)],
      a.decide,
      tx,
    );
    expect(a.asked.map((c) => [c.name, c.canReplace, c.left])).toEqual([
      ["s.bin", true, 2],
      ["k.txt", true, 1],
      ["r.bin", true, 0],
    ]);
    expect(plan?.skipped).toEqual(["/pc/s.bin"]);
    expect(plan?.steps).toEqual([
      {
        kind: "list",
        txId: tx(),
        files: [
          { src: "/pc/k.txt", name: "k (3).txt" },
          { src: "/pc/r.bin", name: "r.bin", replacesSize: 77 },
          { src: "/pc/new.bin", name: "new.bin" },
        ],
      },
    ]);
  });

  it("Apply to all answers the later conflicts without asking", async () => {
    const a = answers({ choice: "skip", all: true });
    const plan = await planFsUpload(
      [file("/pc/a"), file("/pc/b"), file("/pc/c")],
      [there("a"), there("b"), there("c")],
      a.decide,
      tx,
    );
    expect(a.asked).toHaveLength(1);
    expect(plan?.steps).toEqual([]);
    expect(plan?.skipped).toHaveLength(3);
  });

  it("Replace-for-all still asks where a file and a folder meet (neither can replace the other)", async () => {
    const a = answers({ choice: "replace", all: true }, { choice: "keep", all: false });
    const plan = await planFsUpload(
      [file("/pc/a"), file("/pc/Data"), file("/pc/c")],
      [there("a"), there("Data", "dir"), there("c")],
      a.decide,
      tx,
    );
    expect(a.asked.map((c) => [c.name, c.canReplace, c.existingDir])).toEqual([
      ["a", true, false],
      ["Data", false, true],
    ]);
    const list = plan?.steps[0];
    expect(list?.kind === "list" && list.files.map((f) => f.name)).toEqual(["a", "Data (2)", "c"]);
  });

  it("a folder onto a folder: Replace merges into it (the folder job, same name)", async () => {
    const plan = await planFsUpload(
      [folder("/pc/Game")],
      [there("Game", "dir")],
      answers({ choice: "replace", all: false }).decide,
      tx,
    );
    expect(plan?.steps).toEqual([{ kind: "one", src: "/pc/Game", name: "Game", txId: tx() }]);
  });

  it("Keep both keeps a folder's dots", async () => {
    const plan = await planFsUpload(
      [folder("/pc/v1.2")],
      [there("v1.2", "dir")],
      answers({ choice: "keep", all: false }).decide,
      tx,
    );
    expect(plan?.steps).toEqual([{ kind: "one", src: "/pc/v1.2", name: "v1.2 (2)", txId: tx() }]);
  });

  it("two picked files with one name conflict with each other; Replace keeps the later one", async () => {
    const a = answers({ choice: "replace", all: false });
    const plan = await planFsUpload(
      [file("/pc/x/a.bin"), file("/pc/y/a.bin"), file("/pc/b.bin")],
      [],
      a.decide,
      tx,
    );
    expect(a.asked.map((c) => c.name)).toEqual(["a.bin"]);
    const list = plan?.steps[0];
    expect(list?.kind === "list" && list.files).toEqual([
      { src: "/pc/y/a.bin", name: "a.bin" },
      { src: "/pc/b.bin", name: "b.bin" },
    ]);
  });

  it("Keep both between two picks avoids every name taken, on the console and in the plan", async () => {
    const plan = await planFsUpload(
      [file("/pc/x/a.bin"), file("/pc/y/a.bin")],
      [there("a (2).bin")],
      answers({ choice: "keep", all: false }).decide,
      tx,
    );
    const list = plan?.steps[0];
    expect(list?.kind === "list" && list.files.map((f) => f.name)).toEqual(["a.bin", "a (3).bin"]);
  });

  it("a name that differs only in case or Unicode form is a conflict", async () => {
    const a = answers({ choice: "keep", all: true });
    const plan = await planFsUpload(
      [file("/pc/README.TXT"), file("/pc/Cafe\u0301.bin"), file("/pc/new.bin")],
      [there("readme.txt"), there("Caf\u00e9.bin"), there("readme (2).TXT")],
      a.decide,
      tx,
    );
    expect(a.asked.map((c) => c.name)).toEqual(["README.TXT"]);
    const list = plan?.steps[0];
    // Keep both skips "readme (2).TXT" too: free means free whatever the case.
    expect(list?.kind === "list" && list.files.map((f) => f.name)).toEqual([
      "README (3).TXT",
      "Cafe\u0301 (2).bin",
      "new.bin",
    ]);
  });

  it("Replace of a case-only twin lands under the name that is there", async () => {
    const plan = await planFsUpload(
      [file("/pc/EBOOT.BIN")],
      [there("eboot.bin", "file", 5)],
      answers({ choice: "replace", all: false }).decide,
      tx,
    );
    expect(plan?.steps).toEqual([{ kind: "one", src: "/pc/EBOOT.BIN", name: "eboot.bin", txId: tx() }]);
  });

  it("two picks differing only in case clash with each other", async () => {
    const a = answers({ choice: "replace", all: false });
    const plan = await planFsUpload(
      [file("/pc/x/Data.bin"), file("/pc/y/data.BIN"), file("/pc/z.bin")],
      [],
      a.decide,
      tx,
    );
    expect(a.asked.map((c) => c.name)).toEqual(["data.BIN"]);
    const list = plan?.steps[0];
    expect(list?.kind === "list" && list.files).toEqual([
      { src: "/pc/y/data.BIN", name: "Data.bin" },
      { src: "/pc/z.bin", name: "z.bin" },
    ]);
  });

  it("Cancel at a conflict sends nothing", async () => {
    const plan = await planFsUpload([file("/pc/a"), file("/pc/b")], [there("a")], answers(null).decide, tx);
    expect(plan).toBeNull();
  });
});

describe("keep both names", () => {
  it("numbers before the extension, and counts on from an existing number", () => {
    expect(keepBothName("a.txt", 2, false)).toBe("a (2).txt");
    expect(keepBothName("archive.tar.gz", 2, false)).toBe("archive.tar (2).gz");
    expect(keepBothName(".bashrc", 2, false)).toBe(".bashrc (2)");
    expect(keepBothName("v1.2", 2, true)).toBe("v1.2 (2)");
    expect(freeKeepBothName("a (2).txt", false, () => false)).toBe("a (3).txt");
  });
});

describe("kindsOf", () => {
  it("trusts the picker's kind and asks nothing", async () => {
    let asked = 0;
    const r = await kindsOf(["/a", "/b"], async () => (asked++, "folder"), "file");
    expect(asked).toBe(0);
    expect(r.map((x) => x.kind)).toEqual(["file", "file"]);
  });

  it("asks about each dropped path, in parallel, keeping the order; a failed ask is a file", async () => {
    let inFlight = 0;
    let peak = 0;
    const r = await kindsOf(
      ["/f", "/dir", "/gone", "/err"],
      async (p) => {
        inFlight++;
        peak = Math.max(peak, inFlight);
        await new Promise((res) => setTimeout(res, 1));
        inFlight--;
        if (p === "/err") throw new Error("boom");
        return p === "/dir" ? "folder" : p === "/gone" ? "missing" : "file";
      },
      undefined,
      2,
    );
    expect(r.map((x) => x.kind)).toEqual(["file", "folder", "missing", "file"]);
    expect(peak).toBe(2);
  });
});

describe("landedOfList", () => {
  const files = [
    { src: "/a", name: "a" },
    { src: "/b", name: "b" },
    { src: "/r", name: "r", replacesSize: 5 },
    { src: "/s", name: "s", replacesSize: 9 },
  ];

  it("landed = in the folder with the planned size; a same-size Replace is not claimed", () => {
    const r = landedOfList(
      files,
      [there("a", "file", 1), there("b", "file", 999), there("r", "file", 7), there("s", "file", 9)],
      new Map([
        ["a", 1],
        ["b", 2],
        ["r", 7],
        ["s", 9],
      ]),
    );
    expect(r.landed.map((f) => f.name)).toEqual(["a", "r"]);
    expect(r.notLanded.map((f) => f.name)).toEqual(["b", "s"]);
  });

  it("without the planned sizes only new names count, by presence", () => {
    const r = landedOfList(files, [there("a"), there("r", "file", 7)], null);
    expect(r.landed.map((f) => f.name)).toEqual(["a"]);
  });

  it("no listing: nothing is claimed", () => {
    expect(landedOfList(files, null, null).notLanded).toHaveLength(4);
  });
});

describe("helpers", () => {
  it("sendOrder sorts bytewise, the engine's order", () => {
    expect(sendOrder([{ src: "", name: "b" }, { src: "", name: "B" }, { src: "", name: "a" }])).toEqual([
      "B",
      "a",
      "b",
    ]);
  });

  it("namesPreview shows a few and counts the rest", () => {
    expect(namesPreview(["a", "b"])).toBe("a, b");
    expect(namesPreview(["a", "b", "c"], 2)).toBe("a, b, … (+1)");
  });
});
