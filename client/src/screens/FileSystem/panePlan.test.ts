import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { installWindowStub } from "../../test-utils/storage";
import {
  copyLooksWhole,
  namesAlreadyThere,
  paneRoute,
  readOtherPane,
  readTwoPanes,
  runPaneTransfer,
  TWO_PANES_KEY,
  writeOtherPane,
  writeTwoPanes,
  type CopyOutcome,
} from "./panePlan";
import type { Place } from "../../lib/placeList";

const computer: Place = { kind: "computer" };
const server: Place = { kind: "server", id: "nas" };
const otherConsole: Place = { kind: "console", host: "192.168.0.6" };

describe("which transfer each pair of places uses (#952)", () => {
  it("PS5 → this computer is Download, PS5 → a saved server is Download to the server", () => {
    expect(paneRoute(computer, "other", "copy")).toEqual({ ok: true, via: "download" });
    expect(paneRoute(server, "other", "copy")).toEqual({ ok: true, via: "download-server" });
  });

  it("this computer or a saved server → PS5 is Add files", () => {
    expect(paneRoute(computer, "ps5", "copy")).toEqual({ ok: true, via: "upload" });
    expect(paneRoute(server, "ps5", "copy")).toEqual({ ok: true, via: "upload" });
  });

  it("console ↔ console is the send to another console, both ways, copy and move", () => {
    for (const toward of ["other", "ps5"] as const)
      for (const op of ["copy", "move"] as const)
        expect(paneRoute(otherConsole, toward, op)).toEqual({ ok: true, via: "console-send" });
  });

  it("files move off the PS5 to this computer and to a saved server", () => {
    expect(paneRoute(computer, "other", "move")).toEqual({ ok: true, via: "download" });
    // To a saved server the engine checks the copy and deletes the source itself (#953).
    expect(paneRoute(server, "other", "move")).toEqual({ ok: true, via: "download-server", engineMoves: true });
    expect(paneRoute(server, "other", "copy")).toEqual({ ok: true, via: "download-server" });
  });

  it("a folder moves only where every file of its copy is checked: to a saved server", () => {
    expect(paneRoute(server, "other", "move", true)).toEqual({ ok: true, via: "download-server", engineMoves: true });
    expect(paneRoute(computer, "other", "move", true)).toEqual({ ok: false, block: "files-only" });
    expect(paneRoute(otherConsole, "other", "move", true)).toEqual({ ok: false, block: "files-only" });
    expect(paneRoute(otherConsole, "ps5", "move", true)).toEqual({ ok: false, block: "files-only" });
    // Copying a folder is fine everywhere.
    expect(paneRoute(computer, "other", "copy", true)).toEqual({ ok: true, via: "download" });
    expect(paneRoute(otherConsole, "ps5", "copy", true)).toEqual({ ok: true, via: "console-send" });
  });

  it("no move from this computer or a saved server: nothing in the app deletes files there", () => {
    expect(paneRoute(computer, "ps5", "move")).toEqual({ ok: false, block: "no-delete-computer" });
    expect(paneRoute(server, "ps5", "move")).toEqual({ ok: false, block: "no-delete-server" });
    expect(paneRoute(computer, "ps5", "move", true)).toEqual({ ok: false, block: "no-delete-computer" });
  });
});

type Item = { name: string };
const a: Item = { name: "a" };
const b: Item = { name: "b" };
const c: Item = { name: "c" };

function recorder(outcomes: Record<string, CopyOutcome>, whole: Record<string, boolean> = {}) {
  const log: string[] = [];
  return {
    log,
    deps: {
      copy: vi.fn(async (i: Item) => {
        log.push(`copy ${i.name}`);
        return outcomes[i.name] ?? "done";
      }),
      verify: vi.fn(async (i: Item) => {
        log.push(`verify ${i.name}`);
        return whole[i.name] ?? true;
      }),
      removeSource: vi.fn(async (i: Item) => {
        log.push(`delete ${i.name}`);
      }),
    },
  };
}

describe("runPaneTransfer: a move is copy, check, then delete", () => {
  it("a copy never checks or deletes anything", async () => {
    const r = recorder({});
    const out = await runPaneTransfer([a, b], "copy", r.deps);
    expect(r.log).toEqual(["copy a", "copy b"]);
    expect(out.finished).toEqual([a, b]);
    expect(r.deps.removeSource).not.toHaveBeenCalled();
  });

  it("a move deletes each source only after its own copy finished and was checked", async () => {
    const r = recorder({});
    const out = await runPaneTransfer([a, b], "move", r.deps);
    expect(r.log).toEqual(["copy a", "verify a", "delete a", "copy b", "verify b", "delete b"]);
    expect(out.finished).toEqual([a, b]);
  });

  it("a failed copy keeps its source, and the next item still runs", async () => {
    const r = recorder({ a: "failed" });
    const out = await runPaneTransfer([a, b], "move", r.deps);
    expect(r.log).toEqual(["copy a", "copy b", "verify b", "delete b"]);
    expect(out.failed).toEqual([a]);
    expect(out.finished).toEqual([b]);
  });

  it("a copy that throws counts as failed and keeps its source", async () => {
    const r = recorder({});
    r.deps.copy.mockRejectedValueOnce(new Error("engine gone"));
    const out = await runPaneTransfer([a], "move", r.deps);
    expect(out.failed).toEqual([a]);
    expect(r.deps.removeSource).not.toHaveBeenCalled();
  });

  it("a copy that can't be found whole keeps its source", async () => {
    const r = recorder({}, { a: false });
    const out = await runPaneTransfer([a], "move", r.deps);
    expect(out.unverified).toEqual([a]);
    expect(r.deps.removeSource).not.toHaveBeenCalled();
  });

  it("a check that throws keeps the source", async () => {
    const r = recorder({});
    r.deps.verify.mockRejectedValueOnce(new Error("listing failed"));
    const out = await runPaneTransfer([a], "move", r.deps);
    expect(out.unverified).toEqual([a]);
    expect(r.deps.removeSource).not.toHaveBeenCalled();
  });

  it("a stop ends the run: the stopped item and the ones after it are left alone", async () => {
    const r = recorder({ b: "stopped" });
    const out = await runPaneTransfer([a, b, c], "move", r.deps);
    expect(r.log).toEqual(["copy a", "verify a", "delete a", "copy b"]);
    expect(out.stopped).toBe(true);
    expect(out.finished).toEqual([a]);
  });

  it("a source that won't delete is reported, the copy stays", async () => {
    const r = recorder({});
    r.deps.removeSource.mockRejectedValueOnce(new Error("busy"));
    const out = await runPaneTransfer([a, b], "move", r.deps);
    expect(out.notRemoved).toEqual([a]);
    expect(out.finished).toEqual([b]);
  });
});

describe("checking a copy", () => {
  it("a file needs its name and its size", () => {
    expect(copyLooksWhole({ isDir: false, size: 10 }, { is_dir: false, size: 10 })).toBe(true);
    expect(copyLooksWhole({ isDir: false, size: 10 }, { is_dir: false, size: 9 })).toBe(false);
    expect(copyLooksWhole({ isDir: false, size: 10 }, null)).toBe(false);
  });

  it("a folder needs to be there as a folder", () => {
    expect(copyLooksWhole({ isDir: true, size: 0 }, { is_dir: true, size: 4096 })).toBe(true);
    expect(copyLooksWhole({ isDir: true, size: 0 }, { is_dir: false, size: 0 })).toBe(false);
  });

  it("names already in the destination are found", () => {
    expect(namesAlreadyThere(["a", "b", "c"], ["c", "x", "a"])).toEqual(["a", "c"]);
  });
});

describe("remembered per device", () => {
  beforeEach(() => installWindowStub());
  afterEach(() => vi.unstubAllGlobals());

  it("the toggle is off until turned on, and stays as set", () => {
    expect(readTwoPanes()).toBe(false);
    writeTwoPanes(true);
    expect(window.localStorage.getItem(TWO_PANES_KEY)).toBe("1");
    expect(readTwoPanes()).toBe(true);
    writeTwoPanes(false);
    expect(readTwoPanes()).toBe(false);
  });

  it("the other side's place and folder come back; a broken memory is no memory", () => {
    writeOtherPane({ place: server, path: "/games" });
    expect(readOtherPane()).toEqual({ place: server, path: "/games" });
    window.localStorage.setItem("ps5upload.files.otherPane", "{nope");
    expect(readOtherPane()).toBeNull();
    window.localStorage.setItem("ps5upload.files.otherPane", JSON.stringify({ place: { kind: "server" }, path: "/" }));
    expect(readOtherPane()).toBeNull();
  });
});
