import { describe, it, expect } from "vitest";

import en from "../i18n/locales/en";
import {
  NAV_ITEMS,
  HOME_NAV_ITEM,
  sidebarGroups,
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

describe("the sidebar's sections", () => {
  const paths = (groups: ReturnType<typeof sidebarGroups>) => groups.flatMap((g) => g.items.map((i) => i.to));
  const sections = (groups: ReturnType<typeof sidebarGroups>) => groups.map((g) => g.section.key);

  it("lists every screen, in its sections, with nothing hidden", () => {
    const groups = sidebarGroups([], false, false);
    expect(paths(groups)).toEqual(NAV_ITEMS.filter((i) => !i.beta).map((i) => i.to));
    expect(sections(groups)).toContain("nav_section_files");
    // Home sits above the sections, not inside one.
    expect(paths(groups)).not.toContain(HOME_NAV_ITEM.to);
  });

  it("leaves hidden screens out", () => {
    const groups = sidebarGroups(["/cheats", "/shell"], false, false);
    expect(paths(groups)).not.toContain("/cheats");
    expect(paths(groups)).not.toContain("/shell");
    expect(paths(groups)).toContain("/games");
  });

  it("keeps a section whose first screen is hidden, with the rest in it", () => {
    // Upload opens Files & storage: hiding it must not merge the rest into Setup.
    const groups = sidebarGroups(["/upload"], false, false);
    const files = groups.find((g) => g.section.key === "nav_section_files");
    expect(files?.items.map((i) => i.to)).toContain("/files");
    const setup = groups.find((g) => g.section.key === "nav_section_setup");
    expect(setup?.items.map((i) => i.to)).not.toContain("/files");
  });

  it("drops a section once every screen in it is hidden", () => {
    const help = groupNavItems(NAV_ITEMS).find((g) => g.section.key === "nav_section_help")!;
    const groups = sidebarGroups(help.items.map((i) => i.to), false, false);
    expect(sections(groups)).not.toContain("nav_section_help");
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

// Convert Games left the beta program once its packages installed and played: it shows for
// everyone, with the beta switch off.
describe("Convert Games", () => {
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
