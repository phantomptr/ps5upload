import { describe, it, expect } from "vitest";

import en from "../i18n/locales/en";
import {
  NAV_ITEMS,
  HOME_NAV_ITEM,
  ABOUT_NAV_ITEM,
  PERMANENT_NAV_ITEMS,
  sidebarNavItems,
  resolveFavorites,
  groupNavItems,
  filterNavItems,
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

  it("keeps Install Package directly reachable", () => {
    expect(NAV_ITEMS.map((item) => item.to)).toContain("/install-package");
  });

  it("keeps every screen reachable from More", () => {
    expect(NAV_ITEMS.map((item) => item.to)).toContain("/install-package");
  });

  it("retires standalone backport screens in favor of Games", () => {
    expect(NAV_ITEMS.map((item) => item.to)).not.toContain("/fakelib");
    expect(NAV_ITEMS.map((item) => item.to)).not.toContain("/sdk-changer");
    expect(NAV_ITEMS.map((item) => item.to)).toContain("/games");
  });
});

describe("sidebar favorites", () => {
  it("pins Home permanently and starts with nothing else assumed", () => {
    // The sidebar used to hardcode five screens. Home is the only one the
    // app still chooses for the user; everything else is opt-in.
    expect(HOME_NAV_ITEM.to).toBe("/home");
    expect(HOME_NAV_ITEM.section).toBeDefined();
    expect(resolveFavorites([])).toEqual([]);
  });

  it("resolves stored paths in the order they were starred", () => {
    const out = resolveFavorites(["/files", "/games"]);
    expect(out.map((i) => i.to)).toEqual(["/files", "/games"]);
  });

  it("drops paths that no longer exist", () => {
    // Favorites outlive the build that wrote them, so a screen removed or
    // renamed in a later version must not leave a dead row linking nowhere.
    expect(
      resolveFavorites(["/files", "/screen-that-was-removed"]).map((i) => i.to),
    ).toEqual(["/files"]);
  });

  it("never lets Home appear twice", () => {
    expect(resolveFavorites(["/home", "/files"]).map((i) => i.to)).toEqual([
      "/files",
    ]);
  });

  it("ignores a duplicate entry in the stored list", () => {
    expect(resolveFavorites(["/files", "/files"]).map((i) => i.to)).toEqual([
      "/files",
    ]);
  });

  it("renders as one group under the Favorites header", () => {
    const groups = groupNavItems([
      HOME_NAV_ITEM,
      ...resolveFavorites(["/files", "/games"]),
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].section.key).toBe("nav_section_favorites");
    expect(groups[0].items.map((i) => i.to)).toEqual([
      "/home",
      "/files",
      "/games",
    ]);
  });

  it("uses translation keys present in the English catalogue", () => {
    for (const item of NAV_ITEMS) {
      expect(en, `missing navigation translation: ${item.key}`).toHaveProperty(
        item.key,
      );
      if (item.section) {
        expect(
          en,
          `missing navigation section translation: ${item.section.key}`,
        ).toHaveProperty(item.section.key);
      }
    }
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

describe("permanent sidebar rows", () => {
  it("pins Home and About", () => {
    expect([...PERMANENT_NAV_ITEMS].map((i) => i.to).sort()).toEqual([
      "/about",
      "/home",
    ]);
  });

  it("renders Home first and About last, whatever is starred", () => {
    expect(sidebarNavItems([]).map((i) => i.to)).toEqual(["/home", "/about"]);
    expect(sidebarNavItems(["/settings", "/logs"]).map((i) => i.to)).toEqual([
      "/home",
      "/settings",
      "/logs",
      "/about",
    ]);
  });

  it("keeps About last even when it is stored as a favorite", () => {
    expect(sidebarNavItems(["/about", "/settings"]).map((i) => i.to)).toEqual([
      "/home",
      "/settings",
      "/about",
    ]);
  });

  it("puts every row in one group, with About in it", () => {
    const groups = groupNavItems(sidebarNavItems(["/settings"]));
    expect(groups).toHaveLength(1);
    expect(groups[0].items.map((i) => i.to)).toEqual([
      "/home",
      "/settings",
      "/about",
    ]);
  });

  it("only the first permanent row opens the section", () => {
    // groupNavItems drops anything before the first section header, so if
    // About ever grew its own `section` it would split the group in two.
    expect(HOME_NAV_ITEM.section).toBeDefined();
    expect(ABOUT_NAV_ITEM.section).toBeUndefined();
  });

  it("never renders a permanent row twice when it is also starred", () => {
    // Favorites are hand-editable on disk and survive downgrades, so a stored
    // "/about" from an older build must not produce a duplicate row.
    const resolved = resolveFavorites(["/about", "/home", "/settings"]);
    expect(resolved.map((i) => i.to)).not.toContain("/about");
    expect(resolved.map((i) => i.to)).not.toContain("/home");
    expect(resolved.map((i) => i.to)).toContain("/settings");
  });
});

// SMB Browser and FTP Server were replaced by Connections: they are gone from the navigation,
// and an old link or saved favourite to either lands on Connections.
describe("retired screens", () => {
  const APP = Object.values(
    import.meta.glob("../App.tsx", { query: "?raw", import: "default", eager: true }) as Record<
      string,
      string
    >,
  )[0];

  it("are not in the navigation, and Connections is", async () => {
    const { NAV_ITEMS } = await import("./navItems");
    const paths = NAV_ITEMS.map((i: { to: string }) => i.to);
    expect(paths).not.toContain("/smb-browser");
    expect(paths).not.toContain("/ftp-server");
    expect(paths).toContain("/connections");
  });

  it("send their old links to Connections", () => {
    for (const old of ["/smb-browser", "/ftp-server"]) {
      expect(APP).toMatch(
        new RegExp(`path="${old}"\\s+element=\\{<Navigate to="/connections" replace />\\}`),
      );
    }
    expect(APP).not.toMatch(/SmbBrowserScreen|FtpServerScreen/);
  });
});

// Convert to FPKG left the beta program once its packages installed and played: it shows for
// everyone, with the beta switch off.
describe("Convert to FPKG", () => {
  it("is no longer a beta feature", () => {
    const convert = NAV_ITEMS.find((i) => i.to === "/convert");
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
