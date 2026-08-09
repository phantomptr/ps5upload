import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// pkgLibrary pulls in the Tauri invoke bridge + the ps5 api at module
// load; stub both so importing the store doesn't touch a real backend.
// (Hoisted by vitest above the imports below — affects this whole file,
// but the pure titleIdFromContentId tests don't care.)
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// invokeLogged branches on isTauriEnv() to route to browserInvoke instead of
// the mocked Tauri invoke above; force the Tauri path so this mock is used.
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../api/ps5", () => ({
  fsListDir: vi.fn(async () => []),
  fsDelete: vi.fn(async () => {}),
  fsMkdir: vi.fn(async () => {}),
  fsCopy: vi.fn(async () => {}),
  fsOpStatus: vi.fn(async () => ({ total_bytes: 0, bytes_copied: 0 })),
  // Refresh kicks off background metadata enrichment; default to "no data" so
  // store tests don't need a console. Individual tests can override.
  pkgMetadataConsole: vi.fn(async () => null),
  // Install fires a best-effort PS5 toast; stub it so store tests don't reach a
  // console.
  toastPush: vi.fn(async () => ({ ok: true })),
  // Pre-install space check; default to "plenty free" so store tests aren't
  // blocked. (null would also be fine — it means "couldn't read, don't block".)
  installFreeBytes: vi.fn(async () => 1_000_000_000_000),
  // Console-readiness probe; default to "ready" so install tests proceed past
  // the pre-install gate immediately. Readiness-specific tests override it.
  consoleReadiness: vi.fn(async () => true),
  pkgInstalledInventory: vi.fn(async () => []),
}));
// No active transfer in tests → installs proceed immediately.
vi.mock("../lib/ps5Transfers", () => ({ transferScreenBusy: () => false }));

import { invoke } from "@tauri-apps/api/core";
import {
  fsDelete,
  fsListDir,
  fsCopy,
  pkgMetadataConsole,
  consoleReadiness,
  pkgInstalledInventory,
} from "../api/ps5";
import {
  titleIdFromContentId,
  platformFromTitleId,
  pkgEntryInstallOrder,
  pkgAlternativeKey,
  pkgAlternativeGroups,
  pkgEntryIdentity,
  pkgInstallAllPlan,
  pkgLibraryStore,
  evictPkgLibraryStore,
  isFinishedPkg,
  pkgInstallMayNotLaunch,
  pkgTypeForCategory,
  pkgRowInstalled,
  installSpaceWarning,
  installedLastResult,
  runPkgInstall,
  waitForConsoleReady,
  recordPkgInstalled,
  isPkgInstalledHere,
  loadPkgAlternativeSelections,
  recordPkgAlternativeSelection,
  PKG_MAY_NOT_LAUNCH_MESSAGE,
  PKG_ACCEPTED_UNVERIFIED_HINT,
  PKG_PATCH_REJECTED_HINT,
  type PkgEntry,
} from "./pkgLibrary";
import { useTaskStore } from "./tasks";

describe("platformFromTitleId", () => {
  it("maps CUSA → ps4 and PPSA → ps5", () => {
    expect(platformFromTitleId("CUSA03474")).toBe("ps4");
    expect(platformFromTitleId("PPSA01650")).toBe("ps5");
  });
  it("returns empty for unknown prefixes / missing ids", () => {
    expect(platformFromTitleId("NPXS40047")).toBe("");
    expect(platformFromTitleId(null)).toBe("");
    expect(platformFromTitleId(undefined)).toBe("");
    expect(platformFromTitleId("")).toBe("");
  });
});

describe("pkgEntryInstallOrder", () => {
  it("orders by PARAM.SFO category: base(0) < update(1) < DLC(2)", () => {
    expect(pkgEntryInstallOrder({ category: "gd", path: "/x.pkg" })).toBe(0);
    expect(pkgEntryInstallOrder({ category: "gp", path: "/x.pkg" })).toBe(1);
    expect(pkgEntryInstallOrder({ category: "ac", path: "/x.pkg" })).toBe(2);
  });
  it("falls back to a path hint when category is absent (headerless rows)", () => {
    expect(
      pkgEntryInstallOrder({
        category: undefined,
        path: "/data/updates/p.pkg",
      }),
    ).toBe(1);
    expect(
      pkgEntryInstallOrder({ category: undefined, path: "/data/dlc/p.pkg" }),
    ).toBe(2);
    expect(
      pkgEntryInstallOrder({
        category: undefined,
        path: `/data/updates/${"a".repeat(64)}/p.pkg`,
      }),
    ).toBe(1);
  });
  it("defaults an unknown/base-shaped row to 0", () => {
    expect(
      pkgEntryInstallOrder({ category: undefined, path: "/data/game.pkg" }),
    ).toBe(0);
    expect(
      pkgEntryInstallOrder({ category: "xx", path: "/data/game.pkg" }),
    ).toBe(0);
  });
  it("prefers the category over a misleading path hint", () => {
    // A base game that happens to live under an /updates/ dir must still be 0.
    expect(
      pkgEntryInstallOrder({ category: "gd", path: "/data/updates/base.pkg" }),
    ).toBe(0);
  });
  it("sorts a mixed batch base → update → DLC (stable within a tier)", () => {
    const rows = [
      { category: "ac", path: "/dlc1.pkg" },
      { category: "gp", path: "/upd.pkg" },
      { category: "gd", path: "/base2.pkg" },
      { category: "gd", path: "/base1.pkg" },
    ];
    const sorted = rows
      .slice()
      .sort((a, b) => pkgEntryInstallOrder(a) - pkgEntryInstallOrder(b))
      .map((r) => r.path);
    expect(sorted).toEqual([
      "/base2.pkg",
      "/base1.pkg",
      "/upd.pkg",
      "/dlc1.pkg",
    ]);
  });
});

describe("pkgInstallAllPlan (variant safety)", () => {
  const row = (p: Partial<PkgEntry> & Pick<PkgEntry, "path">): PkgEntry => ({
    name: p.path.split("/").pop() || "x.pkg",
    path: p.path,
    size: p.size ?? 1,
    contentId: p.contentId ?? "UP0000-CUSA33334_00-TEST000000000000",
    titleId: p.titleId ?? "CUSA33334",
    category: p.category,
    appVer: p.appVer,
    fingerprint: p.fingerprint,
    status: p.status ?? "idle",
    installedHere: p.installedHere,
  });

  it("preserves but skips two distinct same-version patch alternatives", () => {
    const optional = row({
      path: "/updates/optional.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "a".repeat(64),
    });
    const backport = row({
      path: "/updates/backport.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "b".repeat(64),
    });
    const plan = pkgInstallAllPlan([optional, backport]);
    expect(plan.targets).toEqual([]);
    expect(plan.conflicts.map((e) => e.path)).toEqual([
      optional.path,
      backport.path,
    ]);
    expect(pkgAlternativeKey(optional)).toBe("patch:CUSA33334:01.02");
  });

  it("does not let Install all replace a previously-installed sibling variant", () => {
    const optional = row({
      path: "/updates/optional.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "a".repeat(64),
      installedHere: true,
    });
    const backport = row({
      path: "/updates/backport.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "b".repeat(64),
    });
    const plan = pkgInstallAllPlan([optional, backport]);
    expect(plan.targets).toEqual([]);
    expect(plan.conflicts.map((e) => e.path)).toEqual([backport.path]);
  });

  it("installs exactly the explicitly selected same-version variant", () => {
    const optional = row({
      path: "/updates/optional.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "a".repeat(64),
    });
    const backport = row({
      path: "/updates/backport.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "b".repeat(64),
    });
    const key = pkgAlternativeKey(optional)!;
    const plan = pkgInstallAllPlan([optional, backport], {
      [key]: pkgEntryIdentity(backport),
    });
    expect(plan.conflicts).toEqual([]);
    expect(plan.targets.map((entry) => entry.path)).toEqual([backport.path]);
  });

  it("treats a stale selection as unresolved instead of choosing by list order", () => {
    const optional = row({
      path: "/updates/optional.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "a".repeat(64),
    });
    const backport = row({
      path: "/updates/backport.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "b".repeat(64),
    });
    const key = pkgAlternativeKey(optional)!;
    const plan = pkgInstallAllPlan([optional, backport], {
      [key]: "removed-artifact",
    });
    expect(plan.targets).toEqual([]);
    expect(plan.conflicts).toHaveLength(2);
  });

  it("reports alternative groups while leaving different versions independent", () => {
    const optional = row({
      path: "/updates/optional.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "a".repeat(64),
    });
    const backport = row({
      path: "/updates/backport.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "b".repeat(64),
    });
    const newer = row({
      path: "/updates/103.pkg",
      category: "gp",
      appVer: "01.03",
      fingerprint: "c".repeat(64),
    });
    const groups = pkgAlternativeGroups([optional, backport, newer]);
    expect(groups).toHaveLength(1);
    expect(groups[0].entries.map((entry) => entry.path)).toEqual([
      optional.path,
      backport.path,
    ]);
  });

  it("batches base, different patch versions, and independent DLC in order", () => {
    const base = row({ path: "/base.pkg", category: "gd" });
    const patch102 = row({
      path: "/updates/102.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "2".repeat(64),
    });
    const patch101 = row({
      path: "/updates/101.pkg",
      category: "gp",
      appVer: "01.01",
      fingerprint: "1".repeat(64),
    });
    const dlc = row({
      path: "/dlc/one.pkg",
      category: "ac",
      contentId: "UP0000-CUSA33334_00-DLC000000000001",
      fingerprint: "d".repeat(64),
    });
    expect(
      pkgInstallAllPlan([dlc, patch102, base, patch101]).targets.map(
        (e) => e.path,
      ),
    ).toEqual([base.path, patch101.path, patch102.path, dlc.path]);
  });

  it("treats same-ContentID DLC repacks as alternatives but not separate DLC", () => {
    const first = row({
      path: "/dlc/a.pkg",
      category: "ac",
      contentId: "UP0000-CUSA33334_00-DLC000000000001",
      fingerprint: "a".repeat(64),
    });
    const repack = row({
      path: "/dlc/b.pkg",
      category: "ac",
      contentId: first.contentId,
      fingerprint: "b".repeat(64),
    });
    const separate = row({
      path: "/dlc/c.pkg",
      category: "ac",
      contentId: "UP0000-CUSA33334_00-DLC000000000002",
      fingerprint: "c".repeat(64),
    });
    const plan = pkgInstallAllPlan([first, repack, separate]);
    expect(plan.conflicts.map((e) => e.path)).toEqual([
      first.path,
      repack.path,
    ]);
    expect(plan.targets.map((e) => e.path)).toEqual([separate.path]);
  });

  it("does not manufacture a conflict for duplicate rows of one exact artifact", () => {
    const a = row({
      path: "/legacy/a.pkg",
      category: "gp",
      appVer: "01.02",
      fingerprint: "a".repeat(64),
    });
    const b = row({ ...a, path: "/new/a.pkg" });
    expect(pkgInstallAllPlan([a, b]).conflicts).toEqual([]);
  });
});

describe("addAndUpload drag-event deduplication", () => {
  const host = "192.168.55.5";
  const localPath = "/tmp/OptionalFix.pkg";

  afterEach(() => {
    vi.mocked(invoke).mockReset();
    evictPkgLibraryStore(host);
  });

  it("parses and starts only one add when two surfaces report the same drop", async () => {
    let resolveMetadata!: (value: unknown) => void;
    const metadata = new Promise((resolve) => {
      resolveMetadata = resolve;
    });
    vi.mocked(invoke).mockImplementation(async (command: unknown) => {
      if (command === "pkg_metadata_split") return metadata;
      // No job id makes the first call settle quickly after the assertion;
      // the important contract is that the second call never reaches parsing.
      if (command === "transfer_file") return {};
      return {};
    });

    const first = pkgLibraryStore(host)
      .getState()
      .addAndUpload(localPath, host);
    await Promise.resolve();
    const second = pkgLibraryStore(host)
      .getState()
      .addAndUpload(localPath, host);
    await second;

    expect(
      vi
        .mocked(invoke)
        .mock.calls.filter(([command]) => command === "pkg_metadata_split"),
    ).toHaveLength(1);

    resolveMetadata({
      parts: [localPath],
      total_size: 8_192_000,
      head: {
        content_id: "UP0000-CUSA33334_00-TEST000000000000",
        title: "Test",
        category: "gp",
        app_ver: "01.02",
        fingerprint: "a".repeat(64),
      },
    });
    await first;
  });
});

describe("per-console store registry (eviction)", () => {
  it("returns the SAME store instance for a host until evicted", () => {
    const a1 = pkgLibraryStore("192.168.50.1");
    const a2 = pkgLibraryStore("192.168.50.1:9114"); // port stripped → same key
    expect(a2).toBe(a1);
  });

  it("hands out a FRESH store after eviction (stale state can't resurface)", () => {
    const host = "192.168.50.2";
    const before = pkgLibraryStore(host);
    // Dirty the transient state that a re-listing wouldn't clear.
    before.setState({ busyNotice: "installing…" });
    expect(before.getState().busyNotice).toBe("installing…");

    evictPkgLibraryStore(host);
    const after = pkgLibraryStore(host);
    expect(after).not.toBe(before);
    expect(after.getState().busyNotice).toBeNull();
  });

  it("evicting an unknown host is a no-op (no throw)", () => {
    expect(() => evictPkgLibraryStore("203.0.113.7")).not.toThrow();
  });
});

describe("titleIdFromContentId", () => {
  it("extracts the title id from a standard ContentID", () => {
    expect(titleIdFromContentId("EP9000-CUSA00207_00-BLOODBORNE000000")).toBe(
      "CUSA00207",
    );
    expect(titleIdFromContentId("IV0002-PPSA01234_00-SOMEGAME00000000")).toBe(
      "PPSA01234",
    );
  });

  it("handles homebrew/region prefixes that still carry a title id", () => {
    expect(titleIdFromContentId("UP1234-PLAS10000_00-XYZ")).toBe("PLAS10000");
  });

  it("returns null when there is no well-formed title id", () => {
    expect(titleIdFromContentId("")).toBeNull();
    expect(titleIdFromContentId("HB0000-HOMEBREW_00-X")).toBeNull(); // not 4+5
    expect(titleIdFromContentId("HB0000-12345678_00-X")).toBeNull(); // all digits
    expect(titleIdFromContentId("justastring")).toBeNull();
    expect(titleIdFromContentId("AB-CD-EF")).toBeNull();
  });

  it("requires exactly four letters then five digits", () => {
    // FAKE00001 = FAKE (4) + 00001 (5) → valid shape.
    expect(titleIdFromContentId("HB0000-FAKE00001_00-X")).toBe("FAKE00001");
    // too few digits
    expect(titleIdFromContentId("HB0000-CUSA0020_00-X")).toBeNull();
    // lowercase letters not accepted
    expect(titleIdFromContentId("HB0000-cusa00207_00-X")).toBeNull();
  });
});

// ── may-not-launch surfacing (the 2.27.x FW-12 install fix) ─────────────────

describe("pkgTypeForCategory (patch data-loss guard input)", () => {
  it("maps a patch ('gp') to a …DP type that arms the payload guard", () => {
    // THE load-bearing case: a patch shares its base game's content_id, so it
    // MUST be tagged "…DP" or a fallback tier re-registers the id and wipes the
    // base (hardware-confirmed: a Jak X patch deleted its 3.8 GB base).
    expect(pkgTypeForCategory("gp")).toBe("PS4DP");
    expect(pkgTypeForCategory("gp")?.endsWith("DP")).toBe(true);
  });
  it("maps a full game ('gd') to PS4GD (not a …DP type)", () => {
    expect(pkgTypeForCategory("gd")).toBe("PS4GD");
    expect(pkgTypeForCategory("gd")?.endsWith("DP")).toBe(false);
  });
  it("maps DLC ('ac' — its own content_id) to PS4AC, not …DP", () => {
    expect(pkgTypeForCategory("ac")).toBe("PS4AC");
  });
  it("returns null for unknown/absent category (payload keeps its default)", () => {
    expect(pkgTypeForCategory(undefined)).toBeNull();
    expect(pkgTypeForCategory(null)).toBeNull();
    expect(pkgTypeForCategory("")).toBeNull();
    expect(pkgTypeForCategory("zz")).toBeNull();
  });
});

describe("installSpaceWarning (#115 pre-install free-space check)", () => {
  it("warns when the pkg is larger than free space", () => {
    const w = installSpaceWarning("Big Game", 50_000_000_000, 10_000_000_000);
    expect(w).toMatch(/Not enough free space/);
    expect(w).toMatch(/Big Game/);
  });
  it("does not warn when it fits", () => {
    expect(
      installSpaceWarning("Game", 5_000_000_000, 50_000_000_000),
    ).toBeNull();
  });
  it("never blocks when free space is unknown (null)", () => {
    expect(installSpaceWarning("Game", 50_000_000_000, null)).toBeNull();
  });
  it("ignores a zero/unknown pkg size", () => {
    expect(installSpaceWarning("Game", 0, 1)).toBeNull();
  });
});

describe("pkgRowInstalled (installed/Reinstall badge)", () => {
  // The reported bug: installing a base game made its never-installed UPDATE
  // (and DLC) show "INSTALLED · Reinstall", because an add-on shares the base
  // game's title id and the console's app_list is keyed on title id.
  const installed = new Set(["CUSA07842"]); // base game is on the console

  it("marks an installed BASE game as installed", () => {
    expect(
      pkgRowInstalled({ titleId: "CUSA07842", category: "gd" }, installed),
    ).toBe(true);
    // A root-level pkg of unknown category is treated as a base game.
    expect(
      pkgRowInstalled({ titleId: "CUSA07842", category: undefined }, installed),
    ).toBe(true);
  });

  it("does NOT mark an UPDATE installed just because its base is", () => {
    // The load-bearing regression case — the update shares CUSA07842 but was
    // never itself installed, so it must read as installable, not "Reinstall".
    expect(
      pkgRowInstalled({ titleId: "CUSA07842", category: "gp" }, installed),
    ).toBe(false);
  });

  it("does NOT mark DLC installed off the base game's title id", () => {
    expect(
      pkgRowInstalled({ titleId: "CUSA07842", category: "ac" }, installed),
    ).toBe(false);
  });

  it("is not installed when the base title isn't on the console", () => {
    expect(
      pkgRowInstalled({ titleId: "CUSA99999", category: "gd" }, installed),
    ).toBe(false);
  });

  it("is not installed without a derivable title id", () => {
    expect(
      pkgRowInstalled({ titleId: undefined, category: "gd" }, installed),
    ).toBe(false);
  });

  // The follow-up bug: once we install THIS update/DLC ourselves it must read
  // "Reinstall". `installedHere` is our per-package record (app_list can't
  // confirm an add-on), so it wins regardless of category or app_list state.
  it("marks an UPDATE installed once we've installed it here", () => {
    expect(
      pkgRowInstalled(
        { titleId: "CUSA07842", category: "gp", installedHere: true },
        installed,
      ),
    ).toBe(true);
  });

  it("marks DLC installed once we've installed it here", () => {
    expect(
      pkgRowInstalled(
        { titleId: "CUSA07842", category: "ac", installedHere: true },
        installed,
      ),
    ).toBe(true);
  });

  it("honours installedHere even when the base isn't in app_list", () => {
    // e.g. a standalone update staged on a console where the base was removed —
    // we still installed this package, so it reads "Reinstall".
    expect(
      pkgRowInstalled(
        { titleId: "CUSA99999", category: "gp", installedHere: true },
        installed,
      ),
    ).toBe(true);
  });

  it("matches only the exact installed patch variant when live artifacts exist", () => {
    const optionalFingerprint = "a".repeat(64);
    const backportFingerprint = "b".repeat(64);
    const artifacts = [
      {
        kind: "patch" as const,
        size: 15_335_424,
        fingerprint: backportFingerprint,
        contentId: "UP1082-CUSA33334_00-SLUS008930000000",
      },
    ];
    expect(
      pkgRowInstalled(
        {
          titleId: "CUSA33334",
          category: "gp",
          size: 8_192_000,
          fingerprint: optionalFingerprint,
          installedHere: true,
        },
        new Set(["CUSA33334"]),
        artifacts,
      ),
    ).toBe(false);
    expect(
      pkgRowInstalled(
        {
          titleId: "CUSA33334",
          category: "gp",
          size: 15_335_424,
          fingerprint: backportFingerprint,
        },
        new Set(["CUSA33334"]),
        artifacts,
      ),
    ).toBe(true);
  });

  it("matches a cold-scan 128-bit directory token to the full installed fingerprint", () => {
    const fullFingerprint = "0123456789abcdef".repeat(4);
    expect(
      pkgRowInstalled(
        {
          titleId: "CUSA33334",
          category: "gp",
          fingerprint: fullFingerprint.slice(0, 32),
        },
        new Set(["CUSA33334"]),
        [
          {
            kind: "patch",
            size: 15_335_424,
            fingerprint: fullFingerprint,
            contentId: "UP1082-CUSA33334_00-SLUS008930000000",
          },
        ],
      ),
    ).toBe(true);
  });

  it("uses DLC ContentID and size for a legacy row without a fingerprint", () => {
    expect(
      pkgRowInstalled(
        {
          titleId: "CUSA07842",
          category: "ac",
          size: 4096,
          contentId: "UP0000-CUSA07842_00-DLC000000000001",
        },
        installed,
        [
          {
            kind: "dlc",
            size: 4096,
            fingerprint: "f".repeat(64),
            contentId: "UP0000-CUSA07842_00-DLC000000000001",
          },
        ],
      ),
    ).toBe(true);
  });
});

describe("waitForConsoleReady (install readiness gate)", () => {
  const mockedReady = vi.mocked(consoleReadiness);
  beforeEach(() => {
    mockedReady.mockReset();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    // Restore the module default ("ready") so later install suites sail through
    // the pre-install gate instead of inheriting this suite's not-ready stub.
    mockedReady.mockReset();
    mockedReady.mockResolvedValue(true);
  });

  it("returns true immediately when the console is already ready (no wait)", async () => {
    mockedReady.mockResolvedValue(true);
    const p = waitForConsoleReady("192.168.1.10");
    await expect(p).resolves.toBe(true);
    expect(mockedReady).toHaveBeenCalledTimes(1); // first probe, no delay
  });

  it("polls until the console becomes ready", async () => {
    // Not ready twice, then ready.
    mockedReady
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(false)
      .mockResolvedValue(true);
    const p = waitForConsoleReady("192.168.1.10");
    // Let the polling timers + awaits flush.
    await vi.advanceTimersByTimeAsync(5_000);
    await expect(p).resolves.toBe(true);
    expect(mockedReady.mock.calls.length).toBeGreaterThanOrEqual(3);
  });

  it("gives up (false) after the timeout when never ready", async () => {
    mockedReady.mockResolvedValue(false);
    const p = waitForConsoleReady("192.168.1.10", { timeoutMs: 4_500 });
    await vi.advanceTimersByTimeAsync(10_000);
    // timeoutMs/POLL(1500) = 3 attempts, then false.
    await expect(p).resolves.toBe(false);
  });
});

describe("recordPkgInstalled / isPkgInstalledHere (per-console isolation)", () => {
  // The reported bug: the same .pkg staged on multiple consoles lands at an
  // identical path, and installing on ONE console used to mark it installed on
  // ALL of them (the flag was keyed on path only). It must be scoped per host.
  beforeEach(() => {
    const store = new globalThis.Map<string, string>();
    (globalThis as { window?: unknown }).window = {
      localStorage: {
        getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
        setItem: (k: string, v: string) => void store.set(k, String(v)),
        removeItem: (k: string) => void store.delete(k),
        clear: () => store.clear(),
      },
    };
  });
  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
  });

  const PATH = "/data/ps5upload/pkg_temp/Game[v01.04].pkg";
  const A = "192.168.1.10";
  const B = "192.168.1.20";

  it("installing on one console does NOT mark it installed on another", () => {
    recordPkgInstalled(A, PATH);
    expect(isPkgInstalledHere(A, PATH)).toBe(true);
    // The sibling console with the SAME staged path must read as not-installed.
    expect(isPkgInstalledHere(B, PATH)).toBe(false);
  });

  it("normalizes host:port to the bare host (addr form is accepted)", () => {
    recordPkgInstalled("192.168.1.10:9113", PATH);
    expect(isPkgInstalledHere("192.168.1.10", PATH)).toBe(true);
    expect(isPkgInstalledHere("192.168.1.10:1234", PATH)).toBe(true);
  });

  it("tracks distinct paths per console independently", () => {
    const other = "/data/ps5upload/pkg_temp/Other.pkg";
    recordPkgInstalled(A, PATH);
    expect(isPkgInstalledHere(A, PATH)).toBe(true);
    expect(isPkgInstalledHere(A, other)).toBe(false);
  });

  it("clears a replaced alternative without touching another console", () => {
    const backport = "/data/updates/backport.pkg";
    recordPkgInstalled(A, PATH);
    recordPkgInstalled(B, PATH);
    recordPkgInstalled(A, backport, [PATH]);
    expect(isPkgInstalledHere(A, PATH)).toBe(false);
    expect(isPkgInstalledHere(A, backport)).toBe(true);
    expect(isPkgInstalledHere(B, PATH)).toBe(true);
  });

  it("keeps alternative choices isolated per console", () => {
    const key = "patch:CUSA33334:01.02";
    recordPkgAlternativeSelection(A, key, "optional-fingerprint");
    recordPkgAlternativeSelection(B, key, "backport-fingerprint");
    expect(loadPkgAlternativeSelections(A)[key]).toBe("optional-fingerprint");
    expect(loadPkgAlternativeSelections(B)[key]).toBe("backport-fingerprint");
  });
});

describe("pkgInstallMayNotLaunch", () => {
  it("trusts the engine's explicit may_not_launch flag", () => {
    expect(pkgInstallMayNotLaunch({ may_not_launch: true })).toBe(true);
    expect(pkgInstallMayNotLaunch({ may_not_launch: false })).toBe(false);
    // Flag wins even if register_path would say otherwise.
    expect(
      pkgInstallMayNotLaunch({
        may_not_launch: false,
        register_path: "appinst-local",
      }),
    ).toBe(false);
  });

  it("falls back to register_path for older engines without the flag", () => {
    // Only the unlaunchable last-resort path warns.
    expect(pkgInstallMayNotLaunch({ register_path: "appinst-local" })).toBe(
      true,
    );
    // Every launchable tier does not.
    for (const rp of [
      "appinst",
      "shellui-rpc",
      "intdebug",
      "regular",
      "tier0-worker",
      "none",
      "",
    ]) {
      expect(pkgInstallMayNotLaunch({ register_path: rp })).toBe(false);
    }
    // Nothing at all (very old engine) → no warning.
    expect(pkgInstallMayNotLaunch({})).toBe(false);
  });

  it("prefers the engine's definitive app.db launchability verdict", () => {
    // launchable=true overrides even the unlaunchable register_path: the
    // engine confirmed the title registered in app.db, so it's a clean
    // success (this is the elf-arsenal wait_for_install_row payoff).
    expect(
      pkgInstallMayNotLaunch({
        register_path: "appinst-local",
        launchable: true,
      }),
    ).toBe(false);
    // launchable=false is a definitive warning even on a "launchable" tier —
    // Sony accepted it but the title never registered.
    expect(
      pkgInstallMayNotLaunch({ register_path: "appinst", launchable: false }),
    ).toBe(true);
    // launchable wins over a conflicting may_not_launch flag too.
    expect(
      pkgInstallMayNotLaunch({ may_not_launch: true, launchable: true }),
    ).toBe(false);
    // launchable null/undefined ⇒ verification not applicable ⇒ heuristic.
    expect(
      pkgInstallMayNotLaunch({
        register_path: "appinst-local",
        launchable: null,
      }),
    ).toBe(true);
  });
});

// ── delete_staging threading (the Auto-Delete data-loss fix) ────────────────
//
// runPkgInstall MUST forward the caller's delete-staging intent to the engine
// (pkg_install_start). Before the fix the engine always deleted the uploaded
// pkg regardless; the regression we're guarding is "Auto Delete off but the pkg
// was deleted anyway". A response without a status session is deliberately
// unverified (never installed), which also keeps this test fast.
describe("runPkgInstall — forwards deleteStaging to the engine", () => {
  const mockedInvoke = vi.mocked(invoke);
  const mockedInventory = vi.mocked(pkgInstalledInventory);

  beforeEach(() => {
    mockedInventory.mockReset().mockResolvedValue([]);
    mockedInvoke.mockReset();
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        // err_code 0 + no session_id ⇒ accepted, but completion unverified.
        return { err_code: 0, register_path: "shellui-rpc" };
      }
      return {};
    });
  });

  const startArgs = () =>
    mockedInvoke.mock.calls.find((c) => c[0] === "pkg_install_start")?.[1] as
      { deleteStaging?: boolean } | undefined;

  it("passes deleteStaging=false → engine KEEPS the pkg (Auto Delete off)", async () => {
    const r = await runPkgInstall(
      "192.168.1.50",
      "/user/data/x.pkg",
      "CID",
      null,
      false,
    );
    expect(r.installed).toBe(false);
    expect(r.acceptedUnverified).toBe(true);
    expect(startArgs()?.deleteStaging).toBe(false);
  });

  it("passes deleteStaging=true as a confirmed-completion cleanup preference", async () => {
    await runPkgInstall("192.168.1.50", "/user/data/x.pkg", "CID", null, true);
    expect(startArgs()?.deleteStaging).toBe(true);
  });

  it("a rejected patch (…DP) reaches DPI but is not called installed on rc=0 alone", async () => {
    // The user-reported case: a Jak X update rejected in-process with 0x80B21106
    // (a firmware authid gate; the base game itself lands via shellui-rpc, which
    // a patch can't use). The DPI daemon runs Sony's appinst in a separate
    // process and applies the update on top of the base — HW-proven safe. So a
    // patch MUST fall through to DPI, not bail with the raw error.
    mockedInvoke.mockReset();
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        return {
          err_code: 0x80b21106,
          err_message:
            "PS5 rejected the PKG header — file may be corrupt or wrongly named",
          register_path: "none",
          package_type: "PS4DP",
        };
      }
      if (cmd === "dpi_ensure") return { ok: true };
      if (cmd === "pkg_dpi_install") return { ok: true, rc: 0 };
      if (cmd === "payload_bundled_path")
        return { ok: true, path: "/tmp/p.elf" };
      return {};
    });
    const r = await runPkgInstall(
      "192.168.1.50",
      "/user/data/ps5upload/pkg_library/updates/CID.pkg",
      "CID",
      "PS4DP",
      false,
    );
    expect(r.installed).toBe(false);
    expect(r.acceptedUnverified).toBe(true);
    expect(r.errMessage).toBe(PKG_ACCEPTED_UNVERIFIED_HINT);
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_dpi_install"),
    ).toBe(true);
  });

  it("a rejected base game also reaches DPI with its staging path intact", async () => {
    mockedInvoke.mockReset();
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        return {
          err_code: 0x80b21106,
          register_path: "none",
          package_type: "PS4GD",
        };
      }
      if (cmd === "dpi_ensure") return { ok: true };
      if (cmd === "pkg_dpi_install") return { ok: true, rc: 0 };
      if (cmd === "payload_bundled_path") {
        return { ok: true, path: "/tmp/p.elf" };
      }
      return {};
    });

    const localPs5Path = "/user/data/ps5upload/pkg_library/CID.pkg";
    const r = await runPkgInstall(
      "192.168.1.50",
      localPs5Path,
      "CID",
      "PS4GD",
      true,
    );

    expect(r.acceptedUnverified).toBe(true);
    const dpiArgs = mockedInvoke.mock.calls.find(
      (call) => call[0] === "pkg_dpi_install",
    )?.[1] as { localPs5Path?: string } | undefined;
    expect(dpiArgs?.localPs5Path).toBe(localPs5Path);
  });

  it("confirms a staged DLC routed directly to DPI by exact fingerprint", async () => {
    const fingerprint = "a".repeat(64);
    const contentId = "UP9000-CUSA00900_00-SPDLCMESSENGER00";
    mockedInventory.mockResolvedValue([
      {
        kind: "dlc",
        path: "/mnt/ext1/user/addcont/CUSA00900/SPDLCMESSENGER00/ac.pkg",
        size: 1_048_576,
        fingerprint,
        contentId,
      },
    ]);
    mockedInvoke.mockReset();
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        return {
          err_code: 0xe0000008,
          err_message:
            "Staged DLC is being handed to the safer standalone DPI installer",
          register_path: "dpi-required",
          package_type: "PS4AC",
        };
      }
      if (cmd === "dpi_ensure") return { ok: true };
      if (cmd === "pkg_dpi_install") return { ok: true, rc: 0 };
      if (cmd === "payload_bundled_path") {
        return { ok: true, path: "/tmp/p.elf" };
      }
      return {};
    });

    const r = await runPkgInstall(
      "192.168.1.50",
      "/user/data/ps5upload/pkg_library/dlc/fp/addon.pkg",
      contentId,
      "PS4AC",
      false,
      undefined,
      undefined,
      { size: 1_048_576, fingerprint },
    );

    expect(r.installed).toBe(true);
    expect(r.acceptedUnverified).toBe(false);
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_dpi_install"),
    ).toBe(true);
  });

  it("confirms a DPI patch only when the exact installed fingerprint matches", async () => {
    const fingerprint = "7".repeat(64);
    const contentId = "UP1082-CUSA33334_00-SLUS008930000000";
    mockedInventory.mockResolvedValue([
      {
        kind: "patch",
        path: "/user/patch/CUSA33334/patch.pkg",
        size: 8_192_000,
        fingerprint,
        contentId,
      },
    ]);
    mockedInvoke.mockReset();
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        return {
          err_code: 0x80b21106,
          register_path: "none",
          package_type: "PS4DP",
        };
      }
      if (cmd === "dpi_ensure") return { ok: true };
      if (cmd === "pkg_dpi_install") return { ok: true, rc: 0 };
      if (cmd === "payload_bundled_path") {
        return { ok: true, path: "/tmp/p.elf" };
      }
      return {};
    });

    const r = await runPkgInstall(
      "192.168.1.50",
      "/user/data/ps5upload/pkg_library/updates/fp/CID.pkg",
      contentId,
      "PS4DP",
      false,
      undefined,
      undefined,
      { size: 8_192_000, fingerprint },
    );

    expect(r.installed).toBe(true);
    expect(r.acceptedUnverified).toBe(false);
    expect(mockedInventory).toHaveBeenCalledWith(
      "192.168.1.50:9113",
      "CUSA33334",
    );
  });

  it("a patch DPI can't apply gets update-specific guidance, not the raw error", async () => {
    mockedInvoke.mockReset();
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        return {
          err_code: 0x80b21106,
          register_path: "none",
          package_type: "PS4DP",
        };
      }
      if (cmd === "dpi_ensure") return { ok: true };
      if (cmd === "pkg_dpi_install")
        return { ok: false, rc: 0x80b21106, err_message: "raw" };
      if (cmd === "payload_bundled_path")
        return { ok: true, path: "/tmp/p.elf" };
      return {};
    });
    const r = await runPkgInstall(
      "192.168.1.50",
      "/user/data/ps5upload/pkg_library/updates/CID.pkg",
      "CID",
      "PS4DP",
      false,
    );
    expect(r.installed).toBe(false);
    expect(r.errMessage).toBe(PKG_PATCH_REJECTED_HINT);
  });
});

// ── progress-tracked completion (the large-pkg / Bloodborne data-loss fix) ──
//
// runPkgInstall must report `installed:true` ONLY when the engine confirms the
// install actually completed — never on a timer. A stall or async failure must
// leave `installed:false` (so callers KEEP the pkg) and carry the right copy.
// We drive the poll loop with fake timers so the 2.5s interval doesn't slow the
// suite.
describe("runPkgInstall — tracks the install to genuine completion", () => {
  const mockedInvoke = vi.mocked(invoke);

  beforeEach(() => {
    vi.useFakeTimers();
    mockedInvoke.mockReset();
    useTaskStore.setState({ tasks: [] });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const START_OK = {
    err_code: 0,
    register_path: "shellui-rpc",
    session_id: "s1",
  };

  it("reports completed only after the engine says 'done' — and surfaces live %", async () => {
    // Two "still installing" polls (growing bytes) then a confirmed done. The
    // old fixed-window code would have declared success on the FIRST poll.
    let n = 0;
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") return START_OK;
      if (cmd === "pkg_install_status") {
        n += 1;
        if (n < 3)
          return { phase: "install", installed_bytes: n * 1000, total: 3000 };
        return {
          phase: "done",
          launchable: true,
          installed_bytes: 3000,
          total: 3000,
        };
      }
      return {};
    });
    const progress: Array<[number, number]> = [];
    const promise = runPkgInstall(
      "192.168.1.50",
      "/user/data/x.pkg",
      "CID",
      null,
      true,
      (b, t) => progress.push([b, t]),
    );
    await vi.advanceTimersByTimeAsync(2600 * 4);
    const r = await promise;
    expect(r.installed).toBe(true);
    expect(r.stalled).toBeFalsy();
    // The live % was surfaced for the UI, ending at 100%-equivalent bytes.
    expect(progress.length).toBeGreaterThan(0);
    expect(progress[progress.length - 1]).toEqual([3000, 3000]);
    expect(useTaskStore.getState().tasks[0]).toMatchObject({
      kind: "pkg-install",
      status: "done",
      consoleId: "192.168.1.50",
    });
  });

  it("a STALL keeps the pkg: installed=false, stalled=true, retry copy", async () => {
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") return START_OK;
      if (cmd === "pkg_install_status")
        return {
          phase: "error",
          stalled: true,
          installed_bytes: 5_000_000_000,
          total: 25_000_000_000,
        };
      return {};
    });
    const promise = runPkgInstall(
      "192.168.1.50",
      "/user/data/x.pkg",
      "CID",
      null,
      true,
    );
    await vi.advanceTimersByTimeAsync(2600);
    const r = await promise;
    expect(r.installed).toBe(false); // ⇒ callers KEEP the pkg
    expect(r.stalled).toBe(true);
    expect(r.errMessage).toMatch(/kept on the PS5/i);
  });

  it("a Sony async failure is a (non-stall) failure, pkg kept", async () => {
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") return START_OK;
      if (cmd === "pkg_install_status")
        return { phase: "error", err_code: 0x80b21106, total: 25_000_000_000 };
      return {};
    });
    const promise = runPkgInstall(
      "192.168.1.50",
      "/user/data/x.pkg",
      "CID",
      null,
      true,
    );
    await vi.advanceTimersByTimeAsync(2600);
    const r = await promise;
    expect(r.installed).toBe(false);
    expect(r.stalled).toBeFalsy();
    expect(r.errMessage).toMatch(/Package Installer/);
  });

  it("keeps polling across a transient status blip instead of giving up", async () => {
    // A single failed poll must NOT end tracking (it once would have, and an
    // engine-GC'd-mid-large-install would falsely resolve). Recover → done.
    let n = 0;
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") return START_OK;
      if (cmd === "pkg_install_status") {
        n += 1;
        if (n === 1) throw new Error("transient socket blip");
        if (n === 2) return { phase: "install", installed_bytes: 1, total: 2 };
        return { phase: "done", launchable: null };
      }
      return {};
    });
    const promise = runPkgInstall(
      "192.168.1.50",
      "/user/data/x.pkg",
      "CID",
      null,
      true,
    );
    await vi.advanceTimersByTimeAsync(2600 * 4);
    const r = await promise;
    expect(r.installed).toBe(true);
  });

  it("treats the engine's accepted_unverified terminal state as a warning", async () => {
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") return START_OK;
      if (cmd === "pkg_install_status") {
        return {
          phase: "done",
          accepted_unverified: true,
          installed_bytes: 1_000_000,
          total: 25_000_000_000,
        };
      }
      return {};
    });
    const promise = runPkgInstall(
      "192.168.1.50",
      "/user/data/x.pkg",
      "CID",
      null,
      true,
    );
    await vi.advanceTimersByTimeAsync(2600);
    const r = await promise;
    expect(r.installed).toBe(false);
    expect(r.acceptedUnverified).toBe(true);
    expect(r.errMessage).toBe(PKG_ACCEPTED_UNVERIFIED_HINT);
    expect(useTaskStore.getState().tasks[0]).toMatchObject({
      kind: "pkg-install",
      status: "failed",
      lastError: { code: "INSTALL_UNVERIFIED", recoverable: true },
    });
  });
});

// ── install-from-USB: copy USB→internal, then install ───────────────────────
//
// We do NOT install directly from the USB path: handing Sony's installer a
// /mnt/usb… package registers it as a BGFT download task that streams the pkg
// off USB at a crawl (a 25 GB game showed "Downloading… 50 hours" + a broken
// tile — Bloodborne, 3.3.4). So we always copy to internal first, then install
// from there. This pins that copy-then-install path.
describe("installExternal — copies USB→internal, then installs", () => {
  const mockedInvoke = vi.mocked(invoke);
  const mockedCopy = vi.mocked(fsCopy);
  const mockedList = vi.mocked(fsListDir);
  const mockedDeleteUsb = vi.mocked(fsDelete);
  const USBHOST = "192.168.9.9";
  const usbPkg = {
    path: "/mnt/usb0/Bloodborne.pkg",
    drive: "/mnt/usb0",
    name: "Bloodborne",
    size: 25_000_000_000,
    contentId: "UP9000-CUSA00900_00-BLOODBORNE000000",
    titleId: "CUSA00900",
    platform: "ps4",
  };

  beforeEach(() => {
    vi.useFakeTimers();
    mockedInvoke.mockReset();
    mockedCopy.mockClear();
    mockedDeleteUsb.mockClear();
    mockedList.mockReset();
    mockedList.mockResolvedValue([]);
  });
  afterEach(() => {
    vi.useRealTimers();
    evictPkgLibraryStore(USBHOST);
  });

  it("copies USB→internal and installs from there (never installs off /mnt/usb)", async () => {
    // The install must run against the INTERNAL staging path, never the USB
    // path — installing off /mnt/usb is what registers the broken download tile.
    const startPaths: string[] = [];
    mockedInvoke.mockImplementation(async (cmd: unknown, args: unknown) => {
      if (cmd === "pkg_install_start") {
        startPaths.push(
          (args as { localPs5Path?: string })?.localPs5Path ?? "",
        );
        return { err_code: 0, register_path: "shellui-rpc", session_id: "s1" };
      }
      if (cmd === "pkg_install_status")
        return {
          phase: "done",
          launchable: true,
          installed_bytes: 1,
          total: 1,
        };
      return {};
    });
    const p = pkgLibraryStore(USBHOST)
      .getState()
      .installExternal(usbPkg, USBHOST);
    await vi.advanceTimersByTimeAsync(2600 * 2);
    const r = await p;
    expect(r.ok).toBe(true);
    // The copy ran, USB → internal pkg_temp.
    expect(mockedCopy).toHaveBeenCalledTimes(1);
    expect(mockedCopy).toHaveBeenCalledWith(
      expect.any(String),
      usbPkg.path,
      expect.stringContaining("/pkg_temp/"),
      expect.any(Number), // trackable op_id for the drop-tolerant copy
    );
    // The install targeted the internal copy, NOT the /mnt/usb path.
    expect(startPaths.every((p) => p.includes("/pkg_temp/"))).toBe(true);
    expect(startPaths.some((p) => p.startsWith("/mnt/usb"))).toBe(false);
  });

  it("keeps the internal copy when install acceptance is unverified", async () => {
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_install_start") {
        return { err_code: 0, register_path: "shellui-rpc", session_id: "s1" };
      }
      if (cmd === "pkg_install_status") {
        return {
          phase: "done",
          accepted_unverified: true,
          installed_bytes: 1_000_000,
          total: 25_000_000_000,
        };
      }
      return {};
    });
    const p = pkgLibraryStore(USBHOST)
      .getState()
      .installExternal(usbPkg, USBHOST);
    await vi.advanceTimersByTimeAsync(2600 * 2);
    const r = await p;
    expect(r.ok).toBe(false);
    expect(r.acceptedUnverified).toBe(true);
    expect(r.message).toMatch(/staging was kept/i);
    expect(mockedDeleteUsb).not.toHaveBeenCalled();
  });
});

describe("installedLastResult", () => {
  it("plain green success when launchable", () => {
    expect(installedLastResult(false)).toEqual({
      ok: true,
      message: "Installed package verified on the console.",
    });
  });
  it("amber warn with re-install guidance when may not launch", () => {
    const r = installedLastResult(true);
    expect(r.ok).toBe(true);
    expect(r.warn).toBe(true);
    expect(r.message).toBe(PKG_MAY_NOT_LAUNCH_MESSAGE);
    expect(r.message).toMatch(/Package Installer/);
  });
});

// ── Library cleanup (clearFinished / clearAll / isFinishedPkg) ──────────────

const mockedDelete = vi.mocked(fsDelete);
const HOST = "192.168.1.50";
const DIR = "/user/data/ps5upload/pkg_library";

function entry(p: Partial<PkgEntry> & { name: string }): PkgEntry {
  return {
    ...p,
    name: p.name,
    path: p.path ?? `${DIR}/${p.name}`,
    size: p.size ?? 1000,
    contentId: p.contentId ?? p.name.replace(/\.pkg$/, ""),
    status: p.status ?? "idle",
    title: p.title,
    titleId: p.titleId,
    bytes: p.bytes,
    totalBytes: p.totalBytes,
    lastResult: p.lastResult,
  };
}

function seed(entries: PkgEntry[]) {
  pkgLibraryStore(HOST).setState({ entries, error: null });
}

describe("isFinishedPkg", () => {
  it("is true only for idle rows whose last install succeeded", () => {
    expect(
      isFinishedPkg(
        entry({ name: "a.pkg", lastResult: { ok: true, message: "" } }),
      ),
    ).toBe(true);
    expect(
      isFinishedPkg(
        entry({ name: "b.pkg", lastResult: { ok: false, message: "x" } }),
      ),
    ).toBe(false);
    expect(isFinishedPkg(entry({ name: "c.pkg" }))).toBe(false);
    expect(
      isFinishedPkg(
        entry({
          name: "d.pkg",
          status: "installing",
          lastResult: { ok: true, message: "" },
        }),
      ),
    ).toBe(false);
  });
});

describe("clearFinished", () => {
  beforeEach(() => {
    mockedDelete.mockReset().mockResolvedValue(undefined);
  });
  afterEach(() => {
    pkgLibraryStore(HOST).setState({ entries: [], error: null });
  });

  it("deletes only the successfully-installed rows from the PS5", async () => {
    seed([
      entry({
        name: "done1.pkg",
        lastResult: { ok: true, message: "Installed." },
      }),
      entry({ name: "pending.pkg" }),
      entry({ name: "failed.pkg", lastResult: { ok: false, message: "err" } }),
      entry({ name: "uploading.pkg", status: "uploading" }),
      entry({
        name: "done2.pkg",
        lastResult: { ok: true, message: "Installed." },
      }),
    ]);

    await pkgLibraryStore(HOST).getState().clearFinished(HOST);

    const names = pkgLibraryStore(HOST)
      .getState()
      .entries.map((e) => e.name)
      .sort();
    expect(names).toEqual(["failed.pkg", "pending.pkg", "uploading.pkg"]);
    const deleted = mockedDelete.mock.calls.map((c) => c[1]).sort();
    expect(deleted).toEqual([`${DIR}/done1.pkg`, `${DIR}/done2.pkg`]);
  });

  it("is a no-op (no deletes) when nothing is finished", async () => {
    seed([entry({ name: "pending.pkg" })]);
    await pkgLibraryStore(HOST).getState().clearFinished(HOST);
    expect(mockedDelete).not.toHaveBeenCalled();
    expect(pkgLibraryStore(HOST).getState().entries).toHaveLength(1);
  });

  it("restores rows whose PS5 delete failed and surfaces an error", async () => {
    mockedDelete.mockImplementation(async (_addr: string, path: string) => {
      if (path.endsWith("done2.pkg")) throw new Error("EACCES");
    });
    seed([
      entry({
        name: "done1.pkg",
        lastResult: { ok: true, message: "Installed." },
      }),
      entry({
        name: "done2.pkg",
        lastResult: { ok: true, message: "Installed." },
      }),
    ]);

    await pkgLibraryStore(HOST).getState().clearFinished(HOST);

    const names = pkgLibraryStore(HOST)
      .getState()
      .entries.map((e) => e.name);
    expect(names).toEqual(["done2.pkg"]);
    expect(pkgLibraryStore(HOST).getState().error).toContain(
      "Failed to delete 1",
    );
  });

  it("does nothing without a host", async () => {
    seed([entry({ name: "done1.pkg", lastResult: { ok: true, message: "" } })]);
    await pkgLibraryStore(HOST).getState().clearFinished("  ");
    expect(mockedDelete).not.toHaveBeenCalled();
    expect(pkgLibraryStore(HOST).getState().entries).toHaveLength(1);
  });
});

describe("clearAll", () => {
  beforeEach(() => {
    mockedDelete.mockReset().mockResolvedValue(undefined);
  });
  afterEach(() => {
    pkgLibraryStore(HOST).setState({ entries: [], error: null });
  });

  it("deletes every idle row but never an in-flight one", async () => {
    seed([
      entry({ name: "idle1.pkg" }),
      entry({ name: "idle2.pkg", lastResult: { ok: true, message: "" } }),
      entry({ name: "uploading.pkg", status: "uploading" }),
      entry({ name: "installing.pkg", status: "installing" }),
      entry({ name: "queued.pkg", status: "queued" }),
    ]);

    await pkgLibraryStore(HOST).getState().clearAll(HOST);

    const names = pkgLibraryStore(HOST)
      .getState()
      .entries.map((e) => e.name)
      .sort();
    expect(names).toEqual(["installing.pkg", "queued.pkg", "uploading.pkg"]);
    expect(mockedDelete).toHaveBeenCalledTimes(2);
  });
});

// ── Base + update coexistence (the #3 bug fix) ──────────────────────────────

describe("refresh — base + update coexistence and badging", () => {
  const mockedList = vi.mocked(fsListDir);
  const CID = "EP9000-CUSA00207_00-BLOODBORNE000000";
  const file = (name: string, size: number, mtime = 0) =>
    ({ name, kind: "file", size, mtime }) as Awaited<
      ReturnType<typeof fsListDir>
    >[number];
  const dir = (name: string) =>
    ({ name, kind: "dir", size: 0 }) as Awaited<
      ReturnType<typeof fsListDir>
    >[number];

  beforeEach(() => {
    mockedList.mockReset();
    pkgLibraryStore(HOST).setState({
      entries: [],
      error: null,
      loading: false,
    });
  });
  afterEach(() => {
    pkgLibraryStore(HOST).setState({ entries: [], error: null });
  });

  it("lists a base and its same-ContentID update as two distinct, badged rows", async () => {
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d.endsWith("/updates")) return [file(`${CID}.pkg`, 200)];
      if (d.endsWith("/dlc")) return [];
      // library root: the base + the two sub-dirs (which we must NOT treat
      // as packages).
      return [file(`${CID}.pkg`, 100), dir("updates"), dir("dlc")];
    });

    await pkgLibraryStore(HOST).getState().refresh("192.168.1.50");

    const entries = pkgLibraryStore(HOST).getState().entries;
    expect(entries).toHaveLength(2);
    const base = entries.find((e) => e.category === undefined);
    const update = entries.find((e) => e.category === "gp");
    // Same ContentID...
    expect(base?.contentId).toBe(CID);
    expect(update?.contentId).toBe(CID);
    // ...but DIFFERENT paths — neither overwrites the other (the bug fix).
    expect(base?.path).not.toBe(update?.path);
    expect(base?.path.endsWith(`/${CID}.pkg`)).toBe(true);
    expect(update?.path).toContain("/updates/");
    expect(pkgLibraryStore(HOST).getState().error).toBeNull();
  });

  it("uses the staged file mtime as upload-time metadata for legacy rows", async () => {
    const stagedAt = 1_725_000_000;
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d === DIR) return [file(`${CID}.pkg`, 100, stagedAt)];
      return [];
    });

    await pkgLibraryStore(HOST).getState().refresh(HOST);

    expect(pkgLibraryStore(HOST).getState().entries[0].uploadedAt).toBe(
      stagedAt * 1000,
    );
  });

  it("keeps filename, source path, and upload time scoped to each console", async () => {
    const hostA = "192.168.7.31";
    const hostB = "192.168.7.32";
    const stagedPath = `${DIR}/${CID}.pkg`;
    const storage = new globalThis.Map<string, string>();
    storage.set(
      "ps5upload.pkg_library.pathmeta.v1",
      JSON.stringify({
        [`${hostA}\u0000${stagedPath}`]: {
          name: "optional-fix.pkg",
          sourcePath: "/downloads/optional-fix.pkg",
          uploadedAt: 1_725_000_000_000,
        },
        [`${hostB}\u0000${stagedPath}`]: {
          name: "backport.pkg",
          sourcePath: "/archive/backport.pkg",
          uploadedAt: 1_726_000_000_000,
        },
      }),
    );
    (globalThis as { window?: unknown }).window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) =>
          void storage.set(key, String(value)),
        removeItem: (key: string) => void storage.delete(key),
        clear: () => storage.clear(),
      },
    };
    mockedList.mockImplementation(async (_addr: string, d: string) =>
      d === DIR ? [file(`${CID}.pkg`, 100)] : [],
    );

    try {
      await pkgLibraryStore(hostA).getState().refresh(hostA);
      await pkgLibraryStore(hostB).getState().refresh(hostB);
      expect(pkgLibraryStore(hostA).getState().entries[0]).toMatchObject({
        originalName: "optional-fix.pkg",
        sourcePath: "/downloads/optional-fix.pkg",
        uploadedAt: 1_725_000_000_000,
      });
      expect(pkgLibraryStore(hostB).getState().entries[0]).toMatchObject({
        originalName: "backport.pkg",
        sourcePath: "/archive/backport.pkg",
        uploadedAt: 1_726_000_000_000,
      });
    } finally {
      evictPkgLibraryStore(hostA);
      evictPkgLibraryStore(hostB);
      delete (globalThis as { window?: unknown }).window;
    }
  });

  it("preserves legacy path-only metadata when creating a console-scoped row", async () => {
    const legacyHost = "192.168.7.33";
    const stagedPath = `${DIR}/${CID}.pkg`;
    const storage = new globalThis.Map<string, string>();
    storage.set(
      "ps5upload.pkg_library.pathmeta.v1",
      JSON.stringify({
        [stagedPath]: {
          name: "legacy-upload.pkg",
          sourcePath: "/downloads/legacy-upload.pkg",
          uploadedAt: 1_724_000_000_000,
          appVer: "01.09",
        },
      }),
    );
    (globalThis as { window?: unknown }).window = {
      localStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) =>
          void storage.set(key, String(value)),
        removeItem: (key: string) => void storage.delete(key),
        clear: () => storage.clear(),
      },
    };
    mockedList.mockImplementation(async (_addr: string, d: string) =>
      d === DIR ? [file(`${CID}.pkg`, 100)] : [],
    );
    vi.mocked(pkgMetadataConsole).mockResolvedValueOnce({
      contentId: CID,
      title: "Bloodborne",
      titleId: "CUSA00207",
      category: "gd",
      appVer: "01.09",
      platform: "ps4",
      fingerprint: "f".repeat(64),
    });

    try {
      await pkgLibraryStore(legacyHost).getState().refresh(legacyHost);
      await new Promise((resolve) => setTimeout(resolve, 0));

      const cached = JSON.parse(
        storage.get("ps5upload.pkg_library.pathmeta.v1") || "{}",
      );
      expect(cached[`${legacyHost}\u0000${stagedPath}`]).toMatchObject({
        name: "legacy-upload.pkg",
        sourcePath: "/downloads/legacy-upload.pkg",
        uploadedAt: 1_724_000_000_000,
        appVer: "01.09",
        fingerprint: "f".repeat(64),
      });
    } finally {
      evictPkgLibraryStore(legacyHost);
      delete (globalThis as { window?: unknown }).window;
    }
  });

  it("lists multiple same-ContentID variants from fingerprint directories", async () => {
    const optionalFingerprint = "a".repeat(64);
    const backportFingerprint = "b".repeat(64);
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d === DIR) return [dir("updates"), dir("dlc")];
      if (d === `${DIR}/updates`) {
        return [dir(optionalFingerprint), dir(backportFingerprint)];
      }
      if (d === `${DIR}/updates/${optionalFingerprint}`) {
        return [file(`${CID}.pkg`, 8_192_000)];
      }
      if (d === `${DIR}/updates/${backportFingerprint}`) {
        return [file(`${CID}.pkg`, 15_335_424)];
      }
      return [];
    });

    await pkgLibraryStore(HOST).getState().refresh(HOST);

    const updates = pkgLibraryStore(HOST)
      .getState()
      .entries.filter((e) => e.category === "gp");
    expect(updates).toHaveLength(2);
    expect(updates.map((e) => e.fingerprint).sort()).toEqual([
      optionalFingerprint,
      backportFingerprint,
    ]);
    expect(new Set(updates.map((e) => e.path)).size).toBe(2);
    expect(updates.every((e) => e.contentId === CID)).toBe(true);
  });

  it("recognises current 32-hex variant directories after a cold refresh", async () => {
    const token = "c".repeat(32);
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d === DIR) return [dir("updates")];
      if (d === `${DIR}/updates`) return [dir(token)];
      if (d === `${DIR}/updates/${token}`) {
        return [file(`${CID}.pkg`, 15_335_424)];
      }
      return [];
    });

    await pkgLibraryStore(HOST).getState().refresh(HOST);

    const update = pkgLibraryStore(HOST).getState().entries[0];
    expect(update.category).toBe("gp");
    expect(update.fingerprint).toBe(token);
    expect(update.path).toContain(`/updates/${token}/`);
  });

  it("tolerates missing updates/ + dlc/ sub-dirs (ENOENT), still lists the base", async () => {
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d.endsWith("/updates") || d.endsWith("/dlc")) {
        throw new Error("fs_list_dir_opendir_errno_2");
      }
      return [file(`${CID}.pkg`, 100)];
    });

    await pkgLibraryStore(HOST).getState().refresh("192.168.1.50");

    const entries = pkgLibraryStore(HOST).getState().entries;
    expect(entries).toHaveLength(1);
    expect(entries[0].category).toBeUndefined();
    expect(pkgLibraryStore(HOST).getState().error).toBeNull();
  });

  it("surfaces a real (non-ENOENT) error on the ROOT list without wiping the list", async () => {
    pkgLibraryStore(HOST).setState({
      entries: [
        {
          name: "x.pkg",
          path: "/lib/x.pkg",
          size: 1,
          contentId: "x",
          status: "idle",
        },
      ],
    });
    mockedList.mockImplementation(async (_addr: string, _d: string) => {
      throw new Error("connection refused");
    });

    await pkgLibraryStore(HOST).getState().refresh("192.168.1.50");

    // existing list preserved, error surfaced
    expect(pkgLibraryStore(HOST).getState().entries).toHaveLength(1);
    expect(pkgLibraryStore(HOST).getState().error).toBeTruthy();
  });

  it("enriches a staged row's version + category by reading the pkg off the console", async () => {
    // A unique host so the module-level enrich dedupe can't collide with other
    // tests. (No localStorage in this node env, so the path cache is always
    // empty here — nothing short-circuits the enrichment.)
    const ENRICH_HOST = "192.168.7.7";
    vi.mocked(pkgMetadataConsole).mockResolvedValueOnce({
      contentId: CID,
      title: "Bloodborne",
      titleId: "CUSA00207",
      category: "gp",
      appVer: "01.09",
      platform: "ps4",
      fingerprint: "f".repeat(64),
    });
    mockedList.mockImplementation(async (_addr: string, d: string) => {
      if (d.endsWith("/updates") || d.endsWith("/dlc")) return [];
      return [file(`${CID}.pkg`, 100)];
    });

    await pkgLibraryStore(ENRICH_HOST).getState().refresh(ENRICH_HOST);
    // Enrichment is fire-and-forget after refresh; let its microtasks flush.
    await new Promise((r) => setTimeout(r, 0));

    const e = pkgLibraryStore(ENRICH_HOST).getState().entries[0];
    expect(e.appVer).toBe("01.09");
    expect(e.fingerprint).toBe("f".repeat(64));
    expect(e.category).toBe("gp");
    expect(vi.mocked(pkgMetadataConsole)).toHaveBeenCalled();
  });
});
