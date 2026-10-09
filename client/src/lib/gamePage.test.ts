import { describe, expect, it } from "vitest";

import type { CollectionLocation, CollectionOffer } from "../api/collection";
import type { ConsoleEntry, GameView } from "../api/games";
import { ageLabel, driveOffers, hasTitleId, offersAfterReread, rosterOnly, needsReread, queueLinkFor, REREAD_MS, rowActions, summaryLine, titleIdFromContentId } from "./gamePage";

const tr = (_k: string, vars?: Record<string, string | number>, fallback?: string) =>
  (fallback ?? "").replace(/\{(\w+)\}/g, (_, k) => String(vars?.[k] ?? ""));

const offer = (category: string, path: string, version = ""): CollectionOffer => ({
  path,
  name: path,
  version,
  content_id: "",
  title: "",
  size_bytes: 1,
  category,
});
const entry = (over: Partial<ConsoleEntry> = {}): ConsoleEntry => ({
  host: "192.168.1.10",
  read_at: 1_000,
  installed: false,
  dlc_missing: [],
  ...over,
});
const folder = { type: "folder", absolute_path: "/g/Astro", pkg: undefined } as unknown as CollectionLocation;

describe("needsReread", () => {
  it("re-reads only an entry older than ten minutes", () => {
    const now = 1_000_000_000;
    expect(needsReread(Math.floor((now - REREAD_MS + 1000) / 1000), now)).toBe(false);
    expect(needsReread(Math.floor((now - REREAD_MS - 1000) / 1000), now)).toBe(true);
  });
});

describe("ageLabel", () => {
  const now = 10_000_000;
  it("says now for the connected console", () => {
    expect(ageLabel(9_000, now, true, tr)).toBe("now");
  });
  it("says how old a saved read is", () => {
    expect(ageLabel(now / 1000 - 30, now, false, tr)).toBe("as of just now");
    expect(ageLabel(now / 1000 - 5 * 60, now, false, tr)).toBe("as of 5 min ago");
    expect(ageLabel(now / 1000 - 2 * 3600, now, false, tr)).toBe("as of 2 h ago");
    expect(ageLabel(now / 1000 - 3 * 86400, now, false, tr)).toBe("as of 3 days ago");
  });
  it("says when a console never read the game", () => {
    expect(ageLabel(null, now, false, tr)).toBe("Not checked yet");
  });
});

describe("rowActions", () => {
  it("installs the base with its update and DLC on a console without the game", () => {
    const e = entry({ base: offer("gd", "/b"), update: offer("gp", "/u"), dlc_missing: [offer("ac", "/d")] });
    expect(rowActions(e, [], false)).toEqual([{ kind: "install", offers: [e.base, e.update, ...e.dlc_missing] }]);
  });
  it("offers the newer update and missing DLC on a console that has it", () => {
    const e = entry({ installed: true, update: offer("gp", "/u", "01.004"), dlc_missing: [offer("ac", "/d")] });
    expect(rowActions(e, [], false)).toEqual([
      { kind: "update", offer: e.update },
      { kind: "dlc", offers: e.dlc_missing },
    ]);
  });
  it("plays first on the connected console", () => {
    expect(rowActions(entry({ installed: true }), [], true)[0]).toEqual({ kind: "play" });
    expect(rowActions(entry({ installed: true }), [], false)).toEqual([]);
  });
  it("sends a folder, image or archive when there is no package", () => {
    expect(rowActions(entry(), [folder], false)).toEqual([{ kind: "send" }]);
  });
  it("sends from the drives to a console that never read the game", () => {
    expect(rowActions(undefined, [folder], false)).toEqual([{ kind: "send" }]);
  });
});

describe("summaryLine", () => {
  it("says where the game is", () => {
    const view: GameView = {
      title_id: "PPSA01234",
      title: "Astro",
      platform: "PS5",
      cover: null,
      copies: [folder, folder],
      consoles: [entry({ host: "1.1.1.1", installed: true, version: "01.004" }), entry({ host: "2.2.2.2" })],
    };
    expect(summaryLine(view, { "1.1.1.1": "Pro", "2.2.2.2": "Phat" }, tr)).toBe(
      "Installed on Pro (01.004) · not on Phat · 2 copies on your drives",
    );
  });
});

describe("ids", () => {
  it("knows a title ID", () => {
    expect(hasTitleId("PPSA01234")).toBe(true);
    expect(hasTitleId("CUSA00900")).toBe(true);
    expect(hasTitleId("MY-FOLDER")).toBe(false);
  });
  it("reads the title ID out of a content ID", () => {
    expect(titleIdFromContentId("UP0000-PPSA01234_00-ASTRO0000000000")).toBe("PPSA01234");
    expect(titleIdFromContentId("")).toBeNull();
    expect(titleIdFromContentId("garbage")).toBeNull();
  });
});

describe("queueLinkFor", () => {
  it("opens the screen that shows the job on the connected console", () => {
    expect(queueLinkFor("1.1.1.1", "1.1.1.1", true)).toBe("/install-package");
    expect(queueLinkFor("1.1.1.1", "1.1.1.1:9114", false)).toBe("/upload");
  });
  it("opens that console's activity for another console", () => {
    expect(queueLinkFor("2.2.2.2", "1.1.1.1", true)).toBe("/activity?console=2.2.2.2");
  });
});

describe("driveOffers", () => {
  const view: GameView = {
    title_id: "PPSA01234",
    title: "Astro",
    platform: "PS5",
    cover: null,
    copies: [],
    consoles: [
      entry({ host: "1.1.1.1", installed: true, update: offer("gp", "/u", "01.004"), dlc_missing: [offer("ac", "/d")] }),
    ],
  };
  it("lists the newer update, or the missing DLC, the drives have for this console", () => {
    expect(driveOffers(view, "1.1.1.1", "updates").map((o) => o.path)).toEqual(["/u"]);
    expect(driveOffers(view, "1.1.1.1", "addons").map((o) => o.path)).toEqual(["/d"]);
  });
  it("lists nothing for a console the engine has not read", () => {
    expect(driveOffers(view, "2.2.2.2", "updates")).toEqual([]);
    expect(driveOffers(null, "1.1.1.1", "updates")).toEqual([]);
  });
});

describe("offersAfterReread", () => {
  it("installs what the console still lacks after reading it again", () => {
    const fresh = entry({ installed: true, update: offer("gp", "/u2", "01.006"), dlc_missing: [] });
    expect(offersAfterReread("update", fresh, []).map((o) => o.path)).toEqual(["/u2"]);
  });
  it("installs nothing when the console has it by now", () => {
    expect(offersAfterReread("update", entry({ installed: true }), [])).toEqual([]);
    expect(offersAfterReread("dlc", entry({ installed: true, dlc_missing: [] }), [])).toEqual([]);
  });
  it("installs the base only while the game is still missing", () => {
    const missing = entry({ base: offer("gd", "/b"), dlc_missing: [] });
    expect(offersAfterReread("install", missing, []).map((o) => o.path)).toEqual(["/b"]);
    expect(offersAfterReread("install", entry({ installed: true }), [])).toEqual([]);
  });
});

describe("rosterOnly", () => {
  it("drops consoles the app no longer has", () => {
    const view: GameView = {
      title_id: "PPSA01234",
      title: "Astro",
      platform: "PS5",
      cover: null,
      copies: [],
      consoles: [entry({ host: "1.1.1.1" }), entry({ host: "9.9.9.9" })],
    };
    expect(rosterOnly(view, ["1.1.1.1"]).consoles.map((c) => c.host)).toEqual(["1.1.1.1"]);
  });
});
