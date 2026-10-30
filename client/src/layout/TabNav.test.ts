import { describe, expect, it } from "vitest";

import { NAV_ITEMS, FIXED_NAV_ITEMS, groupNavItems } from "./navItems";
import { tabForPath, type TabId } from "./TabNav";

// The tabs must file every screen where the sidebar does: one section, one tab.
// Diagnostics has no tab since Play took it (10.0.0): its screens are in More (null).
const SECTION_TAB: Record<string, TabId | null> = {
  nav_section_connect: "home",
  nav_section_app: "home",
  nav_section_files: "files",
  nav_section_games_mods: "games",
  nav_section_console: "console",
  nav_section_diagnostics: null,
};

describe("TabNav groups follow the sidebar's sections", () => {
  it("knows every section", () => {
    for (const g of groupNavItems(NAV_ITEMS)) {
      expect(g.section.key in SECTION_TAB, g.section.key).toBe(true);
    }
  });

  it("lights the tab of each screen's section", () => {
    for (const g of groupNavItems(NAV_ITEMS)) {
      for (const item of g.items) {
        expect([item.to, tabForPath(item.to)]).toEqual([item.to, SECTION_TAB[g.section.key]]);
      }
    }
  });

  it("files the screens the audit found misplaced where the sidebar has them", () => {
    expect(tabForPath("/volumes")).toBe("files");
    expect(tabForPath("/cleanup")).toBe("files");
    expect(tabForPath("/workshop")).toBe("games");
  });

  it("covers pinned items, redirects and sub-routes", () => {
    for (const item of FIXED_NAV_ITEMS) expect(tabForPath(item.to), item.to).not.toBeNull();
    expect(FIXED_NAV_ITEMS.map((i) => i.to)).toEqual(["/home", "/play", "/bug-report"]);
    expect(tabForPath("/games/PPSA01234")).toBe("games");
    expect(tabForPath("/activity")).toBeNull();
    expect(tabForPath("/play")).toBe("play");
    expect(tabForPath("/bug-report")).toBe("home");
    expect(tabForPath("/file-system")).toBe("files");
    expect(tabForPath("/more")).toBeNull();
  });
});
