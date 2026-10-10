import { describe, expect, it } from "vitest";

import { NAV_ITEMS, PINNED_NAV_ITEMS, groupNavItems } from "./navItems";
import { tabForPath, type TabId } from "./TabNav";

// The tabs must file every screen where the sidebar does: one section, one tab.
const SECTION_TAB: Record<string, TabId> = {
  nav_section_setup: "home",
  nav_section_help: "home",
  nav_section_files: "files",
  nav_section_games_mods: "games",
  nav_section_console: "console",
  nav_section_advanced: "console",
  nav_section_diagnostics: "tasks",
};

describe("TabNav groups follow the sidebar's sections", () => {
  it("knows every section", () => {
    for (const g of groupNavItems(NAV_ITEMS)) {
      expect(SECTION_TAB[g.section.key], g.section.key).toBeDefined();
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
    expect(tabForPath("/search")).toBe("files");
    expect(tabForPath("/backup")).toBe("files");
    expect(tabForPath("/local-image")).toBe("games");
  });

  it("covers pinned items, redirects and sub-routes", () => {
    for (const item of PINNED_NAV_ITEMS) expect(tabForPath(item.to), item.to).not.toBeNull();
    expect(tabForPath("/games/PPSA01234")).toBe("games");
    expect(tabForPath("/activity")).toBe("tasks");
    expect(tabForPath("/file-system")).toBe("files");
    expect(tabForPath("/more")).toBeNull();
  });
});
