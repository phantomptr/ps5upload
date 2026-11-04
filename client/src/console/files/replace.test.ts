import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import { t } from "../strings";
import { filesErrorText, pasteMessage } from "./Files";
import type { JobEnd } from "./move";
import { pasteItems, type PasteResult } from "./paste";
import { ReplaceError, tempName, tempStem } from "./replace";

const mockCall = vi.mocked(call);

/**
 * A console whose entries remember what they hold (`content`): the test can tell the old item
 * from the new one at the same path, and fail any request on its way.
 */
function fakeConsole(start: Record<string, string>, fail: (route: string, b: Record<string, string>) => ApiError | null = () => null) {
  const fs = { ...start };
  const log: string[] = [];
  let n = 0;
  mockCall.mockImplementation(async (route: string, body?: unknown) => {
    const b = body as Record<string, string>;
    if (route === "/api/jobs/status") throw new ApiError(404, "unknown_job");
    if (route !== "/api/fs/stat") log.push(`${route.replace("/api/fs/", "")} ${b.path ?? `${b.from ?? b.src} -> ${b.to ?? b.dest}`}`);
    const refused = fail(route, b);
    if (refused) throw refused;
    switch (route) {
      case "/api/fs/stat":
        if (fs[b.path]) return { kind: "dir" } as never;
        throw new ApiError(404, "not_found");
      case "/api/fs/delete":
        delete fs[b.path];
        return { job: `del${++n}` } as never;
      case "/api/fs/rename":
        if (b.from.startsWith("/mnt/") !== b.to.startsWith("/mnt/")) throw new ApiError(409, "cross_mount");
        if (fs[b.to]) throw new ApiError(409, "exists");
        fs[b.to] = fs[b.from];
        delete fs[b.from];
        return {} as never;
      case "/api/fs/copy":
        if (fs[b.dest]) throw new ApiError(409, "exists");
        fs[b.dest] = fs[b.src];
        return { job: `copy${++n}` } as never;
    }
    throw new Error(`unexpected ${route}`);
  });
  return { fs, log };
}

const paste = (over: { waitJob?: (id: string, unstoppable?: boolean) => Promise<JobEnd>; mode?: "copy" | "cut"; item?: string } = {}) =>
  pasteItems(
    [{ path: over.item ?? "/data/a/G", dir: true }],
    "/data/b",
    over.mode ?? "copy",
    { decide: async () => ({ choice: "replace", all: false }), volumes: [{ path: "/data" }, { path: "/mnt/usb0" }] },
    { waitJob: over.waitJob ?? (async () => "done") },
  );

const failure = (r: PasteResult) => r.failed[0]?.error as ReplaceError;
const refuse = (status: number, code: string) => new ApiError(status, code);

const OLD = "old contents";
const NEW = "new contents";
const WORLD = { "/data/a/G": NEW, "/data/b/G": OLD };

beforeEach(() => {
  mockCall.mockReset();
});

describe("Replace keeps what is there until the new one has its place", () => {
  it("the whole way: copy beside it, swap, delete the old one; nothing is left over", async () => {
    const { fs, log } = fakeConsole(WORLD);
    const r = await paste();
    expect(log).toEqual([
      "copy /data/a/G -> /data/b/G.ps5u-new",
      "rename /data/b/G -> /data/b/G.ps5u-old",
      "rename /data/b/G.ps5u-new -> /data/b/G",
      "delete /data/b/G.ps5u-old",
    ]);
    expect(fs).toEqual({ "/data/a/G": NEW, "/data/b/G": NEW });
    expect(r.done).toEqual(["/data/a/G"]);
    expect(r.failed).toEqual([]);
    expect(pasteMessage(r)?.text).toBe(t("cw_files_done"));
  });

  it("the copy job fails (no room, a read error): the old one was never touched and the partial copy goes", async () => {
    const { fs, log } = fakeConsole(WORLD);
    const r = await paste({ waitJob: async (id) => (id.startsWith("copy") ? "failed" : "done") });
    expect(fs).toEqual(WORLD);
    expect(log).toEqual(["copy /data/a/G -> /data/b/G.ps5u-new", "delete /data/b/G.ps5u-new"]);
    expect(failure(r)).toMatchObject({ what: "copy", item: "G", temp: null });
    expect(r.done).toEqual([]);
    const text = filesErrorText(failure(r));
    expect(text).toContain(t("cw_files_replace_copy_failed"));
    expect(text).toContain(t("cw_files_replace_kept", { name: "G" }));
  });

  it("the helper says the drive is full: the message says what to do, the old one stays", async () => {
    fakeConsole(WORLD);
    const base = mockCall.getMockImplementation()!;
    mockCall.mockImplementation(async (route: string, body?: unknown) => {
      if (route === "/api/jobs/status") return { state: "failed", code: 10, message: "the destination drive is full" } as never;
      return base(route, body as never);
    });
    const r = await paste({ waitJob: async (id) => (id.startsWith("copy") ? "failed" : "done") });
    expect(failure(r).why).toMatchObject({ code: "no_space" });
    expect(filesErrorText(failure(r))).toBe(t("cw_files_replace_no_space", { name: "G" }));
  });

  it("the copy is refused outright: nothing was started, the refusal is the reason", async () => {
    const { fs, log } = fakeConsole(WORLD, (route) => (route === "/api/fs/copy" ? refuse(403, "path_not_allowed") : null));
    const r = await paste();
    expect(fs).toEqual(WORLD);
    expect(log).toEqual(["copy /data/a/G -> /data/b/G.ps5u-new"]);
    expect(filesErrorText(failure(r))).toBe(`${t("cw_files_err_denied")} ${t("cw_files_replace_kept", { name: "G" })}`);
  });

  it("the copy job vanishes (cancelled on Home, the helper restarted): it does not count as whole", async () => {
    const { fs, log } = fakeConsole(WORLD);
    const r = await paste({ waitJob: async (id) => (id.startsWith("copy") ? "gone" : "done") });
    expect(fs).toEqual(WORLD);
    expect(log.some((l) => l.startsWith("rename"))).toBe(false);
    expect(failure(r)).toMatchObject({ what: "copy" });
  });

  it("the token goes during the copy: the 401 ends the batch and nothing else is touched", async () => {
    const { fs, log } = fakeConsole(WORLD);
    const r = await paste({
      waitJob: async () => {
        throw new ApiError(401, "token");
      },
    });
    expect(r.failed[0].error).toMatchObject({ status: 401 });
    expect(filesErrorText(r.failed[0].error)).toBeNull();
    expect(fs["/data/b/G"]).toBe(OLD);
    expect(log).toEqual(["copy /data/a/G -> /data/b/G.ps5u-new"]);
  });

  it("the old one cannot move aside (a game has it open): nothing changed, the new copy goes", async () => {
    const { fs } = fakeConsole(WORLD, (route, b) => (route === "/api/fs/rename" && b.from === "/data/b/G" ? refuse(500, "io") : null));
    const r = await paste();
    expect(fs).toEqual(WORLD);
    expect(failure(r)).toMatchObject({ what: "swap", temp: null });
    expect(filesErrorText(failure(r))).toBe(`${t("cw_files_replace_failed")} ${t("cw_files_replace_kept", { name: "G" })}`);
  });

  it("the new one cannot take the name: the old one gets its name back", async () => {
    const { fs, log } = fakeConsole(WORLD, (route, b) => (route === "/api/fs/rename" && b.from === "/data/b/G.ps5u-new" ? refuse(500, "io") : null));
    const r = await paste();
    expect(fs).toEqual(WORLD);
    expect(log).toContain("rename /data/b/G.ps5u-old -> /data/b/G");
    expect(failure(r)).toMatchObject({ what: "swap", temp: null });
  });

  it("the old one cannot get its name back either: both are named, neither is deleted", async () => {
    const { fs, log } = fakeConsole(WORLD, (route, b) =>
      route === "/api/fs/rename" && b.to === "/data/b/G" ? refuse(500, "io") : null,
    );
    const r = await paste();
    expect(fs).toEqual({ "/data/a/G": NEW, "/data/b/G.ps5u-old": OLD, "/data/b/G.ps5u-new": NEW });
    expect(log.some((l) => l.startsWith("delete"))).toBe(false);
    expect(failure(r)).toMatchObject({ what: "stuck", old: "/data/b/G.ps5u-old", temp: "/data/b/G.ps5u-new" });
    expect(filesErrorText(failure(r))).toBe(t("cw_files_replace_stuck", { name: "G", old: "G.ps5u-old", temp: "G.ps5u-new" }));
  });

  it("the old one cannot be deleted: the replace worked, and the page says where the old one is", async () => {
    const { fs } = fakeConsole(WORLD, (route) => (route === "/api/fs/delete" ? refuse(500, "io") : null));
    const r = await paste();
    expect(fs).toEqual({ "/data/a/G": NEW, "/data/b/G": NEW, "/data/b/G.ps5u-old": OLD });
    expect(r.failed).toEqual([]);
    expect(r.done).toEqual(["/data/a/G"]);
    const m = pasteMessage(r);
    expect(m?.tone).toBe("error");
    expect(m?.text).toContain(t("cw_files_replace_old_left", { name: "G", old: "G.ps5u-old" }));
  });

  it("a partial copy that cannot be cleared away is named", async () => {
    const { fs } = fakeConsole(WORLD, (route) => (route === "/api/fs/delete" ? refuse(500, "io") : null));
    const r = await paste({ waitJob: async (id) => (id.startsWith("copy") ? "failed" : "done") });
    expect(fs["/data/b/G"]).toBe(OLD);
    expect(failure(r)).toMatchObject({ what: "copy", temp: "/data/b/G.ps5u-new" });
    expect(filesErrorText(failure(r))).toContain(t("cw_files_replace_temp_left", { temp: "G.ps5u-new" }));
  });

  it("the clear-away is tried twice: a cancelled job's last writes may land just after it left the list", async () => {
    let deletes = 0;
    const { fs } = fakeConsole(WORLD, (route) => (route === "/api/fs/delete" && ++deletes === 1 ? refuse(500, "busy") : null));
    const r = await paste({ waitJob: async (id) => (id.startsWith("copy") ? "cancelled" : "done") });
    expect(deletes).toBe(2);
    expect(fs).toEqual(WORLD);
    expect(r.notes).toEqual([]);
  });
});

describe("Stop during a Replace", () => {
  it("the copy is cancelled and cleared away, whatever Stop says about the clean-up", async () => {
    const { fs } = fakeConsole(WORLD);
    const asked: [string, boolean | undefined][] = [];
    const r = await paste({
      waitJob: async (id, unstoppable) => {
        asked.push([id, unstoppable]);
        return id.startsWith("copy") ? "cancelled" : "done";
      },
    });
    expect(asked).toEqual([
      ["copy1", undefined],
      ["del2", true],
    ]);
    expect(fs).toEqual(WORLD);
    expect(r.halted).toEqual(["/data/a/G"]);
    expect(r.done).toEqual([]);
    expect(pasteMessage(r)).toEqual({ tone: "info", text: t("cw_files_paste_halted", { name: "G" }) });
  });

  it("the job would not stop: the temporary stays and is named, nothing else happens", async () => {
    const { fs, log } = fakeConsole(WORLD);
    const r = await paste({ waitJob: async () => "left" });
    expect(log).toEqual(["copy /data/a/G -> /data/b/G.ps5u-new"]);
    expect(fs["/data/b/G"]).toBe(OLD);
    expect(r.halted).toEqual(["/data/a/G"]);
    const m = pasteMessage(r);
    expect(m?.tone).toBe("error");
    expect(m?.text).toBe(`${t("cw_files_paste_halted", { name: "G" })} ${t("cw_files_replace_temp_left", { temp: "G.ps5u-new" })}`);
  });
});

describe("a cut that replaces", () => {
  it("on one drive: three renames and the old one's delete, no copy", async () => {
    const { fs, log } = fakeConsole(WORLD);
    const r = await paste({ mode: "cut" });
    expect(log).toEqual([
      "rename /data/a/G -> /data/b/G.ps5u-new",
      "rename /data/b/G -> /data/b/G.ps5u-old",
      "rename /data/b/G.ps5u-new -> /data/b/G",
      "delete /data/b/G.ps5u-old",
    ]);
    expect(fs).toEqual({ "/data/b/G": NEW });
    expect(r.moved).toEqual(["/data/a/G"]);
    expect(r.done).toEqual(["/data/a/G"]);
  });

  it("on one drive, when the swap fails: the item goes back where it came from, never deleted", async () => {
    const { fs, log } = fakeConsole(WORLD, (route, b) => (route === "/api/fs/rename" && b.from === "/data/b/G" ? refuse(500, "io") : null));
    const r = await paste({ mode: "cut" });
    expect(fs).toEqual(WORLD);
    expect(log.some((l) => l.startsWith("delete"))).toBe(false);
    expect(r.moved).toEqual([]);
    expect(r.done).toEqual([]);
    expect(failure(r)).toMatchObject({ what: "swap", temp: null });
  });

  it("across drives: the original goes last, after the new one has its place", async () => {
    const world = { "/mnt/usb0/G": NEW, "/data/b/G": OLD };
    const { fs, log } = fakeConsole(world);
    const r = await paste({ mode: "cut", item: "/mnt/usb0/G" });
    expect(log).toEqual([
      "copy /mnt/usb0/G -> /data/b/G.ps5u-new",
      "rename /data/b/G -> /data/b/G.ps5u-old",
      "rename /data/b/G.ps5u-new -> /data/b/G",
      "delete /data/b/G.ps5u-old",
      "delete /mnt/usb0/G",
    ]);
    expect(fs).toEqual({ "/data/b/G": NEW });
    expect(r.done).toEqual(["/mnt/usb0/G"]);
  });

  it("across drives, when the copy fails: the original and the old one both stay", async () => {
    const world = { "/mnt/usb0/G": NEW, "/data/b/G": OLD };
    const { fs } = fakeConsole(world);
    const r = await paste({ mode: "cut", item: "/mnt/usb0/G", waitJob: async (id) => (id.startsWith("copy") ? "failed" : "done") });
    expect(fs).toEqual(world);
    expect(r.done).toEqual([]);
  });

  it("across drives, when the original cannot be deleted: replaced, and the page says so", async () => {
    const world = { "/mnt/usb0/G": NEW, "/data/b/G": OLD };
    const { fs } = fakeConsole(world, (route, b) => (route === "/api/fs/delete" && b.path === "/mnt/usb0/G" ? refuse(500, "io") : null));
    const r = await paste({ mode: "cut", item: "/mnt/usb0/G" });
    expect(fs["/data/b/G"]).toBe(NEW);
    expect(r.done).toEqual(["/mnt/usb0/G"]);
    expect(pasteMessage(r)?.text).toContain(t("cw_files_replace_source_left", { name: "G" }));
  });

  it("drives unknown: the rename tries first, and a cross-mount refusal goes by copy before anything else moved", async () => {
    const world = { "/mnt/usb0/G": NEW, "/data/b/G": OLD };
    const { fs, log } = fakeConsole(world);
    await pasteItems([{ path: "/mnt/usb0/G", dir: true }], "/data/b", "cut", { decide: async () => ({ choice: "replace", all: false }) }, {
      waitJob: async () => "done",
    });
    expect(log.slice(0, 2)).toEqual(["rename /mnt/usb0/G -> /data/b/G.ps5u-new", "copy /mnt/usb0/G -> /data/b/G.ps5u-new"]);
    expect(fs).toEqual({ "/data/b/G": NEW });
  });
});

describe("a rename whose answer never came", () => {
  /** Does the rename `which` of the helper, but the page never hears back. */
  function answerLost(which: (b: Record<string, string>) => boolean) {
    const { fs, log } = fakeConsole(WORLD);
    const real = mockCall.getMockImplementation()!;
    mockCall.mockImplementation(async (route: string, body?: unknown) => {
      const r = await real(route, body as object);
      if (route === "/api/fs/rename" && which(body as Record<string, string>)) throw new ApiError(0, "timeout");
      return r as never;
    });
    return { fs, log };
  }

  it("moving the old one aside did happen: the swap goes on", async () => {
    const { fs } = answerLost((b) => b.to.endsWith(".ps5u-old"));
    const r = await paste();
    expect(fs).toEqual({ "/data/a/G": NEW, "/data/b/G": NEW });
    expect(r.failed).toEqual([]);
  });

  it("taking the name did happen: replaced, not put back", async () => {
    const { fs } = answerLost((b) => b.to === "/data/b/G");
    const r = await paste();
    expect(fs).toEqual({ "/data/a/G": NEW, "/data/b/G": NEW });
    expect(r.failed).toEqual([]);
  });

  it("a cut's rename did happen: the item is not taken for a partial copy and deleted", async () => {
    const { fs } = answerLost((b) => b.from === "/data/a/G");
    const r = await paste({ mode: "cut" });
    expect(fs).toEqual({ "/data/b/G": NEW });
    expect(r.done).toEqual(["/data/a/G"]);
  });

  it("nothing moved and the helper is gone: never reported as moved, nothing cleared", async () => {
    const { fs, log } = fakeConsole(WORLD, (route, b) =>
      route === "/api/fs/rename" && b.from === "/data/a/G" ? new ApiError(0, "offline") : null,
    );
    const r = await paste({ mode: "cut" });
    expect(fs).toEqual(WORLD);
    expect(log.filter((l) => l.startsWith("delete"))).toEqual([]);
    expect(failure(r).what).toBe("copy");
  });
});

describe("temporary names", () => {
  it("a name that is taken is never used: it may be someone's only copy", async () => {
    const leftover = { "/data/b/G.ps5u-new": "someone's", "/data/b/G.ps5u-old": "an earlier old copy" };
    const { fs, log } = fakeConsole({ ...WORLD, ...leftover });
    await paste();
    expect(log[0]).toBe("copy /data/a/G -> /data/b/G.ps5u-new2");
    expect(log[1]).toBe("rename /data/b/G -> /data/b/G.ps5u-old2");
    expect(fs).toEqual({ "/data/a/G": NEW, "/data/b/G": NEW, ...leftover });
  });

  it("no free name: nothing happens", async () => {
    await expect(tempName("/d", "x", "ps5u-new", async () => true)).rejects.toMatchObject({ code: "exists" });
    const taken: Record<string, string> = { ...WORLD };
    for (let i = 1; i <= 16; i++) taken[`/data/b/G.ps5u-new${i === 1 ? "" : i}`] = "x";
    const { fs, log } = fakeConsole(taken);
    const r = await paste();
    expect(log).toEqual([]);
    expect(fs).toEqual(taken);
    expect(failure(r)).toMatchObject({ what: "copy" });
  });

  it("a very long name is cut on a character, so the tag cannot push it past the drive's limit", () => {
    expect(tempStem("Game")).toBe("Game");
    expect(tempStem("a".repeat(250))).toBe("a".repeat(200));
    // 3 bytes each in UTF-8; 4 for an emoji, never split in two.
    expect(tempStem("ゲ".repeat(100))).toBe("ゲ".repeat(66));
    expect(tempStem(`${"a".repeat(199)}🎮b`)).toBe("a".repeat(199));
    expect(new TextEncoder().encode(`${tempStem("游戏".repeat(60))}.ps5u-new16`).length).toBeLessThanOrEqual(255);
  });
});
