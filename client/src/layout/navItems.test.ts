import { describe, it, expect } from "vitest";

import en from "../i18n/locales/en";
import {
  NAV_ITEMS,
  HOME_NAV_ITEM,
  FIXED_NAV_ITEMS,
  sidebarGroups,
  navItemOffered,
  groupNavItems,
  filterNavItems,
  cleanPinned,
  cleanHidden,
  NAV_MOVED,
  navMoved,
  pinnedNavItems,
  pinnedFirst,
  MAX_PINNED,
  type NavItem,
} from "./navItems";

/** Minimal `tr` stub: returns the fallback, like an untranslated locale. */
const tr = (
  key: string,
  _vars?: Record<string, string | number>,
  fallback?: string,
) => fallback ?? key;

/** Icons are irrelevant to these pure functions. */
const icon = null as unknown as NavItem["icon"];

describe("NAV_ITEMS", () => {
  it("every item has a unique route", () => {
    const routes = NAV_ITEMS.map((i) => i.to);
    expect(new Set(routes).size).toBe(routes.length);
  });

  it("starts with a sectioned item so no item is orphaned", () => {
    // groupNavItems drops anything before the first section header.
    expect(NAV_ITEMS[0].section).toBeDefined();
  });

  it("keeps every item reachable through grouping", () => {
    const grouped = groupNavItems(NAV_ITEMS).flatMap((g) => g.items);
    expect(grouped).toHaveLength(NAV_ITEMS.length);
  });

  it("keeps Upload & Install directly reachable", () => {
    expect(NAV_ITEMS.map((item) => item.to)).toContain("/send");
  });

  it("keeps every screen reachable from More", () => {
    expect(NAV_ITEMS.map((item) => item.to)).toContain("/send");
  });

  it("retires standalone backport screens in favor of Games", () => {
    expect(NAV_ITEMS.map((item) => item.to)).not.toContain("/fakelib");
    expect(NAV_ITEMS.map((item) => item.to)).not.toContain("/sdk-changer");
    expect(NAV_ITEMS.map((item) => item.to)).toContain("/games");
  });
});

describe("the sidebar's sections", () => {
  const paths = (groups: ReturnType<typeof sidebarGroups>) => groups.flatMap((g) => g.items.map((i) => i.to));
  const sections = (groups: ReturnType<typeof sidebarGroups>) => groups.map((g) => g.section.key);

  it("lists every screen, in its sections, with nothing hidden", () => {
    const groups = sidebarGroups([], false, false);
    expect(paths(groups)).toEqual(NAV_ITEMS.filter((i) => !i.beta).map((i) => i.to));
    expect(sections(groups)).toContain("nav_section_files");
    // Home and Bug report sit above the sections, not inside one.
    expect(paths(groups)).not.toContain(HOME_NAV_ITEM.to);
    expect(paths(groups)).not.toContain("/bug-report");
    expect(paths(groups)).not.toContain("/play");
    expect(FIXED_NAV_ITEMS.map((i) => i.to)).toEqual(["/home", "/play", "/bug-report"]);
  });

  it("leaves hidden screens out", () => {
    const groups = sidebarGroups(["/cheats", "/shell"], false, false);
    expect(paths(groups)).not.toContain("/cheats");
    expect(paths(groups)).not.toContain("/shell");
    expect(paths(groups)).toContain("/games");
  });

  it("keeps a section whose first screen is hidden, with the rest in it", () => {
    // File System opens Files & Storage: hiding it must not merge the rest into Games & Content.
    const groups = sidebarGroups(["/files"], false, false);
    const files = groups.find((g) => g.section.key === "nav_section_files");
    expect(files?.items.map((i) => i.to)).toContain("/shared-folders");
    const games = groups.find((g) => g.section.key === "nav_section_games_mods");
    expect(games?.items.map((i) => i.to)).not.toContain("/shared-folders");
  });

  it("drops a section once every screen in it is hidden", () => {
    const app = groupNavItems(NAV_ITEMS).find((g) => g.section.key === "nav_section_app")!;
    const groups = sidebarGroups(app.items.map((i) => i.to), false, false);
    expect(sections(groups)).not.toContain("nav_section_app");
  });

  it("names every screen and section with a key the English catalogue has", () => {
    const catalogue = en as Record<string, string>;
    for (const g of sidebarGroups([], true, false)) {
      expect(catalogue[g.section.key], g.section.key).toBeTruthy();
      for (const i of g.items) expect(catalogue[i.key], i.key).toBeTruthy();
    }
    expect(catalogue[HOME_NAV_ITEM.key]).toBeTruthy();
  });

  it("leaves out what the browser build can't offer", () => {
    const browserOnlyHidden = NAV_ITEMS.filter((i) => i.hideInBrowser).map((i) => i.to);
    expect(browserOnlyHidden.length).toBeGreaterThan(0);
    const inBrowser = paths(sidebarGroups([], false, true));
    for (const p of browserOnlyHidden) expect(inBrowser).not.toContain(p);
  });

  it("offers Workshop everywhere (looking inside an image needs no mount, #479)", () => {
    expect(paths(sidebarGroups([], false, false, true))).toContain("/workshop");
    // Windows or Android desktop app.
    expect(paths(sidebarGroups([], false, false, false))).toContain("/workshop");
    // Browser build: the engine reads the image; the screen hides the mount part.
    expect(paths(sidebarGroups([], false, true, true))).toContain("/workshop");
  });
});

describe("navItemOffered", () => {
  const item = (extra: Partial<NavItem>): NavItem => ({
    to: "/x",
    key: "x",
    fallback: "X",
    icon,
    ...extra,
  });
  it("keeps an ordinary screen everywhere", () => {
    for (const inBrowser of [false, true])
      for (const macOrLinux of [false, true])
        expect(navItemOffered(item({}), { inBrowser, macOrLinux })).toBe(true);
  });
  it("drops a macOrLinuxOnly screen elsewhere", () => {
    const i = item({ macOrLinuxOnly: true });
    expect(navItemOffered(i, { inBrowser: false, macOrLinux: true })).toBe(true);
    expect(navItemOffered(i, { inBrowser: false, macOrLinux: false })).toBe(false);
  });
  it("drops a hideInBrowser screen in the browser", () => {
    const i = item({ hideInBrowser: true });
    expect(navItemOffered(i, { inBrowser: true, macOrLinux: true })).toBe(false);
    expect(navItemOffered(i, { inBrowser: false, macOrLinux: true })).toBe(true);
  });
});

describe("groupNavItems", () => {
  it("groups items under the preceding section header", () => {
    const groups = groupNavItems([
      {
        to: "/a",
        key: "a",
        fallback: "A",
        icon,
        section: { key: "s1", fallback: "S1" },
      },
      { to: "/b", key: "b", fallback: "B", icon },
      {
        to: "/c",
        key: "c",
        fallback: "C",
        icon,
        section: { key: "s2", fallback: "S2" },
      },
    ]);
    expect(groups).toHaveLength(2);
    expect(groups[0].section.key).toBe("s1");
    expect(groups[0].items.map((i) => i.to)).toEqual(["/a", "/b"]);
    expect(groups[1].items.map((i) => i.to)).toEqual(["/c"]);
  });

  it("returns an empty array for no items", () => {
    expect(groupNavItems([])).toEqual([]);
  });

  it("drops leading items that precede any section header", () => {
    const groups = groupNavItems([
      { to: "/orphan", key: "o", fallback: "O", icon },
      {
        to: "/a",
        key: "a",
        fallback: "A",
        icon,
        section: { key: "s1", fallback: "S1" },
      },
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].items.map((i) => i.to)).toEqual(["/a"]);
  });
});

describe("filterNavItems", () => {
  const items: NavItem[] = [
    {
      to: "/console",
      key: "hardware",
      fallback: "Hardware",
      icon,
      section: { key: "s", fallback: "S" },
    },
    { to: "/saves", key: "saves", fallback: "Save data", icon },
  ];

  it("returns everything for an empty query", () => {
    expect(filterNavItems(items, "", tr)).toHaveLength(2);
    expect(filterNavItems(items, "   ", tr)).toHaveLength(2);
  });

  it("matches case-insensitively", () => {
    expect(filterNavItems(items, "HARD", tr).map((i) => i.to)).toEqual([
      "/console",
    ]);
  });

  it("matches the English fallback even when the label is translated", () => {
    // Most community docs use the English screen names, so "hardware"
    // must find Hardware even on a Japanese locale.
    const jaTr = (k: string) => (k === "hardware" ? "ハードウェア" : k);
    expect(filterNavItems(items, "hardware", jaTr).map((i) => i.to)).toEqual([
      "/console",
    ]);
  });

  it("matches the translated label", () => {
    const jaTr = (k: string) => (k === "hardware" ? "ハードウェア" : k);
    expect(filterNavItems(items, "ハード", jaTr).map((i) => i.to)).toEqual([
      "/console",
    ]);
  });

  it("ignores diacritics in both query and label", () => {
    const frTr = (k: string) => (k === "saves" ? "Sauvegardés" : k);
    expect(filterNavItems(items, "sauvegardes", frTr).map((i) => i.to)).toEqual(
      ["/saves"],
    );
    expect(filterNavItems(items, "Sauvegardés", frTr).map((i) => i.to)).toEqual(
      ["/saves"],
    );
  });

  it("returns an empty array when nothing matches", () => {
    expect(filterNavItems(items, "zzzz", tr)).toEqual([]);
  });

  it("matches on a substring anywhere in the label", () => {
    expect(filterNavItems(items, "data", tr).map((i) => i.to)).toEqual([
      "/saves",
    ]);
  });
});

describe("retired screens", () => {
  const APP = Object.values(
    import.meta.glob("../App.tsx", { query: "?raw", import: "default", eager: true }) as Record<
      string,
      string
    >,
  )[0];

  it("are not in the navigation, and Shared Folders is", async () => {
    const { NAV_ITEMS } = await import("./navItems");
    const paths = NAV_ITEMS.map((i: { to: string }) => i.to);
    expect(paths).not.toContain("/smb-browser");
    expect(paths).not.toContain("/ftp-server");
    expect(paths).toContain("/shared-folders");
    expect(paths).not.toContain("/connections");
    expect(paths).not.toContain("/mesh-drive");
  });

  it("send their old links to Shared Folders", () => {
    for (const old of ["/smb-browser", "/ftp-server"]) {
      expect(APP).toMatch(
        new RegExp(`path="${old}"\\s+element=\\{<Navigate to="/shared-folders" replace />\\}`),
      );
    }
    // The screens folded into it keep their query (?add=1, ?edit=<id>) and land on their part.
    expect(APP).toContain(`path="/connections" element={<MovedTo to="/shared-folders" />}`);
    expect(APP).toContain(`path="/mesh-drive" element={<MovedTo to="/shared-folders" hash="mesh-drive" />}`);
    expect(APP).not.toMatch(/SmbBrowserScreen|FtpServerScreen/);
  });
});

// Convert Games (now Workshop) left the beta program once its packages installed and played:
// it shows for everyone, with the beta switch off.
describe("Workshop", () => {
  it("is no longer a beta feature", () => {
    const convert = NAV_ITEMS.find((i) => i.to === "/workshop");
    expect(convert).toBeDefined();
    expect(convert?.beta).toBeFalsy();
  });

  it("is reachable without the beta switch", () => {
    const APP = Object.values(
      import.meta.glob("../App.tsx", { query: "?raw", import: "default", eager: true }) as Record<
        string,
        string
      >,
    )[0];
    expect(APP).not.toMatch(/BetaRoute/);
  });
});

// A switch that reveals nothing is a dead control: Settings shows it only while
// something is actually in beta.
describe("hasBetaItems", () => {
  it("is false when no screen is in beta", async () => {
    const { hasBetaItems } = await import("./navItems");
    expect(hasBetaItems([{ to: "/a", beta: false } as never])).toBe(false);
    expect(hasBetaItems([{ to: "/a", beta: true } as never])).toBe(true);
    expect(hasBetaItems()).toBe(NAV_ITEMS.some((i) => i.beta));
  });
});

describe("screens folded into another", () => {
  it("move to a screen that is in the nav", () => {
    for (const [from, to] of Object.entries(NAV_MOVED)) {
      expect(NAV_ITEMS.some((i) => i.to === to), `${from} -> ${to}`).toBe(true);
      expect(NAV_ITEMS.some((i) => i.to === from), from).toBe(false);
    }
    expect(navMoved("/profile")).toBe("/accounts");
    expect(navMoved("/files")).toBe("/files");
  });

  it("carry a pin over once, in its place", () => {
    expect(cleanPinned(["/profile", "/files", "/remote-play"])).toEqual(["/accounts", "/files"]);
  });

  it("stay hidden only when every screen they replace was hidden", () => {
    expect(cleanHidden(["/profile", "/files"])).toEqual(["/files"]);
    expect(cleanHidden(["/remote-play", "/profile", "/files"])).toEqual(["/accounts", "/files"]);
    expect(cleanHidden(["/accounts"])).toEqual(["/accounts"]);
    expect(cleanHidden([3, "/files", "/files"])).toEqual(["/files"]);
  });
});

describe("pinned screens", () => {
  it("cleans a stored pin list: real screens only, no Home/Bug report, no repeats", () => {
    expect(cleanPinned(["/files", "/gone", 3, "/files", "/home", "/bug-report", "/send"])).toEqual([
      "/files",
      "/send",
    ]);
    // Not capped on load: pins this build doesn't show don't count toward MAX_PINNED, and
    // capping here would drop a pin the user still has.
    expect(cleanPinned(NAV_ITEMS.map((i) => i.to))).toHaveLength(NAV_ITEMS.length);
    expect(MAX_PINNED).toBe(12);
  });

  it("resolves pins in pin order, ignoring hidden, but not beta or platform rules", () => {
    expect(pinnedNavItems(["/cheats", "/files"], false, false).map((i) => i.to)).toEqual([
      "/cheats",
      "/files",
    ]);
    expect(pinnedNavItems(["/shell", "/files"], false, true).map((i) => i.to)).toEqual(["/files"]);
  });

  it("orders pinned items first, in pin order, the rest unchanged", () => {
    const items = [{ to: "a" }, { to: "b" }, { to: "c" }, { to: "d" }];
    expect(pinnedFirst(items, ["c", "a"]).map((i) => i.to)).toEqual(["c", "a", "b", "d"]);
    expect(pinnedFirst(items, []).map((i) => i.to)).toEqual(["a", "b", "c", "d"]);
  });
});

