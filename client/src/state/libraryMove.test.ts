import { beforeEach, describe, expect, it } from "vitest";

const {
  useLibraryMoveStore,
  libraryMove,
  libraryMoveKey,
  runLibraryMove,
  stopLibraryMove,
  dismissLibraryMove,
  moveRetryDest,
} = await import("./libraryMove");
type Deps = import("./libraryMove").LibraryMoveDeps;
type End = import("./libraryMove").LibraryMoveState;
type LibraryMoveState = End;

const HOST = "192.168.0.5";
const SRC = "/data/homebrew/GAME";
const DEST = "/mnt/ext0/homebrew/GAME";
const KEY = libraryMoveKey(HOST, SRC);

function console_() {
  const calls: string[] = [];
  let finishCopy!: (err?: Error) => void;
  const copying = new Promise<void>((res, rej) => {
    finishCopy = (err) => (err ? rej(err) : res());
  });
  const deps: Deps = {
    copy: async (_addr, from, to) => {
      calls.push(`copy ${from} -> ${to}`);
      await copying;
    },
    opStatus: async () => ({ bytes_copied: 40, total_bytes: 100 }),
    opCancel: async () => {
      calls.push("cancel");
    },
    deleteSource: async () => {
      calls.push("delete");
      return { ok: true };
    },
    sleep: () => new Promise((r) => setTimeout(r, 0)),
    newOpId: () => 7,
  };
  return { deps, calls, finishCopy };
}

const run = (c: ReturnType<typeof console_>, hooks = {}) =>
  runLibraryMove(
    { host: HOST, addr: HOST, from: SRC, to: DEST, payloadVersion: "6.2.3" },
    c.deps,
    hooks,
  );
const state = () => libraryMove(useLibraryMoveStore.getState(), KEY);
const settle = async (until: () => boolean) => {
  for (let i = 0; i < 200 && !until(); i++)
    await new Promise((r) => setTimeout(r, 1));
};

describe("runLibraryMove", () => {
  beforeEach(() => useLibraryMoveStore.setState({ byKey: {} }));

  it("shows the copy's bytes in the store while it runs, with no row mounted", async () => {
    const c = console_();
    const p = run(c);
    await settle(() => (state()?.bytesCopied ?? 0) > 0);
    expect(state()).toMatchObject({
      phase: "copying",
      bytesCopied: 40,
      totalBytes: 100,
      to: DEST,
    });
    c.finishCopy();
    await p;
  });

  it("copies, then removes the source, and ends done", async () => {
    const c = console_();
    const p = run(c);
    c.finishCopy();
    await p;
    expect(c.calls).toEqual([`copy ${SRC} -> ${DEST}`, "delete"]);
    expect(state()?.phase).toBe("done");
  });

  it("a failed copy leaves the source alone and keeps the error", async () => {
    const c = console_();
    const p = run(c);
    c.finishCopy(new Error("fs_copy_dest_full"));
    await p;
    expect(c.calls).not.toContain("delete");
    expect(state()).toMatchObject({
      phase: "copy-failed",
      error: "fs_copy_dest_full",
    });
  });

  it("stop cancels the console's copy and the run ends cancelled", async () => {
    const c = console_();
    const p = run(c);
    await settle(() => state()?.phase === "copying");
    stopLibraryMove(KEY);
    await settle(() => c.calls.includes("cancel"));
    c.finishCopy(new Error("fs_copy_cancelled"));
    await p;
    expect(state()?.phase).toBe("cancelled");
    expect(c.calls).not.toContain("delete");
  });

  it("says so when the copy landed but the source could not be removed", async () => {
    const c = console_();
    c.deps.deleteSource = async () => ({
      ok: false,
      lastError: new Error("busy"),
    });
    const p = run(c);
    c.finishCopy();
    await p;
    expect(state()).toMatchObject({ phase: "delete-failed", error: "busy" });
  });

  it("tells its caller the progress and the end, whichever screen is open", async () => {
    const c = console_();
    const seen: string[] = [];
    const p = run(c, {
      onProgress: (b: number, t: number) => {
        if (seen.length === 0) seen.push(`${b}/${t}`);
      },
      onEnd: (s: End) => seen.push(`end:${s.phase}`),
    });
    await settle(() => seen.length > 0);
    c.finishCopy();
    await p;
    expect(seen).toEqual(["40/100", "end:done"]);
  });

  it("does not start a second move of the same entry while one runs", async () => {
    const c = console_();
    const first = run(c);
    await settle(() => state()?.phase === "copying");
    await run(c);
    c.finishCopy();
    await first;
    expect(c.calls.filter((x) => x.startsWith("copy"))).toHaveLength(1);
  });

  it("dismiss forgets a finished move and leaves a running one alone", async () => {
    const c = console_();
    const p = run(c);
    await settle(() => state()?.phase === "copying");
    dismissLibraryMove(KEY);
    expect(state()).not.toBeNull();
    c.finishCopy();
    await p;
    dismissLibraryMove(KEY);
    expect(state()).toBeNull();
  });
});

describe("moveRetryDest", () => {
  const ended = (phase: LibraryMoveState["phase"]): LibraryMoveState => ({
    phase,
    from: "/data/homebrew/Game",
    to: "/mnt/ext0/homebrew/Game",
    bytesCopied: 10,
    totalBytes: 100,
    error: null,
    progressUnsupported: null,
    stopRequested: false,
    startedAtMs: 0,
  });

  it("offers the same destination again after a stop or a failed copy", () => {
    expect(moveRetryDest(ended("cancelled"))).toBe("/mnt/ext0/homebrew/Game");
    expect(moveRetryDest(ended("copy-failed"))).toBe("/mnt/ext0/homebrew/Game");
  });

  it("offers nothing once the copy has landed, or while it runs", () => {
    // After a landed copy the destination is the user's game: a second copy would be refused.
    expect(moveRetryDest(ended("done"))).toBeNull();
    expect(moveRetryDest(ended("delete-failed"))).toBeNull();
    expect(moveRetryDest(ended("copying"))).toBeNull();
    expect(moveRetryDest(ended("deleting"))).toBeNull();
    expect(moveRetryDest(null)).toBeNull();
  });
});
