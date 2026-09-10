import { describe, expect, it, vi } from "vitest";
import {
  applyBackport,
  diagnoseLaunchFailure,
  nextSetsAfter,
  verdictFrom,
  resolveSets,
  backportOverlayReady,
  existingLibraries,
  isBackportEligible,
  planBackport,
  rankSets,
  setAttestation,
  requiresBackport,
  undoBackport,
  type BackportTransport,
  type FakelibSet,
} from "./backport";

const title = (titleId: string, source = `/games/${titleId}`) => ({
  titleId,
  titleName: titleId,
  origin: "registered" as const,
  imageBacked: false,
  source,
  system: false,
});
const lib = (name: string, size: number) => ({ name, size });
const set = (
  id: string,
  titleId: string,
  names: string[],
  shippedBy = 1,
): FakelibSet => ({
  id,
  label: id,
  origin: { kind: "scan", title_id: titleId },
  libraries: names.map((n, i) => ({
    ...lib(n, 100 + i),
    path: `builds/${n.replace(/\.(sprx|prx)$/i, "")}/${(i + 10).toString(16)}abcdef.sprx`,
    shippedBy,
  })),
});

const FW11 = "0x1100000000000000";

describe("deciding whether a title needs a backport", () => {
  it("keys on the SDK a title was built with, not its declared required firmware", () => {
    // SILENT HILL 2 declares requiredSystemSoftwareVersion 10.20 but was built
    // with SDK 9.00, and runs on a 9.60 console with no fakelib at all. Across
    // 31 image-backed titles, sdkVersion predicted this without exception and
    // requiredSystemSoftwareVersion did not.
    expect(requiresBackport("0x0900000000000000", "FreeBSD releases/09.60: build")).toBe(false);
    expect(requiresBackport(FW11, "FreeBSD releases/09.60: build")).toBe(true);
    expect(requiresBackport("0x0960000000000000", "FreeBSD releases/09.60: build")).toBe(false);
    expect(requiresBackport("", "FreeBSD releases/09.60: build")).toBe(false);
  });

  it("requires an overlay state reported ready by the current payload", () => {
    expect(backportOverlayReady(undefined)).toBe(false);
    expect(backportOverlayReady({ state: "idle" })).toBe(false);
    // `blocked` means an external BackPork holds the mount; the kernel refuses
    // a second unionfs over one target and the game starts with no libraries.
    expect(backportOverlayReady({ state: "blocked" })).toBe(false);
    expect(backportOverlayReady({ state: "watching" })).toBe(true);
    expect(backportOverlayReady({ state: "mounted" })).toBe(true);
  });

  it("offers image-backed titles based only on their build SDK", () => {
    const image = { ...title("PPSA30528", "/mnt/shadowmnt/PPSA30528_a3a83051"), imageBacked: true };
    expect(isBackportEligible(image, {
      title_id: image.titleId,
      name: image.titleName,
      sdk_version: FW11,
      fw_required: "0x0900000000000000",
      patchable: true,
    }, "FreeBSD releases/09.60: build")).toBe(true);
    expect(isBackportEligible(image, {
      title_id: image.titleId,
      name: image.titleName,
      sdk_version: "0x0900000000000000",
      fw_required: "0x1200000000000000",
      patchable: true,
    }, "FreeBSD releases/09.60: build")).toBe(false);
  });

  it("ignores macOS AppleDouble sidecars, which are not libraries", () => {
    // They end in .sprx, are always ~4 KB, and an early build offered eight of
    // them for installation — visible as "._libSceAgc.sprx (4,096 bytes)".
    expect(
      existingLibraries([
        { name: "libSceAgc.sprx", size: 30, kind: "file" },
        { name: "._libSceAgc.sprx", size: 4096, kind: "file" },
        { name: ".DS_Store", size: 6148, kind: "file" },
        { name: "debug", size: 0, kind: "dir" },
      ]).map((l) => l.name),
    ).toEqual(["libSceAgc.sprx"]);
  });
});

describe("ranking candidate sets", () => {
  it("prefers the target's own proven set when the corpus contains it", () => {
    // Identified through the scan origin: a set knows which title it was
    // harvested from, and that is the one combination known to work for it.
    const sets = [
      set("other", "PPSA26344", ["a.sprx"]),
      set("own", "PPSA30528", ["a.sprx", "b.sprx", "c.sprx"]),
    ];
    expect(rankSets(sets, [], "PPSA30528").map((p) => p.id)).toEqual(["own", "other"]);
  });

  it("ranks by attestation, not by the target's SDK", () => {
    // Measured across 34 titles: one libSceAgc build ships in titles declaring
    // SDK 0500, 0900, 1000 and 1100, and a single SDK (1100) uses four
    // different builds. So SDK does not predict the build, and this used to
    // sort on it. Popularity is the only signal with evidence behind it.
    const sets = [
      set("RARE", "PPSA00001", ["a.sprx"], 1),
      set("COMMON", "PPSA00002", ["a.sprx", "b.sprx"], 13),
    ];
    expect(rankSets(sets).map((p) => p.id)).toEqual(["COMMON", "RARE"]);
  });

  it("rates a set by its RAREST build, not its average", () => {
    // A set is only as well-travelled as its least common member: one obscure
    // library is enough to make the whole combination untested.
    const mixed = set("MIXED", "PPSA00001", ["a.sprx", "b.sprx"], 13);
    mixed.libraries[1] = { ...mixed.libraries[1], shippedBy: 1 };
    const steady = set("STEADY", "PPSA00002", ["a.sprx", "b.sprx"], 4);
    expect(setAttestation(mixed)).toBe(1);
    expect(setAttestation(steady)).toBe(4);
    expect(rankSets([mixed, steady]).map((p) => p.id)).toEqual(["STEADY", "MIXED"]);
  });

  it("drops sets already tried for this title, and empty ones", () => {
    const sets = [
      set("TRIED", "PPSA00001", ["a.sprx"]),
      set("EMPTY", "PPSA00002", []),
      set("NEXT", "PPSA00003", ["a.sprx", "b.sprx"]),
    ];
    expect(rankSets(sets, ["TRIED"]).map((p) => p.id)).toEqual(["NEXT"]);
  });
});

describe("planning an install", () => {
  it("installs the set WHOLE and stashes every original library", () => {
    // A set is the only unit proven to work: it is what one real game
    // ships. A mixture of a set and whatever the target already had is a
    // combination nobody has tested.
    const target = title("PPSA26344");
    const p = set("PPSA30528", "PPSA00000", ["libSceAgc.sprx", "libScePsml.sprx"]);
    const plan = planBackport(
      target,
      p,
      [lib("libSceAgc.sprx", 999), lib("libSceAmpr.sprx", 5)],
      "/host/fakelibs/sets",
    );
    expect(plan.copies.map((c) => c.name)).toEqual(["libSceAgc.sprx", "libScePsml.sprx"]);
    // Copies come from the content-addressed build store, not a per-game
    // directory: one build shipped by 13 games is stored once.
    expect(plan.copies[0].from).toBe("/host/fakelibs/sets/builds/libSceAgc/aabcdef.sprx");
    expect(plan.copies[0].to).toBe("/games/PPSA26344/fakelib/libSceAgc.sprx");
    // Same name does not mean same build. The live RDR trial found sets
    // with the same three names but different bytes, so exact Undo requires
    // stashing both originals before the set is installed.
    expect(plan.replaced.map((l) => l.name)).toEqual([
      "libSceAgc.sprx",
      "libSceAmpr.sprx",
    ]);
    expect(plan.stashDir).toBe("/data/ps5upload/backport/PPSA26344");
  });

  it("refuses a set with no libraries", () => {
    expect(() =>
      planBackport(title("T"), set("P", "PPSA00000", []), [], "/host"),
    ).toThrow(/no libraries/i);
  });

  it("refuses unsafe set paths before constructing transfer destinations", () => {
    const unsafe = set("PPSA30528", "PPSA00000", ["../libSceAgc.sprx"]);
    expect(() => planBackport(title("PPSA26344"), unsafe, [], "/host"))
      .toThrow(/unsafe library name/i);
  });
});

describe("apply and undo", () => {
  it("stashes originals, uploads the complete set, then patches", async () => {
    const calls: string[] = [];
    const transport: BackportTransport = {
      copyConsole: vi.fn(async (from: string) => { calls.push(`stash:${from}`); }),
      uploadHost: vi.fn(async (_from: string, to: string) => { calls.push(`upload:${to}`); }),
      mkdirConsole: vi.fn(async (path: string) => { calls.push(`mkdir:${path}`); }),
      patch: vi.fn(async () => { calls.push("patch"); }),
      remove: vi.fn(async (p: string) => { calls.push(`remove:${p}`); }),
      restore: vi.fn(),
    };
    const plan = planBackport(
      title("T"),
      set("P", "PPSA00000", ["new.sprx"]),
      [lib("old.sprx", 7)],
      "/host",
    );
    const record = await applyBackport(plan, transport, false);
    expect(calls).toEqual([
      "mkdir:/data/ps5upload/backport/T",
      "stash:/games/T/fakelib/old.sprx",
      "remove:/games/T/fakelib/old.sprx",
      "mkdir:/games/T/fakelib",
      "upload:/games/T/fakelib/new.sprx",
      "patch",
    ]);
    expect(record.complete).toBe(true);
    expect(record.replaced.map((l) => l.name)).toEqual(["old.sprx"]);
  });

  it("undo removes what we wrote and restores what we displaced", async () => {
    // Removing our libraries without putting the originals back leaves the
    // title with neither, which looks like a failed backport rather than a
    // failed undo.
    const copies: string[] = [];
    const transport: BackportTransport = {
      copyConsole: vi.fn(async (from: string, to: string) => { copies.push(`${from} -> ${to}`); }),
      uploadHost: vi.fn(),
      mkdirConsole: vi.fn(),
      patch: vi.fn(),
      remove: vi.fn(),
      restore: vi.fn(),
    };
    await undoBackport(
      {
        titleId: "T",
        setId: "P",
        targetSource: "/games/T",
        copiedPaths: ["/games/T/fakelib/new.sprx"],
        replaced: [lib("old.sprx", 7)],
        stashDir: "/data/ps5upload/backport/T",
        complete: true,
      },
      transport,
    );
    expect(transport.remove).toHaveBeenCalledWith("/games/T/fakelib/new.sprx");
    expect(copies).toEqual([
      "/data/ps5upload/backport/T/old.sprx -> /games/T/fakelib/old.sprx",
    ]);
    expect(transport.restore).toHaveBeenCalledWith("T");
    expect(transport.remove).toHaveBeenCalledWith("/data/ps5upload/backport/T");
  });

  it("records partial copies so a failed apply can still be undone", async () => {
    const transport: BackportTransport = {
      copyConsole: vi.fn(),
      uploadHost: vi.fn().mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error("disk full")),
      mkdirConsole: vi.fn(),
      patch: vi.fn(), remove: vi.fn(), restore: vi.fn(),
    };
    const plan = planBackport(
      title("T"),
      set("P", "PPSA00000", ["a.sprx", "b.sprx"]),
      [],
      "/host",
    );
    await expect(applyBackport(plan, transport, false)).rejects.toMatchObject({
      copiedPaths: ["/games/T/fakelib/a.sprx"],
    });
    expect(transport.patch).not.toHaveBeenCalled();
  });

  it("reports only originals that were safely stashed and removed", async () => {
    const transport: BackportTransport = {
      copyConsole: vi.fn().mockResolvedValueOnce(undefined).mockResolvedValueOnce(undefined),
      uploadHost: vi.fn(),
      mkdirConsole: vi.fn(),
      patch: vi.fn(),
      remove: vi.fn().mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error("busy")),
      restore: vi.fn(),
    };
    const plan = planBackport(
      title("T"),
      set("P", "PPSA00000", ["new.sprx"]),
      [lib("old-a.sprx", 1), lib("old-b.sprx", 2)],
      "/host",
    );
    await expect(applyBackport(plan, transport, false)).rejects.toMatchObject({
      replaced: [lib("old-a.sprx", 1)],
    });
  });
});

describe("diagnosing a failed launch", () => {
  // Both failures show "no process", so the process table cannot tell them
  // apart and the fixes are opposite. These are the real klog shapes measured
  // on Red Dead Redemption.
  it("reads a missing-library failure from the unpatched-function marker", () => {
    const klog = [
      "<118>[SceLncService] launchApp(PPSA30528)",
      "<118># === Call to unpatched function is detected!!! ===",
    ].join("\n");
    expect(diagnoseLaunchFailure(klog, "PPSA30528")).toBe("missing-libraries");
  });

  it("reads a wrong-library failure from createApp with no unpatched marker", () => {
    const klog = [
      "<118>[SceLncService] launchApp(PPSA30528)",
      "<118>[Syscore App] createApp PPSA30528",
      "<118>[CheatRunner] [info] [game] game started PPSA30528",
    ].join("\n");
    expect(diagnoseLaunchFailure(klog, "PPSA30528")).toBe("wrong-libraries");
  });

  it("does not guess when the game never got as far as being created", () => {
    // A launch that failed for an unrelated reason: claiming either answer
    // would send the user down a three-minute edit cycle for nothing.
    expect(diagnoseLaunchFailure("<118>[SceLncService] launchApp(PPSA30528)", "PPSA30528")).toBe("unknown");
    expect(diagnoseLaunchFailure("", "PPSA30528")).toBe("unknown");
  });

  it("does not mistake another title's launch for this one", () => {
    const klog = "<118>[Syscore App] createApp PPSA08709";
    expect(diagnoseLaunchFailure(klog, "PPSA30528")).toBe("unknown");
  });
});

describe("resolving a content-addressed corpus", () => {
  const SHA_A = "a".repeat(64);
  const SHA_B = "b".repeat(64);
  const manifest = {
    schema: 3,
    libraries: [
      { name: "libSceAgc.sprx", builds: [
        { sha256: SHA_A, size: 10, sdk: null, shipped_by: ["T1", "T2"], path: "builds/libSceAgc/aaaaaaaa.sprx" },
      ] },
      { name: "libScePsml.sprx", builds: [
        { sha256: SHA_B, size: 20, sdk: null, shipped_by: ["T1"], path: "builds/libScePsml/bbbbbbbb.sprx" },
      ] },
    ],
    sets: [
      { id: "set-1", label: "Red Dead",
        origin: { kind: "scan", title_id: "PPSA30528", console: "PS5-Pro" },
        libraries: { "libSceAgc.sprx": SHA_A, "libScePsml.sprx": SHA_B } },
    ],
  };

  it("resolves a set into the builds it references", () => {
    const [resolved] = resolveSets(manifest);
    expect(resolved.id).toBe("set-1");
    expect(resolved.label).toBe("Red Dead");
    expect(resolved.origin).toEqual({ kind: "scan", title_id: "PPSA30528", console: "PS5-Pro", at: undefined });
    expect(resolved.libraries).toEqual([
      { name: "libSceAgc.sprx", size: 10, sha256: SHA_A, path: "builds/libSceAgc/aaaaaaaa.sprx", shippedBy: 2 },
      { name: "libScePsml.sprx", size: 20, sha256: SHA_B, path: "builds/libScePsml/bbbbbbbb.sprx", shippedBy: 1 },
    ]);
  });

  it("drops a set referencing a build the corpus does not hold", () => {
    // Installing the resolvable half would produce a combination no game ever
    // shipped, which is exactly what the corpus exists to prevent.
    const broken = { ...manifest, sets: [
      { ...manifest.sets[0], libraries: { "libSceAgc.sprx": SHA_A, "libScePsml.sprx": "c".repeat(64) } },
    ] };
    expect(resolveSets(broken)).toEqual([]);
  });

  it("refuses a build filed under a different library name", () => {
    // The manifest has to agree with itself: a libScePsml build must not be
    // installable as libSceAgc.
    const swapped = { ...manifest, sets: [
      { ...manifest.sets[0], libraries: { "libSceAgc.sprx": SHA_B } },
    ] };
    expect(resolveSets(swapped)).toEqual([]);
  });

  it("rejects build paths that escape the corpus directory", () => {
    // `path` is concatenated into a filesystem path we copy from. A manifest
    // is a file on disk and may be hand-edited or fetched from elsewhere.
    for (const bad of ["../../etc/passwd", "/etc/passwd", "builds/../../x.sprx", "builds/x/y.txt"]) {
      const evil = { ...manifest, libraries: [
        { name: "libSceAgc.sprx", builds: [{ sha256: SHA_A, size: 10, sdk: null, shipped_by: [], path: bad }] },
      ], sets: [
        { ...manifest.sets[0], libraries: { "libSceAgc.sprx": SHA_A } },
      ] };
      expect(resolveSets(evil), bad).toEqual([]);
    }
  });

  it("returns nothing for junk rather than throwing", () => {
    expect(resolveSets(null)).toEqual([]);
    expect(resolveSets({})).toEqual([]);
    expect(resolveSets({ schema: 4, libraries: "no", sets: [] })).toEqual([]);
    expect(resolveSets({ schema: 2, sets: [] })).toEqual([]);
  });
});

describe("verifying a backport", () => {
  const KLOG_MISSING = "<118>[SceLncService] launchApp(PPSA30528)\n<118># === Call to unpatched function is detected!!! ===";
  const KLOG_WRONG = "<118>[Syscore App] createApp PPSA30528";

  it("counts a title that started and then died as a failure", () => {
    // The trap: it had 39 threads at one point, so "any sample alive" would
    // call it running. It was gone by the end of the window, which is what
    // matters — and klog says why.
    const v = verdictFrom(
      [{ threads: 39 }, { threads: 39 }, { threads: null }, { threads: null }],
      KLOG_WRONG,
      "PPSA30528",
    );
    expect(v.kind).toBe("wrong-libraries");
  });

  it("tolerates a slow start, judging only the end of the window", () => {
    // A cold start from USB showed no process for the first 40 seconds and
    // went on to reach 263 threads.
    const v = verdictFrom(
      [{ threads: null }, { threads: null }, { threads: null }, { threads: 12 }, { threads: 263 }],
      "",
      "PPSA30528",
    );
    expect(v).toEqual({ kind: "running", peakThreads: 263 });
  });

  it("never calls a running process a success", () => {
    // Thread count misled three times (1 / 18 / 263 threads), and a trial that
    // installed byte-identical libraries twice got one failure and one
    // success. A live process is a question for the human, not a verdict.
    const v = verdictFrom([{ threads: null }, { threads: 40 }, { threads: 39 }], "", "PPSA30528");
    expect(v).toEqual({ kind: "running", peakThreads: 40 });
    // "running" is a question for the human, never a success claim.
  });

  it("reads a missing-library failure when nothing ever ran", () => {
    const v = verdictFrom([{ threads: null }, { threads: null }], KLOG_MISSING, "PPSA30528");
    expect(v.kind).toBe("missing-libraries");
  });

  it("reads a wrong-library failure when the game was created and died", () => {
    const v = verdictFrom([{ threads: null }], KLOG_WRONG, "PPSA30528");
    expect(v.kind).toBe("wrong-libraries");
  });

  it("does not guess when the launch never got that far", () => {
    expect(verdictFrom([{ threads: null }], "", "PPSA30528").kind).toBe("unknown");
  });

  it("offers only LARGER sets after a missing-library failure", () => {
    // Smaller sets cannot supply what was missing, so proposing one wastes a
    // three-minute edit cycle.
    const failed = set("failed", "PPSA00001", ["a.sprx", "b.sprx"]);
    const smaller = set("smaller", "PPSA00002", ["a.sprx"]);
    const bigger = set("bigger", "PPSA00003", ["a.sprx", "b.sprx", "c.sprx"]);
    const next = nextSetsAfter({ kind: "missing-libraries" }, failed, [failed, smaller, bigger]);
    expect(next.map((s) => s.id)).toEqual(["bigger"]);
  });

  it("offers any other set after a wrong-library failure", () => {
    // Wrong lineage, not too few: a smaller set is a perfectly good next try.
    const failed = set("failed", "PPSA00001", ["a.sprx", "b.sprx"]);
    const smaller = set("smaller", "PPSA00002", ["a.sprx"]);
    const next = nextSetsAfter({ kind: "wrong-libraries" }, failed, [failed, smaller]);
    expect(next.map((s) => s.id)).toEqual(["smaller"]);
  });
});
