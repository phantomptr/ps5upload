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
  // Pre-install "do you already have this?" probe. `null` = unknown, which
  // must never block or relabel an install.
  pkgInstallPreflight: vi.fn(async () => null),
  // Unified install endpoint: start returns a job; status is polled to a
  // terminal phase. Individual install tests set these per-case.
  pkgInstall: vi.fn(async () => ({ ok: true, job: "job1" })),
  pkgInstallStatus: vi.fn(async () => ({ phase: "done", verdict: "installed" })),
}));
// No active transfer in tests → installs proceed immediately.
vi.mock("../lib/ps5Transfers", () => ({ transferScreenBusy: () => false }));

import { invoke } from "@tauri-apps/api/core";
import { useInstallSettingsStore } from "./installSettings";
import {
  fsDelete,
  fsListDir,
  fsCopy,
  pkgMetadataConsole,
  consoleReadiness,
  pkgInstall,
  pkgInstallStatus,
  type InstallStatus,
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
  describeInstallSample,
  runPkgInstall,
  waitForConsoleReady,
  recordPkgInstalled,
  isPkgInstalledHere,
  loadPkgAlternativeSelections,
  recordPkgAlternativeSelection,
  skipPkgAlternativeSelection,
  PKG_ALTERNATIVE_SKIP,
  PKG_MAY_NOT_LAUNCH_MESSAGE,
  PKG_ENGINE_BLIND_HINT,
  type PkgEntry,
} from "./pkgLibrary";
import { useTaskStore } from "./tasks";

describe("link install input", () => {
  it("rejects unsafe URLs before contacting the console", async () => {
    vi.mocked(invoke).mockClear();
    const host = "192.168.86.99";
    const store = pkgLibraryStore(host);
    for (const source of [
      "file:///tmp/game.pkg",
      "https://example.org/game.pkg#part",
      "https://example.org/game.pkg\n/next",
      "not a URL",
    ]) {
      const result = await store.getState().installUrl(source, host);
      expect(result.ok).toBe(false);
    }
    expect(invoke).not.toHaveBeenCalled();
    evictPkgLibraryStore(host);
  });
});

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



describe("describeInstallSample (stream state naming)", () => {
  const base = {
    installedBytes: 0,
    transferBytes: 0,
    total: 1000,
    servedRequests: 0,
    stalled: false,
    acceptedUnverified: false,
  };

  it("names each phase the engine can report", () => {
    expect(describeInstallSample({ ...base, phase: "queued" }).detail).toMatch(
      /Waiting for the PS5/,
    );
    expect(
      describeInstallSample({
        ...base,
        phase: "download",
        transferBytes: 500,
        servedRequests: 3,
      }).detail,
    ).toMatch(/Streaming to the PS5 — 50%/);
    expect(
      describeInstallSample({
        ...base,
        phase: "install",
        installedBytes: 1000,
      }).detail,
    ).toMatch(/The PS5 is installing/);
  });

  /* A link install has two legs that fail differently: a slow origin and a
   * slow console link look identical in one blended number, which is why a
   * real 1.9 MB/s report could not be diagnosed from the UI at all. */
  it("names both legs when the origin rate is known", () => {
    const { detail } = describeInstallSample(
      {
        phase: "download",
        installedBytes: 0,
        transferBytes: 500,
        total: 1000,
        servedRequests: 3,
        stalled: false,
        acceptedUnverified: false,
        originRateBps: 1_900_000,
      },
      1_800_000,
    );
    expect(detail).toContain("downloading");
    expect(detail).toContain("sending");
  });

  /* A staged install has no origin, so there is no second leg to name. */
  it("keeps the single-speed wording when there is no origin leg", () => {
    const { detail } = describeInstallSample(
      {
        phase: "download",
        installedBytes: 0,
        transferBytes: 500,
        total: 1000,
        servedRequests: 3,
        stalled: false,
        acceptedUnverified: false,
      },
      1_800_000,
    );
    expect(detail).not.toContain("downloading");
    expect(detail).toMatch(/ at /);
  });

  /* The install phase is the longest part of a large install and used to be
   * a bare percentage: no bytes, no rate, no estimate. Worse, the rate was
   * sampled from transferBytes, which stops moving exactly when this phase
   * begins, so "at X/s" read 0 for the whole of it. */
  it("reports bytes, rate and an estimate while the PS5 installs", () => {
    const { detail } = describeInstallSample(
      {
        phase: "install",
        installedBytes: 45_100_000_000,
        transferBytes: 0,
        total: 108_000_000_000,
        servedRequests: 9,
        stalled: false,
        acceptedUnverified: false,
      },
      96_000_000,
    );
    expect(detail).toContain("of");
    expect(detail).toContain("/s");
    expect(detail).toContain("left");
  });

  /* No rate yet means no honest estimate, so we show neither. */
  it("omits the estimate when there is no rate to base it on", () => {
    const { detail } = describeInstallSample(
      {
        phase: "install",
        installedBytes: 45_100_000_000,
        transferBytes: 0,
        total: 108_000_000_000,
        servedRequests: 9,
        stalled: false,
        acceptedUnverified: false,
      },
      0,
    );
    expect(detail).not.toContain("left");
    expect(detail).not.toContain("/s");
  });

  it("says why the numbers stopped moving when the engine is gone", () => {
    // The bytes below the line are the last ones we could read. Without the
    // note the UI showed a frozen bar with no explanation, which reads as
    // "stuck" — but the install may be running fine and simply unwatched.
    const s = describeInstallSample({
      ...base,
      phase: "install",
      installedBytes: 1000,
      note: PKG_ENGINE_BLIND_HINT,
    });
    expect(s.detail).toBe(PKG_ENGINE_BLIND_HINT);
    // The bar still points at the last real progress rather than resetting.
    expect(s.current).toBe(1000);
    expect(s.pct).toBe(100);
  });

  it("reports the transfer, not the (still zero) install bytes, while streaming", () => {
    // Measured on hardware: `installed_bytes` stays 0 for the whole 19 s of a
    // 1.35 GB transfer, so a bar driven by it would sit at 0% and then jump.
    const s = describeInstallSample({
      ...base,
      phase: "download",
      transferBytes: 250,
      servedRequests: 2,
    });
    expect(s.current).toBe(250);
    expect(s.pct).toBe(25);
  });

  it("never runs backwards across the download → install handover", () => {
    // The install phase reports bytes it wrote (for a PS5 FPKG, the *inner*
    // image, which is smaller than the container we served), so taking either
    // figure alone would drop the bar when the phase flips.
    const streaming = describeInstallSample({
      ...base,
      phase: "download",
      transferBytes: 900,
    });
    const writing = describeInstallSample({
      ...base,
      phase: "install",
      transferBytes: 1000,
      installedBytes: 40,
    });
    expect(writing.current).toBeGreaterThanOrEqual(streaming.current);
  });

  it("appends the rate only when it is known", () => {
    const s = { ...base, phase: "download", transferBytes: 100 };
    expect(describeInstallSample(s, 0).detail).not.toMatch(/\/s/);
    expect(describeInstallSample(s, 90 * 1024 * 1024).detail).toMatch(
      /at 90 MB\/s/,
    );
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
    expect(pkgTypeForCategory("gd", "ps5")).toBe("PS5GD");
    expect(pkgTypeForCategory("gp", "ps5")).toBe("PS5DP");
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

  it("persists an explicit skip instead of falling back to auto-selection", () => {
    const key = "patch:CUSA33334:01.02";
    recordPkgAlternativeSelection(A, key, "optional-fingerprint");
    skipPkgAlternativeSelection(A, key);
    expect(loadPkgAlternativeSelections(A)[key]).toBe(PKG_ALTERNATIVE_SKIP);
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


// ── Unified install: verdict mapping + Auto-Delete of the staged pkg ─────────
// runPkgInstall now posts one request to the engine and polls one status; the
// engine owns the guard/deliver/DPI/restore/verify. The client's remaining
// jobs: choose the console_path source, map the verdict, and — because the
// engine will NOT delete an on-console file — delete the staged copy itself
// after a CONFIRMED install (the Auto-Delete data-loss fix lives here now).

/** A terminal InstallStatus with sensible defaults; override per case. */
function installStatus(over: Partial<InstallStatus>): InstallStatus {
  return {
    job: "j1",
    ps5_addr: "192.168.1.50:9114",
    content_id: "CID",
    title_id: null,
    phase: "done",
    route: "loopback",
    verdict: "installed",
    code: 0,
    hint: null,
    reason: null,
    metrics: {
      total_bytes: 0,
      served_bytes: 0,
      throughput_mbps: 0,
      phase_ms: {},
      retries: 0,
      sony_rc: 0,
    },
    app_ver_before: null,
    app_ver_after: null,
    patch_verdict: null,
    shortened: false,
    started_at: 0,
    updated_at: 0,
    ...over,
  };
}

describe("runPkgInstall — unified endpoint: verdict + Auto-Delete staging", () => {
  const mockedInstall = vi.mocked(pkgInstall);
  const mockedStatus = vi.mocked(pkgInstallStatus);
  const mockedDelete = vi.mocked(fsDelete);
  const host = "192.168.1.50";
  const path = "/user/data/ps5upload/pkg_library/x.pkg";

  beforeEach(() => {
    useTaskStore.setState({ tasks: [] });
    mockedInstall.mockReset();
    mockedStatus.mockReset();
    mockedDelete.mockReset();
    mockedInstall.mockResolvedValue({ ok: true, job: "job1" });
  });

  it("deletes the staged pkg after a confirmed install when Auto-Delete is on", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "done", verdict: "installed" }),
    );
    const r = await runPkgInstall(host, path, "CID", "PS4GD", true);
    expect(r.installed).toBe(true);
    expect(mockedDelete).toHaveBeenCalledWith(
      expect.stringContaining("192.168.1.50"),
      path,
    );
  });

  it("KEEPS the staged pkg when Auto-Delete is off", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "done", verdict: "installed" }),
    );
    await runPkgInstall(host, path, "CID", "PS4GD", false);
    expect(mockedDelete).not.toHaveBeenCalled();
  });

  it("never deletes the pkg on a failed install, even with Auto-Delete on", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "failed", verdict: "failed", hint: "0x80B2116F" }),
    );
    const r = await runPkgInstall(host, path, "CID", "PS4GD", true);
    expect(r.installed).toBe(false);
    expect(mockedDelete).not.toHaveBeenCalled();
  });

  it("sends the console_path source with derived title/category/app_ver", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "done", verdict: "installed" }),
    );
    await runPkgInstall(
      host,
      path,
      "IV0000-CUSA07842_00-0000000000000001",
      "PS4DP",
      false,
      undefined,
      undefined,
      undefined,
      "01.09",
    );
    const req = mockedInstall.mock.calls[0][0];
    expect(req.source).toEqual({ console_path: path });
    expect(req.title_id).toBe("CUSA07842");
    expect(req.category).toBe("PS4DP");
    expect(req.package_app_ver).toBe("01.09");
    // The user explicitly chose this install; the guard must not block a
    // re-install (preserves the prior warn-not-block behaviour).
    expect(req.options?.allow_destructive_reinstall).toBe(true);
  });

  it("a may_not_launch verdict is a success WITH a caution (still deletable)", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "done", verdict: "may_not_launch" }),
    );
    const r = await runPkgInstall(host, path, "CID", "PS4GD", true);
    expect(r.installed).toBe(true);
    expect(r.mayNotLaunch).toBe(true);
    expect(mockedDelete).toHaveBeenCalled();
  });

  it("a regressed patch is reported failed, pkg kept", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({
        phase: "failed",
        verdict: "failed",
        patch_verdict: "regressed",
      }),
    );
    const r = await runPkgInstall(host, path, "CID", "PS4DP", true);
    expect(r.installed).toBe(false);
    expect(mockedDelete).not.toHaveBeenCalled();
  });

  it("a busy engine surfaces as a failure, pkg kept", async () => {
    // A refused start throws (the task row is marked failed by the catch), so
    // the outcome is a rejection — never a silent success — and nothing is
    // deleted.
    mockedInstall.mockResolvedValue({ ok: false, error: "busy", job: "other" });
    await expect(
      runPkgInstall(host, path, "CID", "PS4GD", true),
    ).rejects.toThrow(/already running/i);
    expect(mockedDelete).not.toHaveBeenCalled();
  });

  it("surfaces live progress from the status metrics", async () => {
    // The sample callback fires on every poll, including the terminal one, so a
    // single done status carrying metrics is enough to prove the wiring without
    // a real inter-poll sleep.
    mockedStatus.mockResolvedValue(
      installStatus({
        phase: "done",
        verdict: "installed",
        metrics: {
          total_bytes: 1000,
          served_bytes: 400,
          throughput_mbps: 10,
          phase_ms: {},
          retries: 0,
          sony_rc: 0,
        },
      }),
    );
    const samples: number[] = [];
    const r = await runPkgInstall(
      host,
      path,
      "CID",
      "PS4GD",
      false,
      (s) => samples.push(Math.max(s.transferBytes, s.installedBytes)),
    );
    expect(r.installed).toBe(true);
    expect(samples.some((n) => n === 400)).toBe(true);
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
  const mockedInstall = vi.mocked(pkgInstall);
  const mockedStatus = vi.mocked(pkgInstallStatus);
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
    mockedInstall.mockReset();
    mockedStatus.mockReset();
    mockedInstall.mockResolvedValue({ ok: true, job: "j1" });
    mockedCopy.mockClear();
    mockedDeleteUsb.mockClear();
    mockedList.mockReset();
    mockedList.mockResolvedValue([]);
  });
  afterEach(() => {
    evictPkgLibraryStore(USBHOST);
  });

  it("copies USB→internal and installs from there (never installs off /mnt/usb)", async () => {
    // The install must run against the INTERNAL staging path, never the USB
    // path — installing off /mnt/usb is what registers the broken download tile.
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "done", verdict: "installed" }),
    );
    const r = await pkgLibraryStore(USBHOST)
      .getState()
      .installExternal(usbPkg, USBHOST);
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
    const sources = mockedInstall.mock.calls.map(
      (c) => (c[0].source as { console_path?: string }).console_path ?? "",
    );
    expect(sources.length).toBeGreaterThan(0);
    expect(sources.every((p) => p.includes("/pkg_temp/"))).toBe(true);
    expect(sources.some((p) => p.startsWith("/mnt/usb"))).toBe(false);
  });

  it("keeps the internal copy when the install fails", async () => {
    // A failed verdict must not delete the internal staging copy — the original
    // USB file is untouched and the copy stays for a retry.
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "failed", verdict: "failed", hint: "rejected" }),
    );
    const r = await pkgLibraryStore(USBHOST)
      .getState()
      .installExternal(usbPkg, USBHOST);
    expect(r.ok).toBe(false);
    expect(r.message).toMatch(/staging was kept/i);
    // fsDelete is only reached on a confirmed install (and the dest-exists
    // copy retry, which didn't fire here).
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

describe("installAll as one activity row", () => {
  beforeEach(() => {
    useTaskStore.setState({ tasks: [] });
  });

  function stubInstall(outcome: (path: string) => boolean) {
    const store = pkgLibraryStore(HOST);
    store.setState({
      installing: false,
      installingAll: false,
      install: async (path: string) => {
        const ok = outcome(path);
        store.setState((s) => ({
          entries: s.entries.map((e) =>
            e.path === path ? { ...e, lastResult: { ok, message: ok ? "" : "no" } } : e,
          ),
        }));
      },
    } as never);
  }

  const batch = () => useTaskStore.getState().tasks.filter((t) => t.kind === "install-batch");

  it("registers one install-batch task that ends done with its count", async () => {
    seed([entry({ name: "A.pkg" }), entry({ name: "B.pkg" })]);
    stubInstall(() => true);
    await pkgLibraryStore(HOST).getState().installAll(HOST);
    expect(batch()).toHaveLength(1);
    expect(batch()[0]).toMatchObject({
      status: "done",
      label: "Install all (2)",
      progress: { total: 2 },
    });
  });

  it("ends failed and says how many did not install", async () => {
    seed([entry({ name: "A.pkg" }), entry({ name: "B.pkg" })]);
    stubInstall((path) => path.endsWith("A.pkg"));
    await pkgLibraryStore(HOST).getState().installAll(HOST);
    expect(batch()[0]).toMatchObject({
      status: "failed",
      lastError: { message: "1 of 2 failed" },
    });
  });
});

describe("uploadInstall (Convert's Upload & install)", () => {
  const host = "192.168.55.6";
  const localPath = "/out/UP0000-CUSA33334_00-TEST000000000000.pkg";
  const head = {
    parts: [localPath],
    total_size: 8_192,
    head: {
      content_id: "UP0000-CUSA33334_00-TEST000000000000",
      title: "Test",
      category: "gd",
      app_ver: "01.00",
    },
  };

  afterEach(() => {
    vi.mocked(invoke).mockReset();
    evictPkgLibraryStore(host);
  });

  it("reports the upload failure instead of claiming an install", async () => {
    vi.mocked(invoke).mockImplementation(async (command: unknown) => {
      if (command === "pkg_metadata_split") return head;
      if (command === "transfer_file") throw new Error("connection refused");
      return {};
    });
    const dests: string[] = [];
    const r = await pkgLibraryStore(host)
      .getState()
      .uploadInstall(localPath, host, { onDest: (d) => dests.push(d) });
    expect(r.ok).toBe(false);
    expect(r.message).toMatch(/connection refused/);
    expect(dests).toHaveLength(1);
    expect(vi.mocked(pkgInstall)).not.toHaveBeenCalled();
  });

  it("reports the install as done even when auto-remove drops the row", async () => {
    useInstallSettingsStore.setState({ autoRemoveAfterInstall: true });
    vi.mocked(pkgInstallStatus).mockResolvedValue({
      job: "job1",
      ps5_addr: host,
      content_id: head.head.content_id,
      title_id: "CUSA33334",
      phase: "done",
      route: "loopback",
      verdict: "installed",
      code: 0,
      hint: null,
      reason: null,
      metrics: {
        total_bytes: 8_192,
        served_bytes: 8_192,
        throughput_mbps: 0,
        phase_ms: {},
        retries: 0,
        sony_rc: 0,
      },
      app_ver_before: null,
      app_ver_after: null,
      patch_verdict: null,
      shortened: false,
      started_at: 0,
      updated_at: 0,
    } as InstallStatus);
    vi.mocked(fsListDir).mockImplementation(async () => [
      {
        name: "UP0000-CUSA33334_00-TEST000000000000.pkg",
        kind: "file",
        size: 8_192,
      },
    ] as never);
    vi.mocked(invoke).mockImplementation(async (command: unknown) => {
      if (command === "pkg_metadata_split") return head;
      if (command === "transfer_file") return { job_id: "t1" };
      if (command === "job_status")
        return { status: "done", bytes_sent: 8_192, total_bytes: 8_192 };
      return {};
    });
    let dest = "";
    const r = await pkgLibraryStore(host)
      .getState()
      .uploadInstall(localPath, host, { onDest: (d) => (dest = d) });
    expect(r).toEqual({ ok: true });
    expect(vi.mocked(pkgInstall)).toHaveBeenCalled();
    expect(
      pkgLibraryStore(host)
        .getState()
        .entries.some((e) => e.path === dest),
    ).toBe(false);
  }, 15_000);

  it("refuses while another install holds the lock, without uploading", async () => {
    pkgLibraryStore(host).setState({ installing: true });
    const r = await pkgLibraryStore(host)
      .getState()
      .uploadInstall(localPath, host);
    expect(r.ok).toBe(false);
    expect(
      vi.mocked(invoke).mock.calls.filter(([c]) => c === "transfer_file"),
    ).toHaveLength(0);
  });

  it("reports a header it cannot read", async () => {
    vi.mocked(invoke).mockImplementation(async (command: unknown) => {
      if (command === "pkg_metadata_split") throw new Error("bad magic");
      return {};
    });
    const r = await pkgLibraryStore(host)
      .getState()
      .uploadInstall(localPath, host);
    expect(r).toMatchObject({ ok: false });
    expect(r.message).toMatch(/bad magic/);
  });
});
