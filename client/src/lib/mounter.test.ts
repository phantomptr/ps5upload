import { describe, expect, it } from "vitest";

import type { KrakenStatus, SmpStatus } from "../api/ps5";
import {
  driveOf,
  krakenGameFor,
  krakenIsTheMounter,
  krakenRefusal,
  krakenRuns,
  krakenScanRoot,
  krakenTitleIds,
  mounterOf,
  mountFileName,
} from "./mounter";

function kraken(over: Partial<KrakenStatus> = {}): KrakenStatus {
  return {
    installed: true,
    running: true,
    api: true,
    version: "0.2.0",
    waiting: null,
    games: [],
    title_ids: [],
    ampr: null,
    settings: { inject_ampr: true, ampr_auto_update: true, scan_paths: [] },
    scan_roots: [],
    errors: [],
    ...over,
  };
}

function snap(over: Partial<SmpStatus> = {}): SmpStatus {
  return {
    installed: false,
    running: false,
    config_ini: null,
    autotune_ini: null,
    debug_log_tail: null,
    mounted_images: [],
    errors: [],
    ...over,
  };
}

describe("which mounter runs", () => {
  it("reads the engine's answer, and an older engine's ShadowMount+ state", () => {
    expect(mounterOf(null)).toBeNull();
    expect(mounterOf(snap({ mounter: "kraken" }))).toBe("kraken");
    expect(mounterOf(snap({ running: true }))).toBe("shadowmount");
    expect(mounterOf(snap())).toBe("none");
  });

  it("offers the mount flows only when Kraken Mount runs alone", () => {
    expect(krakenIsTheMounter(snap({ mounter: "kraken" }))).toBe(true);
    expect(krakenIsTheMounter(snap({ mounter: "both" }))).toBe(false);
    expect(krakenRuns(snap({ mounter: "both" }))).toBe(true);
    expect(krakenRuns(snap({ mounter: "shadowmount" }))).toBe(false);
    expect(krakenRuns(snap({ mounter: "unknown" }))).toBe(false);
  });
});

describe("Kraken Mount's titles", () => {
  const s = snap({
    mounter: "kraken",
    kraken: kraken({
      title_ids: ["PPSA01234"],
      games: [
        { title_id: "PPSA01234", title_name: "A", path: "/mnt/usb0/A.pkg", kind: "pkg", status: "duplicate" },
        { title_id: "PPSA01234", title_name: "A", path: "/data/homebrew/A.pkg", kind: "pkg", status: "ready" },
      ],
    }),
  });

  it("knows a title by id, in any case", () => {
    expect(krakenTitleIds(s).has("PPSA01234")).toBe(true);
    expect(krakenTitleIds(snap()).size).toBe(0);
  });

  it("gives the copy it mounts, not a duplicate one", () => {
    expect(krakenGameFor(s, "ppsa01234")?.path).toBe("/data/homebrew/A.pkg");
    expect(krakenGameFor(s, "PPSA09999")).toBeNull();
  });
});

describe("where a package goes for Kraken Mount", () => {
  it("is the drive's homebrew folder by default", () => {
    expect(krakenScanRoot("/data", kraken())).toBe("/data/homebrew");
    expect(krakenScanRoot("/mnt/usb1/", kraken())).toBe("/mnt/usb1/homebrew");
    expect(krakenScanRoot("/mnt/ext0", null)).toBe("/mnt/ext0/homebrew");
  });

  it("is the user's own scan folder on that drive when scanpath replaces the list", () => {
    const own = kraken({
      settings: { inject_ampr: true, ampr_auto_update: true, scan_paths: ["/mnt/usb0/games", "/data/pkgs"] },
    });
    expect(krakenScanRoot("/data", own)).toBe("/data/pkgs");
    expect(krakenScanRoot("/mnt/usb1", own)).toBeNull();
    const ten = kraken({
      settings: { inject_ampr: true, ampr_auto_update: true, scan_paths: ["/mnt/usb10/x"] },
    });
    expect(krakenScanRoot("/mnt/usb1", ten)).toBeNull();
  });

  it("knows a path's drive", () => {
    expect(driveOf("/data/homebrew/x.pkg")).toBe("/data");
    expect(driveOf("/mnt/usb3/homebrew")).toBe("/mnt/usb3");
    expect(driveOf("/mnt/ext1")).toBe("/mnt/ext1");
    expect(driveOf("/user/app")).toBeNull();
    expect(driveOf("/mnt/usb")).toBeNull();
  });

  it("keeps the user's file name, cleaned, with .pkg", () => {
    expect(mountFileName("/Users/me/Game [v1.00].pkg", "UP0000-PPSA01234_00-X")).toBe("Game [v1.00].pkg");
    expect(mountFileName("C:\\pkgs\\a:b?.PKG", "C")).toBe("a_b_.pkg");
    expect(mountFileName("/x/.pkg", "UP0000-PPSA01234_00-X")).toBe("UP0000-PPSA01234_00-X.pkg");
  });
});

describe("what Kraken Mount refuses, from the header", () => {
  it("refuses PS4, retail and split packages", () => {
    expect(krakenRefusal({ platform: "ps5", authenticity: "fake_debug" })).toBeNull();
    expect(krakenRefusal({ platform: "", authenticity: "unknown" })).toBeNull();
    expect(krakenRefusal({ platform: "ps4" })).toBe("ps4");
    expect(krakenRefusal({ platform: "ps5", authenticity: "retail" })).toBe("retail");
    expect(krakenRefusal({ platform: "ps5", parts: 3 })).toBe("split");
  });
});

describe("Kraken Mount older than 0.2.1", () => {
  const k = (version: string | null) => ({ version }) as unknown as import("../api/ps5").KrakenStatus;
  it("is flagged with its version", async () => {
    const { krakenOutdated } = await import("./mounter");
    expect(krakenOutdated(k("0.2.0"))).toBe("0.2.0");
    expect(krakenOutdated(k("v0.1.9"))).toBe("0.1.9");
    expect(krakenOutdated(k("0.0.1-beta"))).toBe("0.0.1-beta");
  });
  it("is quiet for 0.2.1 and newer, or a version it can't read", async () => {
    const { krakenOutdated } = await import("./mounter");
    for (const v of ["0.2.1", "0.3.0", "1.0.0", null, "", "dev"]) expect(krakenOutdated(k(v))).toBeNull();
    expect(krakenOutdated(null)).toBeNull();
  });
});
