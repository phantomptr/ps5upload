import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { gamesViewOf } from "../games/GamesSection";
import { SECTIONS } from "../sections";
import { t } from "../strings";
import { LibraryView, groupSize, type LibraryViewProps } from "./Library";
import { baseTitleId, compareVersions, filterGroups, formatOf, groupEntries, nameKey, roleOf, titleFromFileName } from "./group";
import { copyDest, displayName, entriesOf, parseScan, stagePath, STAGE_DIR, type Entry, type LibItem, type LibScan } from "./model";

const SCAN_JSON = JSON.stringify({
  source: "3fa2c1d0",
  scanned: 1761200000,
  complete: true,
  stopped: null,
  entries: 40,
  items: [
    { kind: "pkg", path: "PS5/Astro.pkg", name: "Astro.pkg", title: "Astro Bot", title_id: "PPSA01650", content_id: "UP9000-PPSA01650_00-ASTRO", version: "01.000.016", category: "gd", type: "game", platform: "ps5", fw: "7.50", fake: true, size: 1024, icon: "3fa2c1d0-abc-0.png" },
    { kind: "folder", path: "PS4/CUSA07842", name: "CUSA07842", title: "Jak X", title_id: "CUSA07842", content_id: "", version: "01.02", category: "gd", type: "game", platform: "ps4", fw: "", fake: false, size: 0, icon: "" },
    { kind: "image", path: "img/PPSA09999 Venus.exfat", name: "PPSA09999 Venus.exfat", title: "", title_id: "PPSA09999", content_id: "", version: "", category: "", type: "game", platform: "ps5", fw: "", fake: false, size: 5, icon: "../../etc/x.png" },
    { kind: "nope", path: "x" },
    { kind: "pkg", path: "" },
  ],
});

describe("library model", () => {
  it("reads a cached scan field by field and drops what is not an item", () => {
    const s = parseScan(SCAN_JSON) as LibScan;
    expect(s.complete).toBe(true);
    expect(s.items.map((i) => i.kind)).toEqual(["pkg", "folder", "image"]);
    expect(s.items[0].fake).toBe(true);
    // a cover name is only ever a plain file in the icons folder
    expect(s.items[2].icon).toBe("");
    expect(parseScan("not json")).toBeNull();
    expect(parseScan("{}")).toBeNull();
  });

  it("names, joins and filters entries across sources", () => {
    const s = parseScan(SCAN_JSON) as LibScan;
    const list = entriesOf(
      [
        { id: "console", name: "This PS5" },
        { id: "3fa2c1d0", name: "NAS" },
      ],
      new Map([
        ["console", { scanned: 1, complete: true, stopped: null, items: [{ ...s.items[1], path: "/data/homebrew/Jak" }] }],
        ["3fa2c1d0", s],
      ]),
    );
    expect(list.map(displayName)).toEqual(["Astro Bot", "Jak X", "Jak X", "PPSA09999 Venus"]);
    expect(list.find((e) => e.source === "console")?.at).toBe("/data/homebrew/Jak");
    expect(list.find((e) => e.kind === "pkg")?.at).toBe("smb://3fa2c1d0/PS5/Astro.pkg");
  });

  it("copies into the drive's homebrew folder and stages packages in the helper's staging folder", () => {
    expect(copyDest("/mnt/usb0", { name: "CUSA07842", path: "PS4/CUSA07842" })).toBe("/mnt/usb0/homebrew/CUSA07842");
    expect(copyDest("/data", { name: "PPSA09999 Venus.exfat", path: "img/PPSA09999 Venus.exfat" })).toBe("/data/homebrew/PPSA09999 Venus.exfat");
    expect(stagePath(36 ** 3)).toBe(`${STAGE_DIR}/library-1000.pkg`);
    expect(STAGE_DIR).toBe("/user/data/ps5upload/pkg_temp");
  });
});

const item = (over: Partial<LibItem>): LibItem => ({
  kind: "pkg",
  path: "x.pkg",
  name: "x.pkg",
  title: "",
  title_id: "",
  content_id: "",
  version: "",
  type: "game",
  platform: "ps5",
  fw: "",
  sdk: "",
  backport: false,
  fake: false,
  size: 0,
  icon: "",
  ...over,
});

/** A NAS and this console holding one game five ways, plus another game named only by its file. */
const GAME_SCANS = new Map<string, LibScan>([
  [
    "nas",
    {
      scanned: 1,
      complete: true,
      stopped: null,
      items: [
        item({ path: "PS5/Astro.pkg", name: "Astro.pkg", title: "Astro Bot", title_id: "PPSA01650", content_id: "UP9000-PPSA01650_00-ASTRO", version: "01.000", fw: "7.50", size: 30e9, icon: "nas-1-0.png" }),
        item({ path: "PS5/Astro-patch.pkg", name: "Astro-patch.pkg", title: "Astro Bot", title_id: "PPSA01650", type: "patch", version: "01.020", fw: "8.00", size: 2e9 }),
        item({ path: "PS5/Astro-patch-old.pkg", name: "Astro-patch-old.pkg", title: "Astro Bot", title_id: "PPSA01650", type: "patch", version: "01.003", size: 1e9 }),
        item({ path: "PS5/Astro-dlc.pkg", name: "Astro-dlc.pkg", title: "Astro Bot Costume", title_id: "", content_id: "UP9000-PPSA01650_00-COSTUME", type: "dlc", size: 1e6, fw: "9.00" }),
        item({ kind: "folder", path: "Backports/Astro", name: "Astro", title: "Astro Bot", title_id: "PPSA01650", backport: true, sdk: "4.50" }),
        item({ kind: "image", path: "img/Venus Vacation v1.02.exfat", name: "Venus Vacation v1.02.exfat", type: "game", size: 5e9 }),
      ],
    },
  ],
  [
    "console",
    { scanned: 1, complete: true, stopped: null, items: [item({ kind: "folder", path: "/data/homebrew/PPSA01650", name: "PPSA01650", title: "Astro Bot", title_id: "PPSA01650" })] },
  ],
]);
const GAME_SOURCES = [
  { id: "console", name: "This PS5" },
  { id: "nas", name: "NAS" },
];

describe("library grouping (#526)", () => {
  const entries: Entry[] = entriesOf(GAME_SOURCES, GAME_SCANS);
  const groups = groupEntries(entries);

  it("shows one game once, however many copies, updates, DLC and backports sit next to it", () => {
    expect(groups.map((g) => g.title)).toEqual(["Astro Bot", "Venus Vacation v1.02"]);
    expect(titleFromFileName("007 First Light - PPSA11386 - v1.18.0 - US.exfat", "PPSA11386")).toBe("007 First Light");
    expect(titleFromFileName("PPSA09999 Venus.exfat", "PPSA09999")).toBe("Venus");
    expect(titleFromFileName("[CUSA12345] Game.ffpkg", "CUSA12345")).toBe("Game");
    expect(titleFromFileName("PPSA09999.exfat", "PPSA09999")).toBe("PPSA09999.exfat");
    const astro = groups[0];
    expect(astro.parts.length).toBe(6);
    expect(astro.titleId).toBe("PPSA01650");
    expect(astro.updates).toBe(2);
    expect(astro.dlc).toBe(1);
    expect(astro.backports).toBe(1);
    expect(astro.cover?.icon).toBe("nas-1-0.png");
    expect(astro.places).toEqual(["This PS5", "NAS"]);
  });

  it("orders the parts: base copies (this console's first), updates newest first, backports, DLC", () => {
    const roles = groups[0].parts.map((p) => `${p.role}:${p.entry.version || p.entry.source}`);
    expect(roles).toEqual(["base:console", "base:01.000", "update:01.020", "update:01.003", "backport:nas", "dlc:nas"]);
  });

  it("sizes a game by its largest base copy, and takes the highest firmware a non-DLC part needs", () => {
    const astro = groups[0];
    expect(astro.size).toBe(30e9);
    expect(astro.sizeUnknown).toBe(false);
    expect(astro.fw).toBe("8.00");
    expect(astro.sdk).toBe("4.50");
    const counted = new Map(astro.parts.filter((p) => p.entry.kind === "folder").map((p) => [p.entry.key, { bytes: 40e9, done: true }]));
    expect(groupSize(astro, counted)).toEqual({ bytes: 40e9, partial: false });
    // only folders: unknown until counted
    const onlyFolders = groupEntries(entries.filter((e) => e.kind === "folder"))[0];
    expect(onlyFolders.sizeUnknown).toBe(true);
    expect(groupSize(onlyFolders, counted)).toEqual({ bytes: 40e9, partial: false });
    // no base: the parts added up
    const dlcOnly = groupEntries(entries.filter((e) => e.type === "dlc" || e.type === "patch"))[0];
    expect(dlcOnly.size).toBe(3e9 + 1e6);
  });

  it("reads each part's role and format, and a base title id from a content id", () => {
    expect(roleOf({ type: "game", name: "Game BACKPORT", path: "", backport: false })).toBe("backport");
    expect(roleOf({ type: "patch", name: "x", path: "", backport: false })).toBe("update");
    expect(formatOf({ kind: "pkg", name: "a.pkg", fake: true })).toBe("fpkg");
    expect(formatOf({ kind: "image", name: "a.FFPKG", fake: false })).toBe("ffpkg");
    expect(formatOf({ kind: "image", name: "a.ffpfsc", fake: false })).toBe("ffpfs");
    expect(baseTitleId({ title_id: "", content_id: "EP0001-CUSA07842_00-JAKX" })).toBe("CUSA07842");
    expect(nameKey("Venus Vacation v1.02.exfat")).toBe(nameKey("venus-vacation.ffpkg"));
    expect(compareVersions("01.020", "01.003")).toBeGreaterThan(0);
    expect(compareVersions("13.60", "13.60")).toBe(0);
  });

  it("finds a game by title, title id or a part's file name", () => {
    expect(filterGroups(groups, "ppsa01650").length).toBe(1);
    expect(filterGroups(groups, "patch-old").length).toBe(1);
    expect(filterGroups(groups, "venus").map((g) => g.title)).toEqual(["Venus Vacation v1.02"]);
  });
});

const base = (over: Partial<LibraryViewProps> = {}): LibraryViewProps => ({
  sources: [
    { id: "console", name: "This PS5" },
    { id: "3fa2c1d0", name: "NAS" },
  ],
  scans: new Map([["3fa2c1d0", parseScan(SCAN_JSON)]]),
  loaded: true,
  query: "",
  task: null,
  message: null,
  open: null,
  drives: [],
  copied: null,
  installed: null,
  firmware: null,
  sizes: new Map(),
  onQuery: vi.fn(),
  onScan: vi.fn(),
  onStop: vi.fn(),
  onOpen: vi.fn(),
  onInstall: vi.fn(),
  onCopy: vi.fn(),
  onRegister: vi.fn(),
  onShowInFiles: null,
  ...over,
});
const html = (over: Partial<LibraryViewProps> = {}) => renderToStaticMarkup(<LibraryView {...base(over)} />);

describe("Library section", () => {
  it("is a view of Games, which opens on it to scan or when asked", () => {
    const ids = SECTIONS.map((s) => s.id);
    expect(ids).not.toContain("library");
    expect(SECTIONS.find((s) => s.id === "games")?.acceptsParams).toBe(true);
    expect(gamesViewOf({ scan: "1" })).toBe("library");
    expect(gamesViewOf({ view: "library" })).toBe("library");
    expect(gamesViewOf(null)).toBe("installed");
  });

  it("shows one card per game with its platform and format", () => {
    const h = html();
    expect(h).toContain("Astro Bot");
    expect(h).toContain(">Venus<");
    expect(h).toContain(t("cw_lib_kind_folder"));
    expect(h.match(/data-lib-key=/g)?.length).toBe(3);
  });

  it("collapses the same game found twice into one card", () => {
    const h = html({ sources: GAME_SOURCES, scans: GAME_SCANS });
    expect(h.match(/data-lib-key=/g)?.length).toBe(2);
    expect(h).toContain(t("cw_lib_badge_dlc", { n: 1 }));
    expect(h).toContain(t("cw_lib_role_backport"));
  });

  it("marks a game installed on this console", () => {
    const h = html({ sources: GAME_SOURCES, scans: GAME_SCANS, installed: new Map([["PPSA01650", { version: null }]]) });
    expect(h).toContain("cw-libcard-installed");
  });

  it("says when nothing was scanned, and when a scan stopped at its limit", () => {
    expect(html({ scans: new Map() })).toContain(t("cw_lib_never"));
    const partial = { ...(parseScan(SCAN_JSON) as LibScan), complete: false, stopped: "items" };
    expect(html({ scans: new Map([["3fa2c1d0", partial]]) })).toContain(t("cw_lib_partial", { name: "NAS" }));
  });

  it("opens a full-screen detail with every part, its size, firmware, SDK and install state", () => {
    const h = html({
      sources: GAME_SOURCES,
      scans: GAME_SCANS,
      open: "ps5:PPSA01650",
      firmware: "7.61",
      installed: new Map([["PPSA01650", { version: "01.000" }]]),
    });
    expect(h).toContain("cw-libview");
    expect(h).toContain(t("cw_lib_parts", { n: 6 }).replace("'", "&#x27;"));
    expect(h).toContain(t("cw_lib_installed_v", { version: "01.000" }));
    expect(h).toContain(t("cw_lib_badge_update", { version: "01.020" }));
    expect(h).toContain("4.50");
    expect(h).toContain(t("cw_lib_fw_newer", { need: "8.00", have: "7.61" }));
  });

  it("offers Install for a package, Copy for a folder on a share, and Register once it is copied", () => {
    const s = parseScan(SCAN_JSON) as LibScan;
    const entries = entriesOf(base().sources, new Map([["3fa2c1d0", s]]));
    const folder = entries.find((e) => e.kind === "folder")!;
    expect(html({ open: "ps5:PPSA01650" })).toContain(t("cw_lib_install"));
    const h = html({ open: "ps4:CUSA07842" });
    expect(h).not.toContain(t("cw_lib_install"));
    expect(h).toContain(t("cw_lib_copy_here"));
    expect(h).not.toContain(t("cw_lib_register"));
    expect(html({ open: "ps4:CUSA07842", copied: { key: folder.key, path: "/mnt/usb0/homebrew/CUSA07842" } })).toContain(t("cw_lib_register"));
    expect(html({ open: "ps4:CUSA07842", onShowInFiles: vi.fn() })).toContain(t("cw_cleanup_show_in_files"));
  });
});
