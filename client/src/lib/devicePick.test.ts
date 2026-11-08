import { beforeEach, describe, expect, it, vi } from "vitest";

import type { BrowserPkgUpload, StageOptions } from "../api/pkgUpload";
import { isStagingCancelled } from "./browserStaging";
import {
  picksFromDrop,
  picksFromInput,
  stagePicks,
  stagePicksWithTask,
  type DropEntry,
  type StagePicksDeps,
} from "./devicePick";
import { useTaskStore } from "../state/tasks";

const file = (name: string, size: number, rel = ""): File => {
  const f = new File([new Uint8Array(size)], name);
  if (rel) Object.defineProperty(f, "webkitRelativePath", { value: rel });
  return f;
};

/** A fake engine: each first file of an upload makes a new id; a folder answers with its root. */
function fakeEngine(opts: { failOn?: string; abortOn?: string; controller?: AbortController } = {}) {
  const calls: { name: string; uploadId?: string; relPath?: string }[] = [];
  let next = 0;
  const deps: StagePicksDeps = {
    stage: vi.fn(async (f: File, o?: StageOptions): Promise<BrowserPkgUpload> => {
      calls.push({ name: f.name, uploadId: o?.uploadId, relPath: o?.relPath });
      if (opts.abortOn === f.name) {
        opts.controller?.abort();
        throw new DOMException("cancelled", "AbortError");
      }
      if (opts.failOn === f.name) throw new Error("no room on the server");
      o?.onProgress?.(f.size, f.size);
      const id = o?.uploadId ?? `u${next++}`;
      const top = o?.relPath?.split("/")[0];
      return {
        uploadId: id,
        path: `/stage/${id}/${o?.relPath ?? f.name}`,
        filename: f.name,
        size: f.size,
        ...(top ? { root: `/stage/${id}/${top}` } : {}),
      };
    }),
    remove: vi.fn(async () => {}),
    note: vi.fn(),
  };
  return { deps, calls };
}

describe("picksFromInput", () => {
  it("takes loose files as files", () => {
    const picks = picksFromInput([file("a.pkg", 3), file("b.bin", 2)]);
    expect(picks.map((p) => p.kind)).toEqual(["file", "file"]);
  });

  it("takes a picked folder as one folder with every nested path", () => {
    const picks = picksFromInput([
      file("eboot.bin", 1, "Game/eboot.bin"),
      file("param.json", 1, "Game/sce_sys/param.json"),
      file("icon0.png", 1, "Game/sce_sys/about/icon0.png"),
    ]);
    expect(picks).toHaveLength(1);
    const p = picks[0];
    expect(p.kind).toBe("folder");
    if (p.kind !== "folder") return;
    expect(p.name).toBe("Game");
    expect(p.files.map((f) => f.relPath)).toEqual([
      "Game/eboot.bin",
      "Game/sce_sys/param.json",
      "Game/sce_sys/about/icon0.png",
    ]);
  });
});

describe("picksFromDrop", () => {
  const fileEntry = (name: string, size = 1): DropEntry => ({
    name,
    isFile: true,
    isDirectory: false,
    file: (ok) => ok(file(name, size)),
  });
  /** `readEntries` answers in batches of one, like a browser with a big folder. */
  const dirEntry = (name: string, children: DropEntry[]): DropEntry => ({
    name,
    isFile: false,
    isDirectory: true,
    createReader: () => {
      const left = [...children];
      return { readEntries: (ok) => ok(left.splice(0, 1)) };
    },
  });

  it("reads a dropped folder through, keeping each file's place", async () => {
    const tree = dirEntry("Game", [
      fileEntry("eboot.bin"),
      dirEntry("sce_sys", [fileEntry("param.json"), dirEntry("empty", [])]),
    ]);
    const picks = await picksFromDrop([
      { entry: tree, file: null },
      { entry: fileEntry("loose.txt"), file: file("loose.txt", 4) },
    ]);
    expect(picks).toHaveLength(2);
    const [folder, loose] = picks;
    expect(folder.kind === "folder" && folder.files.map((f) => f.relPath)).toEqual([
      "Game/eboot.bin",
      "Game/sce_sys/param.json",
    ]);
    expect(loose.kind === "file" && loose.file.name).toBe("loose.txt");
  });
});

describe("stagePicks", () => {
  it("stages single files, one upload each, and notes them for cleanup", async () => {
    const { deps, calls } = fakeEngine();
    const seen: number[] = [];
    const paths = await stagePicks(
      picksFromInput([file("a.pkg", 3), file("b.bin", 2)]),
      new AbortController().signal,
      (sent) => seen.push(sent),
      deps,
    );
    expect(paths).toEqual(["/stage/u0/a.pkg", "/stage/u1/b.bin"]);
    expect(calls.every((c) => c.uploadId === undefined && c.relPath === undefined)).toBe(true);
    expect(deps.note).toHaveBeenCalledTimes(2);
    expect(seen[seen.length - 1]).toBe(5);
  });

  it("stages a folder into one upload with its nested paths and hands on the folder", async () => {
    const { deps, calls } = fakeEngine();
    const paths = await stagePicks(
      picksFromInput([
        file("eboot.bin", 1, "Game/eboot.bin"),
        file("param.json", 1, "Game/sce_sys/param.json"),
      ]),
      new AbortController().signal,
      () => {},
      deps,
    );
    expect(paths).toEqual(["/stage/u0/Game"]);
    expect(calls).toEqual([
      { name: "eboot.bin", uploadId: undefined, relPath: "Game/eboot.bin" },
      { name: "param.json", uploadId: "u0", relPath: "Game/sce_sys/param.json" },
    ]);
    // The folder is what the release watches: its root, under the folder's upload.
    expect(deps.note).toHaveBeenCalledWith(expect.objectContaining({ uploadId: "u0", path: "/stage/u0/Game" }));
  });

  it("removes every copy it made when cancelled part-way, and sends nothing on", async () => {
    const controller = new AbortController();
    const { deps } = fakeEngine({ abortOn: "c.bin", controller });
    const picks = [
      ...picksFromInput([file("loose.pkg", 2)]),
      ...picksFromInput([
        file("a.bin", 1, "Game/a.bin"),
        file("b.bin", 1, "Game/sub/b.bin"),
        file("c.bin", 1, "Game/sub/c.bin"),
      ]),
    ];
    const err = await stagePicks(picks, controller.signal, () => {}, deps).catch((e) => e);
    expect(isStagingCancelled(err)).toBe(true);
    expect(deps.remove).toHaveBeenCalledWith("u0");
    expect(deps.remove).toHaveBeenCalledWith("u1");
    expect(deps.note).not.toHaveBeenCalled();
  });

  it("removes its copies on a failure too, and reports it", async () => {
    const { deps } = fakeEngine({ failOn: "b.bin" });
    const err = await stagePicks(
      picksFromInput([file("a.bin", 1, "Game/a.bin"), file("b.bin", 1, "Game/b.bin")]),
      new AbortController().signal,
      () => {},
      deps,
    ).catch((e) => e);
    expect(String(err)).toContain("no room");
    expect(deps.remove).toHaveBeenCalledWith("u0");
    expect(deps.note).not.toHaveBeenCalled();
  });

  it("leaves out an empty folder", async () => {
    const { deps } = fakeEngine();
    const paths = await stagePicks(
      [{ kind: "folder", name: "Empty", files: [] }],
      new AbortController().signal,
      () => {},
      deps,
    );
    expect(paths).toEqual([]);
    expect(deps.stage).not.toHaveBeenCalled();
  });
});

describe("stagePicksWithTask", () => {
  beforeEach(() => useTaskStore.setState({ tasks: [] }));

  it("is one task for a whole folder", async () => {
    const { deps } = fakeEngine();
    await stagePicksWithTask(
      picksFromInput([file("a.bin", 1, "Game/a.bin"), file("b.bin", 1, "Game/b.bin")]),
      deps,
    );
    const tasks = useTaskStore.getState().tasks;
    expect(tasks).toHaveLength(1);
    expect(tasks[0].label).toContain("Game");
    expect(tasks[0].status).toBe("done");
  });
});
