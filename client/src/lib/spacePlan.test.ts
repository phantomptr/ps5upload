import { describe, expect, it } from "vitest";

import {
  ageBand,
  buildCandidates,
  driveToken,
  gibText,
  parseSizeGib,
  parseSpacePlanQuery,
  planSpace,
  planWithSelection,
  resolveDrive,
  runOrder,
  sameDrive,
  spacePlanLink,
  volumeOfPath,
  type CandidateInput,
  type SpaceCandidate,
  type SpacePlanInput,
} from "./spacePlan";

const GB = 1024 ** 3;
const DAY = 24 * 60 * 60 * 1000;
const NOW = 1_800_000_000_000;

const leftover = (id: number, gb: number): SpaceCandidate => ({
  id: `leftover:${id}`,
  kind: "leftover",
  label: `L${id}`,
  bytes: gb * GB,
  lastPlayedMs: null,
  cleanupId: id,
});
const move = (name: string, gb: number, daysAgo: number | null, targets = ["/mnt/usb0"]): SpaceCandidate => ({
  id: `move:${name}`,
  kind: "move",
  label: name,
  bytes: gb * GB,
  lastPlayedMs: daysAgo === null ? null : NOW - daysAgo * DAY,
  moveTargets: targets,
  path: `/data/homebrew/${name}`,
});
const uninstall = (name: string, gb: number, daysAgo: number | null, hasCopy = true): SpaceCandidate => ({
  id: `uninstall:${name}`,
  kind: "uninstall",
  label: name,
  titleId: name,
  bytes: gb * GB,
  lastPlayedMs: daysAgo === null ? null : NOW - daysAgo * DAY,
  hasCopy,
});

const input = (over: Partial<SpacePlanInput>): SpacePlanInput => ({
  neededBytes: 100 * GB,
  roomBytes: 0,
  candidates: [],
  destRoom: { "/mnt/usb0": 1000 * GB },
  nowMs: NOW,
  ...over,
});
const picked = (p: ReturnType<typeof planSpace>) => runOrder(p).map((s) => s.candidate.id);

describe("planSpace", () => {
  it("asks for nothing when the game already fits", () => {
    const p = planSpace(input({ neededBytes: 50 * GB, roomBytes: 60 * GB, candidates: [move("A", 80, 100)] }));
    expect(p.shortBytes).toBe(0);
    expect(p.enough).toBe(true);
    expect(picked(p)).toEqual([]);
    // The candidate is still listed, so it can be ticked by hand.
    expect(p.steps).toHaveLength(1);
  });

  it("only plans for what is missing beyond the room already there", () => {
    const p = planSpace(input({ neededBytes: 100 * GB, roomBytes: 70 * GB, candidates: [leftover(1, 40)] }));
    expect(p.shortBytes).toBe(30 * GB);
    expect(p.enough).toBe(true);
    expect(picked(p)).toEqual(["leftover:1"]);
  });

  it("uses leftovers first, then moves, then uninstalls", () => {
    const p = planSpace(
      input({
        neededBytes: 100 * GB,
        candidates: [uninstall("U", 90, 400), move("M", 50, 400), leftover(1, 20)],
      }),
    );
    expect(picked(p)).toEqual(["leftover:1", "move:M", "uninstall:U"]);
    expect(p.enough).toBe(true);
    expect(p.actions).toBe(3);
  });

  it("never needs an uninstall when moves cover the rest", () => {
    const p = planSpace(input({ candidates: [uninstall("U", 200, 400), move("M", 120, 1)] }));
    expect(picked(p)).toEqual(["move:M"]);
  });

  it("takes only as many leftovers as it needs, largest first", () => {
    const p = planSpace(input({ neededBytes: 10 * GB, candidates: [leftover(1, 3), leftover(2, 12), leftover(3, 8)] }));
    expect(picked(p)).toEqual(["leftover:2"]);
  });

  it("counts every leftover as one action (one clean-up run)", () => {
    const p = planSpace(input({ neededBytes: 10 * GB, candidates: [leftover(1, 4), leftover(2, 4), leftover(3, 4)] }));
    expect(picked(p)).toHaveLength(3);
    expect(p.actions).toBe(1);
  });

  it("prefers one game that covers it alone over several", () => {
    const p = planSpace(
      input({ neededBytes: 100 * GB, candidates: [move("small1", 60, 500), move("small2", 60, 500), move("big", 110, 40)] }),
    );
    expect(picked(p)).toEqual(["move:big"]);
  });

  it("among games that each cover it alone, picks the least recently played", () => {
    const p = planSpace(input({ candidates: [move("recent", 150, 1), move("old", 120, 200)] }));
    expect(picked(p)).toEqual(["move:old"]);
  });

  it("prefers a game known to be old over one never seen playing, but not over a recent one", () => {
    const old = planSpace(input({ candidates: [move("seen", 150, 400), move("unknown", 150, null)] }));
    expect(picked(old)).toEqual(["move:seen"]);
    const recent = planSpace(input({ candidates: [move("recent", 150, 1), move("unknown", 150, null)] }));
    expect(picked(recent)).toEqual(["move:unknown"]);
  });

  it("combines games least recently played first and drops any not needed", () => {
    const p = planSpace(
      input({
        neededBytes: 100 * GB,
        candidates: [move("A", 70, 300), move("B", 20, 200), move("C", 40, 3), move("D", 50, 100)],
      }),
    );
    // A (oldest) + B, still short; D covers the rest; then B is put back since A + D suffice.
    expect(picked(p).sort()).toEqual(["move:A", "move:D"]);
    expect(p.enough).toBe(true);
  });

  it("within the same age band takes the larger game", () => {
    const p = planSpace(input({ neededBytes: 100 * GB, candidates: [move("A", 30, 120), move("B", 80, 100), move("C", 40, 95)] }));
    expect(picked(p).sort()).toEqual(["move:B", "move:C"]);
  });

  it("only moves what another drive has room for, sharing that room between moves", () => {
    const p = planSpace(
      input({
        neededBytes: 100 * GB,
        destRoom: { "/mnt/usb0": 90 * GB },
        candidates: [move("A", 60, 300), move("B", 50, 300), uninstall("U", 60, 300)],
      }),
    );
    // A fits; B does not fit beside it, so the rest comes from the uninstall.
    expect(picked(p)).toEqual(["move:A", "uninstall:U"]);
    const a = p.steps.find((s) => s.candidate.id === "move:A");
    expect(a?.dest).toBe("/mnt/usb0");
  });

  it("sends each move to the drive with the most room left", () => {
    const p = planWithSelection(
      input({
        destRoom: { "/mnt/usb0": 100 * GB, "/mnt/ext1": 150 * GB },
        candidates: [move("A", 80, 300, ["/mnt/usb0", "/mnt/ext1"]), move("B", 60, 300, ["/mnt/usb0", "/mnt/ext1"])],
      }),
      new Set(["move:A", "move:B"]),
    );
    const dest = Object.fromEntries(p.steps.map((s) => [s.candidate.id, s.dest]));
    expect(dest["move:A"]).toBe("/mnt/ext1");
    expect(dest["move:B"]).toBe("/mnt/usb0");
  });

  it("never offers to uninstall a game with no copy, nor a move with nowhere to go", () => {
    const p = planSpace(input({ candidates: [uninstall("nocopy", 200, 900, false), move("stuck", 200, 900, [])] }));
    expect(p.steps).toHaveLength(0);
    expect(p.enough).toBe(false);
    expect(p.missingBytes).toBe(100 * GB);
  });

  it("drops candidates of unknown size", () => {
    const p = planSpace(input({ candidates: [move("A", 0, 100)] }));
    expect(p.steps).toHaveLength(0);
  });

  it("takes everything it can and says how much is still missing", () => {
    const p = planSpace(input({ neededBytes: 100 * GB, candidates: [leftover(1, 10), move("A", 30, 5), uninstall("U", 20, 5)] }));
    expect(picked(p)).toEqual(["leftover:1", "move:A", "uninstall:U"]);
    expect(p.enough).toBe(false);
    expect(p.missingBytes).toBe(40 * GB);
  });
});

describe("planWithSelection", () => {
  it("re-totals a hand-picked selection", () => {
    const base = input({ candidates: [move("A", 60, 300), uninstall("U", 70, 300)] });
    const p = planWithSelection(base, new Set(["uninstall:U"]));
    expect(p.freedBytes).toBe(70 * GB);
    expect(p.missingBytes).toBe(30 * GB);
    expect(p.actions).toBe(1);
  });

  it("frees nothing for a ticked move no drive has room for", () => {
    const base = input({ destRoom: { "/mnt/usb0": 10 * GB }, candidates: [move("A", 60, 300)] });
    const p = planWithSelection(base, new Set(["move:A"]));
    expect(p.steps[0].dest).toBeNull();
    expect(p.freedBytes).toBe(0);
    expect(runOrder(p)).toEqual([]);
  });
});

describe("ageBand", () => {
  it("bands by days since last seen, unknown after a month old", () => {
    expect(ageBand(null, NOW)).toBe(2.5);
    expect(ageBand(NOW - 100 * DAY, NOW)).toBe(1);
    expect(ageBand(NOW - 45 * DAY, NOW)).toBe(2);
    expect(ageBand(NOW - 10 * DAY, NOW)).toBe(3);
    expect(ageBand(NOW - DAY, NOW)).toBe(4);
  });
});

describe("drives", () => {
  it("treats /user and /data as the one internal drive", () => {
    expect(driveToken("/data")).toBe("internal");
    expect(driveToken("/user")).toBe("internal");
    expect(driveToken("/mnt/ext1")).toBe("ext1");
    expect(driveToken("/mnt/usb0")).toBe("usb0");
    expect(sameDrive("/user", "/data")).toBe(true);
    expect(sameDrive("/data", "/mnt/ext0")).toBe(false);
  });

  it("finds the drive a path lives on", () => {
    const vols = ["/data", "/mnt/ext1"];
    expect(volumeOfPath("/mnt/ext1/user/app/X/app.pkg", vols)).toBe("/mnt/ext1");
    expect(volumeOfPath("/user/app/X/app.pkg", vols)).toBe("/data");
    expect(volumeOfPath("/mnt/usb0/x", vols)).toBeNull();
  });

  it("resolves a requested drive", () => {
    expect(resolveDrive("internal", ["/mnt/usb0", "/user"])).toBe("/user");
    expect(resolveDrive("/data", ["/user"])).toBe("/user");
    expect(resolveDrive("/mnt/usb0", ["/data", "/mnt/usb0"])).toBe("/mnt/usb0");
    expect(resolveDrive("/mnt/ext9", ["/data"])).toBeNull();
  });
});

describe("planner link", () => {
  it("round-trips size, drive and what to retry", () => {
    const link = spacePlanLink({
      neededBytes: 95 * GB,
      drive: "/mnt/ext1",
      retry: { kind: "pkg", path: "/data/pkg/X.pkg", label: "Game X" },
    });
    expect(link.startsWith("/cleanup?")).toBe(true);
    expect(parseSpacePlanQuery(link.slice(link.indexOf("?")))).toEqual({
      neededBytes: 95 * GB,
      drive: "/mnt/ext1",
      retry: { kind: "pkg", path: "/data/pkg/X.pkg", label: "Game X" },
    });
  });

  it("ignores a page without a plan request and refuses to go back off the app", () => {
    expect(parseSpacePlanQuery("?foo=1")).toBeNull();
    expect(parseSpacePlanQuery("?plan=1&retry=back&retry_to=//evil.example&retry_label=x")?.retry).toBeUndefined();
    expect(parseSpacePlanQuery("?plan=1&retry=back&retry_to=/games/PPSA1&retry_label=x")?.retry).toEqual({
      kind: "back",
      to: "/games/PPSA1",
      label: "x",
    });
    expect(parseSpacePlanQuery("?plan=1&need=abc")).toEqual({ neededBytes: 0, drive: "internal" });
  });

  it("reads and writes sizes in GiB", () => {
    expect(parseSizeGib("95")).toBe(95 * GB);
    expect(parseSizeGib("1,5")).toBe(1.5 * GB);
    expect(parseSizeGib("-3")).toBeNull();
    expect(parseSizeGib("x")).toBeNull();
    expect(gibText(95 * GB)).toBe("95");
    expect(gibText(95 * GB + 1)).toBe("95.1");
    expect(gibText(0)).toBe("");
  });
});

describe("buildCandidates", () => {
  const base = (over: Partial<CandidateInput> = {}): CandidateInput => ({
    target: "/data",
    volumes: ["/data", "/mnt/usb0", "/mnt/ps5upload/img"],
    imageMounts: ["/mnt/ps5upload/img"],
    leftovers: [],
    library: [],
    folderBytes: {},
    inUse: { paths: new Set(), imageNames: new Set() },
    installed: [],
    collectionIds: new Set(),
    running: new Set(),
    lastPlayed: () => null,
    ...over,
  });
  const item = (id: number, over: Partial<CandidateInput["leftovers"][number]> = {}) => ({
    id,
    cat: "C",
    drive: "internal",
    action: "delete",
    default: true,
    bytes: 5 * GB,
    alloc_bytes: 6 * GB,
    path: `/data/pkg/${id}.pkg`,
    ...over,
  });

  it("keeps the leftovers Clean up would tick on this drive, never updates of installed games", () => {
    const c = buildCandidates(
      base({
        leftovers: [
          item(1),
          item(2, { drive: "usb0" }),
          item(3, { cat: "A", action: "sony" }),
          item(4, { cat: "G", action: "none" }),
          item(5, { default: false }),
          item(6, { action: "none" }),
        ],
      }),
    );
    expect(c.map((x) => x.id)).toEqual(["leftover:1"]);
    expect(c[0].bytes).toBe(6 * GB);
  });

  it("never offers what Kraken Mount uses: its packages and game folders are the games (#894)", () => {
    const c = buildCandidates(
      base({
        leftovers: [item(1, { path: "/data/homebrew/1.pkg" }), item(2, { kept: "kraken" }), item(3)],
        library: [
          { kind: "game", name: "Mounted", path: "/data/homebrew/PPSA01234", volume: "/data", size: 0 },
          { kind: "game", name: "Mine", path: "/data/games/PPSA05555", volume: "/data", size: 0 },
        ],
        folderBytes: { "/data/homebrew/PPSA01234": 10 * GB, "/data/games/PPSA05555": 10 * GB },
        krakenKeeps: (p) => p.startsWith("/data/homebrew/"),
      }),
    );
    expect(c.map((x) => x.id).sort()).toEqual(["leftover:3", "move:/data/games/PPSA05555"]);
  });

  it("offers to move Library games on this drive that nothing runs from", () => {
    const c = buildCandidates(
      base({
        library: [
          { kind: "game", name: "Free", path: "/data/homebrew/Free", volume: "/data", size: 0, titleId: "PPSA1" },
          { kind: "game", name: "Registered", path: "/data/homebrew/Reg", volume: "/data", size: 0 },
          { kind: "image", name: "Mounted.exfat", path: "/data/img/Mounted.exfat", volume: "/data", size: 9 * GB, imageFormat: "exfat" },
          { kind: "image", name: "SMP.ffpfsc", path: "/data/img/SMP.ffpfsc", volume: "/data", size: 9 * GB, imageFormat: "ffpfsc" },
          { kind: "image", name: "Loose.ffpkg", path: "/data/img/Loose.ffpkg", volume: "/data", size: 9 * GB, imageFormat: "ffpkg" },
          { kind: "image", name: "Save.ffpfs", path: "/data/img/Save.ffpfs", volume: "/data", size: 1 * GB, imageFormat: "ffpfs" },
          { kind: "game", name: "Inside", path: "/mnt/ps5upload/img/G", volume: "/mnt/ps5upload/img", size: 0 },
          { kind: "game", name: "OnUsb", path: "/mnt/usb0/homebrew/U", volume: "/mnt/usb0", size: 0 },
          { kind: "game", name: "Running", path: "/data/homebrew/Run", volume: "/data", size: 0, titleId: "ppsa9" },
        ],
        folderBytes: {
          "/data/homebrew/Free": 40 * GB,
          "/data/homebrew/Reg": 40 * GB,
          "/data/homebrew/Run": 40 * GB,
        },
        inUse: { paths: new Set(["/data/homebrew/Reg", "/data/img/Mounted.exfat"]), imageNames: new Set(["SMP"]) },
        running: new Set(["PPSA9"]),
        lastPlayed: (id) => (id === "PPSA1" ? NOW - 5 * DAY : null),
      }),
    );
    expect(c.map((x) => x.id)).toEqual(["move:/data/homebrew/Free", "move:/data/img/Loose.ffpkg"]);
    expect(c[0].bytes).toBe(40 * GB);
    expect(c[0].lastPlayedMs).toBe(NOW - 5 * DAY);
    // Never into an image mount, never to the same drive.
    expect(c[0].moveTargets).toEqual(["/mnt/usb0"]);
  });

  it("offers to uninstall only package installs on this drive with a copy in the Collection", () => {
    const c = buildCandidates(
      base({
        installed: [
          { titleId: "PPSA1", titleName: "Has copy", origin: "pkg", system: false, base: { path: "/user/app/PPSA1/app.pkg", size: 50 * GB } },
          { titleId: "PPSA2", titleName: "No copy", origin: "pkg", system: false, base: { path: "/user/app/PPSA2/app.pkg", size: 50 * GB } },
          { titleId: "PPSA3", titleName: "Elsewhere", origin: "pkg", system: false, base: { path: "/mnt/ext1/user/app/PPSA3/app.pkg", size: 50 * GB } },
          { titleId: "PPSA4", titleName: "Folder", origin: "registered", system: false, base: null },
          { titleId: "NPXS1", titleName: "System", origin: "pkg", system: true, base: { path: "/user/app/NPXS1/app.pkg", size: GB } },
          { titleId: "PPSA5", titleName: "Unreadable", origin: "pkg", system: false, base: null },
          { titleId: "PPSA6", titleName: "Running", origin: "pkg", system: false, base: { path: "/user/app/PPSA6/app.pkg", size: GB } },
        ],
        collectionIds: new Set(["PPSA1", "PPSA3", "PPSA4", "NPXS1", "PPSA5", "PPSA6"]),
        running: new Set(["PPSA6"]),
      }),
    );
    // The no-copy game is built (so the rule is visible) but planSpace never uses it.
    expect(c.map((x) => [x.id, x.hasCopy])).toEqual([
      ["uninstall:PPSA1", true],
      ["uninstall:PPSA2", false],
    ]);
    const p = planSpace(input({ candidates: c, neededBytes: 200 * GB }));
    expect(p.steps.map((s) => s.candidate.id)).toEqual(["uninstall:PPSA1"]);
  });
});
