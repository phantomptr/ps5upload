import { describe, expect, it } from "vitest";

import {
  finishSwap,
  journalPathFor,
  parkedPathFor,
  readJournals,
  rollbackSwap,
  runSwap,
  type SwapDeps,
  type SwapJournal,
} from "./dumpSwap";

/** A console in memory: files by path, what is registered, and every call made. */
function fakeConsole(opts: {
  files?: Record<string, string>;
  registered?: boolean;
  running?: boolean;
  /** Listings of /user/app/<id> that still show SMP's links before it lets go. */
  linkPolls?: number;
  install?: { ok: boolean; unverified?: boolean; message?: string };
}) {
  const files = new Map(Object.entries(opts.files ?? {}));
  const calls: string[] = [];
  let registered = opts.registered ?? true;
  let polls = opts.linkPolls ?? 1;
  let clock = 1000;
  const deps: SwapDeps = {
    exists: async (p) => files.has(p) || [...files.keys()].some((k) => k.startsWith(`${p}/`)),
    mkdir: async (p) => void calls.push(`mkdir ${p}`),
    move: async (a, b) => {
      calls.push(`move ${a} -> ${b}`);
      for (const k of [...files.keys()]) {
        if (k === a || k.startsWith(`${a}/`)) {
          files.set(b + k.slice(a.length), files.get(k)!);
          files.delete(k);
        }
      }
    },
    writeText: async (p, t) => {
      calls.push(`write ${p}`);
      files.set(p, t);
    },
    readText: async (p) => files.get(p) ?? null,
    remove: async (p) => {
      calls.push(`remove ${p}`);
      for (const k of [...files.keys()]) if (k === p || k.startsWith(`${p}/`)) files.delete(k);
    },
    list: async (dir) =>
      dir.startsWith("/user/app/")
        ? polls-- > 0
          ? ["mount.lnk", "mount_img.lnk"]
          : []
        : [...files.keys()]
            .filter((k) => k.startsWith(`${dir}/`) && !k.slice(dir.length + 1).includes("/"))
            .map((k) => k.slice(dir.length + 1)),
    isRunning: async () => opts.running ?? false,
    isRegistered: async () => registered,
    unregister: async (id) => {
      calls.push(`unregister ${id}`);
      registered = false;
    },
    install: async (p) => {
      calls.push(`install ${p}`);
      return opts.install ?? { ok: true };
    },
    sleep: async (ms) => void (clock += ms),
    now: () => clock,
  };
  return { deps, files, calls };
}

const input = {
  titleId: "PPSA30528",
  dump: "/data/homebrew/PPSA30528.exfat",
  packagePath: "/out/UP0000-PPSA30528_00-X.pkg",
};

describe("where a dump is parked", () => {
  it("stays on the dump's own volume, outside every scan root", () => {
    expect(parkedPathFor("/data/homebrew/G.exfat")).toBe("/data/ps5upload/parked/G.exfat");
    expect(parkedPathFor("/mnt/ext0/games/G")).toBe("/mnt/ext0/ps5upload/parked/G");
    expect(parkedPathFor("/mnt/usb1/x.ffpkg")).toBe("/mnt/usb1/ps5upload/parked/x.ffpkg");
    expect(parkedPathFor("/user/app/PPSA1")).toBeNull();
    expect(parkedPathFor("/data")).toBeNull();
  });
});

describe("replacing a dump with its package", () => {
  it("journals, parks, waits for SMP, unregisters, installs and ends installed", async () => {
    const c = fakeConsole({ files: { [input.dump]: "img" }, linkPolls: 2 });
    const steps: string[] = [];
    const r = await runSwap(input, c.deps, (s) => steps.push(s));
    expect(r).toMatchObject({ ok: true });
    expect(steps).toEqual(["park", "release", "install"]);
    const parked = "/data/ps5upload/parked/PPSA30528.exfat";
    // The journal is written before anything moves.
    const firstWrite = c.calls.findIndex((x) => x.startsWith("write "));
    const move = c.calls.indexOf(`move ${input.dump} -> ${parked}`);
    expect(firstWrite).toBeGreaterThanOrEqual(0);
    expect(firstWrite).toBeLessThan(move);
    expect(c.calls).toContain("unregister PPSA30528");
    expect(c.calls.indexOf("unregister PPSA30528")).toBeLessThan(c.calls.indexOf(`install ${input.packagePath}`));
    const j = JSON.parse(c.files.get(journalPathFor("PPSA30528"))!) as SwapJournal;
    expect(j).toMatchObject({ step: "installed", dump: input.dump, parked });
  });

  it("refuses while the game runs, touching nothing", async () => {
    const c = fakeConsole({ files: { [input.dump]: "img" }, running: true });
    const r = await runSwap(input, c.deps, () => {});
    expect(r.ok).toBe(false);
    expect(r.message).toMatch(/close the game/i);
    expect(c.calls).toEqual([]);
  });

  it("refuses when something already sits where the dump would be parked", async () => {
    const c = fakeConsole({
      files: { [input.dump]: "img", "/data/ps5upload/parked/PPSA30528.exfat": "old" },
    });
    const r = await runSwap(input, c.deps, () => {});
    expect(r.ok).toBe(false);
    expect(c.calls.some((x) => x.startsWith("move"))).toBe(false);
  });

  it("keeps the dump parked and the swap listed when the PS5 cannot confirm the install", async () => {
    const c = fakeConsole({
      files: { [input.dump]: "img" },
      install: { ok: true, unverified: true, message: "verify on PS5" },
    });
    const r = await runSwap(input, c.deps, () => {});
    expect(r).toMatchObject({ ok: true, unconfirmed: true, message: "verify on PS5" });
    const parked = "/data/ps5upload/parked/PPSA30528.exfat";
    // Not moved back (an install may still be going), not called installed (the dump is the
    // only copy known good): parked, with the swap still journaled for the person to decide.
    expect(c.files.has(parked)).toBe(true);
    expect(c.files.has(input.dump)).toBe(false);
    const j = JSON.parse(c.files.get(journalPathFor("PPSA30528"))!) as SwapJournal;
    expect(j.step).toBe("unconfirmed");
  });

  it("moves the dump back when the install fails, and clears the journal", async () => {
    const c = fakeConsole({
      files: { [input.dump]: "img" },
      install: { ok: false, message: "0x80B2116F" },
    });
    const r = await runSwap(input, c.deps, () => {});
    expect(r).toMatchObject({ ok: false, rolledBack: true });
    expect(r.message).toContain("0x80B2116F");
    expect(c.files.has(input.dump)).toBe(true);
    expect(c.files.has(journalPathFor("PPSA30528"))).toBe(false);
  });

  it("moves the dump back when SMP never lets go", async () => {
    const c = fakeConsole({ files: { [input.dump]: "img" }, linkPolls: 1_000 });
    const r = await runSwap(input, c.deps, () => {});
    expect(r).toMatchObject({ ok: false, rolledBack: true });
    expect(c.calls.some((x) => x.startsWith("install"))).toBe(false);
    expect(c.files.has(input.dump)).toBe(true);
  });
});

describe("after the swap", () => {
  const done = async () => {
    const c = fakeConsole({
      files: { [input.dump]: "img", "/data/shadowmount/manual.lst": `/x/other.exfat\n${input.dump}\n` },
    });
    await runSwap(input, c.deps, () => {});
    const j = JSON.parse(c.files.get(journalPathFor("PPSA30528"))!) as SwapJournal;
    return { c, j };
  };

  it("deletes the parked dump and its manual.lst line when asked", async () => {
    const { c, j } = await done();
    await finishSwap(j, "delete", c.deps);
    expect(c.files.has(j.parked)).toBe(false);
    expect(c.files.get("/data/shadowmount/manual.lst")).toBe("/x/other.exfat\n");
    expect(c.files.has(journalPathFor("PPSA30528"))).toBe(false);
  });

  it("keeps the dump parked outside the scan roots", async () => {
    const { c, j } = await done();
    await finishSwap(j, "keep", c.deps);
    expect(c.files.has(j.parked)).toBe(true);
    expect(c.files.get("/data/shadowmount/manual.lst")).toBe("/x/other.exfat\n");
    expect(c.files.has(journalPathFor("PPSA30528"))).toBe(false);
  });

  it("finds an interrupted swap from its journal and rolls it back", async () => {
    const c = fakeConsole({ files: { [input.dump]: "img" }, install: { ok: false } });
    // Interrupted mid-install: the journal is there and the dump is parked.
    c.deps.install = async () => new Promise(() => {});
    void runSwap(input, c.deps, () => {});
    await new Promise((r) => setTimeout(r, 0));
    const found = await readJournals(c.deps);
    expect(found).toHaveLength(1);
    expect(found[0]).toMatchObject({ titleId: "PPSA30528", step: "installing" });
    await rollbackSwap(found[0], c.deps);
    expect(c.files.has(input.dump)).toBe(true);
    expect(await readJournals(c.deps)).toEqual([]);
  });
});

describe("when the console cannot answer", () => {
  const journal: SwapJournal = {
    v: 1,
    titleId: "PPSA30528",
    dump: input.dump,
    packagePath: input.packagePath,
    parked: parkedPathFor(input.dump)!,
    step: "parked",
    at: 1,
  };

  it("rollback fails rather than saying the dump is back", async () => {
    const { deps, files } = fakeConsole({ files: { [journal.parked]: "x" } });
    deps.exists = async () => {
      throw new Error("busy");
    };
    await expect(rollbackSwap(journal, deps)).rejects.toThrow("busy");
    expect(files.has(journal.parked)).toBe(true);
  });

  it("finish keeps the journal when a step could not run, so the swap can be finished later", async () => {
    const path = journalPathFor(journal.titleId);
    const { deps, files } = fakeConsole({ files: { [journal.parked]: "x", [path]: "{}" } });
    deps.remove = async () => {
      throw new Error("busy");
    };
    await expect(finishSwap(journal, "delete", deps)).rejects.toThrow("busy");
    expect(files.has(path)).toBe(true);

    const again = fakeConsole({ files: { [path]: "{}" } });
    again.deps.readText = async () => {
      throw new Error("busy");
    };
    await expect(finishSwap(journal, "keep", again.deps)).rejects.toThrow("busy");
    expect(again.files.has(path)).toBe(true);
  });

  it("a failed listing is not ShadowMount+ letting go: the wait goes on", async () => {
    const { deps, calls } = fakeConsole({ files: { [input.dump]: "x" }, linkPolls: 0 });
    const list = deps.list;
    let n = 0;
    deps.list = async (dir) => {
      if (dir.startsWith("/user/app/") && n++ < 2) throw new Error("busy");
      return list(dir);
    };
    const steps: string[] = [];
    const r = await runSwap(input, deps, (s) => steps.push(s));
    expect(r.ok).toBe(true);
    expect(n).toBe(3);
    expect(calls.findIndex((c) => c.startsWith("unregister"))).toBeGreaterThan(-1);
  });

  it("gives up at the deadline and undoes when the listing never comes back", async () => {
    const { deps, files, calls } = fakeConsole({ files: { [input.dump]: "x" } });
    const list = deps.list;
    deps.list = async (dir) => {
      if (dir.startsWith("/user/app/")) throw new Error("busy");
      return list(dir);
    };
    const r = await runSwap(input, deps, () => {});
    expect(r).toMatchObject({ ok: false, rolledBack: true });
    expect(files.has(input.dump)).toBe(true);
    expect(calls.some((c) => c.startsWith("unregister"))).toBe(false);
  });
});
