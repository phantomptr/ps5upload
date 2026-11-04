import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../lib/safeStorage", () => ({
  safeGetItem: () => null,
  safeSetItem: vi.fn(),
}));
vi.mock("../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let out = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) out = out.replace(`{${k}}`, String(v));
      return out;
    },
}));
vi.mock("../state/logs", () => ({
  useLogsStore: (
    selector: (state: { errorCount: number }) => unknown,
  ) => selector({ errorCount: 0 }),
}));
vi.mock("../state/update", () => ({
  useUpdateStore: (
    selector: (state: { phase: { kind: string } }) => unknown,
  ) => selector({ phase: { kind: "idle" } }),
}));
vi.mock("./NotificationInbox", () => ({
  default: () => <span data-testid="notifications" />,
}));
vi.mock("./RosterPicker", () => ({
  default: () => <div data-testid="roster" />,
}));

import Sidebar from "./Sidebar";

function render(el: React.ReactElement = <Sidebar />) {
  return renderToStaticMarkup(<MemoryRouter initialEntries={["/home"]}>{el}</MemoryRouter>);
}

/** A sidebar whose store holds these lists (and, optionally, starts collapsed). */
async function withLists(
  hidden: string[],
  closedSections: string[],
  pinned: string[] = [],
  collapsed = false,
  windows = false,
) {
  vi.resetModules();
  vi.doMock("../lib/platform", async (orig) => ({
    ...(await orig<typeof import("../lib/platform")>()),
    isWindows: () => windows,
  }));
  vi.doMock("../lib/safeStorage", () => ({
    safeGetItem: (k: string) => (collapsed && k === "ps5upload.desktop-sidebar.collapsed.v1" ? "1" : null),
    safeSetItem: vi.fn(),
  }));
  vi.doMock("../state/navSidebar", () => ({
    useNavSidebarStore: (selector: (state: Record<string, unknown>) => unknown) =>
      selector({
        hidden,
        closedSections,
        pinned,
        toggleHidden: () => {},
        toggleSection: () => {},
        togglePinned: () => {},
        movePinnedTo: () => {},
      }),
  }));
  return (await import("./Sidebar")).default;
}

describe("Sidebar", () => {
  it("shows every screen from the start, in its sections, and keeps More", () => {
    // safeStorage is mocked empty: a first run, nothing hidden.
    const html = render();
    expect(html).toContain('data-collapsed="false"');
    for (const to of ["/home", "/send", "/workshop", "/files", "/games", "/console", "/settings"]) {
      expect(html).toContain(`href="${to}"`);
    }
    expect(html).toContain("Files &amp; Storage");
    expect(html).toContain('href="/more"');
    // The old favorites hint is gone.
    expect(html).not.toContain("Star screens in More");
  });

  it("offers to hide each screen but Home", () => {
    const html = render();
    expect(html).toContain('aria-label="Hide Workshop from the sidebar"');
    expect(html).not.toContain('aria-label="Hide Home from the sidebar"');
  });

  // A Tab stop after every link doubled the walk through the sidebar: 40 presses
  // never left it. The buttons are still there for the pointer, are reached
  // from a focused link with the arrow key toward them, and every screen has the
  // same switch on More.
  it("keeps the hide buttons out of the Tab order", () => {
    const html = render();
    const buttons = html.match(/<button[^>]*aria-label="Hide [^"]* from the sidebar"[^>]*>/g) ?? [];
    expect(buttons.length).toBeGreaterThan(5);
    for (const b of buttons) expect(b).toContain('tabindex="-1"');
    // The links themselves stay Tab stops.
    expect(html).not.toMatch(/<a[^>]*tabindex="-1"/);
  });

  it("leaves hidden screens out and says how many, with the way back", async () => {
    const Hidden = await withLists(["/cheats", "/shell"], []);
    const html = render(<Hidden />);
    expect(html).not.toContain('href="/cheats"');
    expect(html).not.toContain('href="/shell"');
    expect(html).toContain('href="/games"');
    expect(html).toContain("2 hidden");
  });

  it("pins Bug report right under Home, outside the sections, with no way to hide it", async () => {
    const html = render();
    const home = html.indexOf('href="/home"');
    const report = html.indexOf('href="/bug-report"');
    const firstSection = html.indexOf("<section");
    expect(home).toBeGreaterThan(-1);
    expect(report).toBeGreaterThan(home);
    expect(report).toBeLessThan(firstSection);
    expect(html).not.toContain('aria-label="Hide Bug report from the sidebar"');
    // Hidden by an older version: it stays, and is not counted as hidden.
    const Old = await withLists(["/bug-report"], ["nav_section_diagnostics"]);
    const old = render(<Old />);
    expect(old).toContain('href="/bug-report"');
    expect(old).not.toContain("1 hidden");
  });

  it("folds a closed section away but keeps its header", async () => {
    const Folded = await withLists([], ["nav_section_diagnostics"]);
    const html = render(<Folded />);
    expect(html).toContain("Diagnostics");
    expect(html).toMatch(/aria-expanded="false"[^>]*>(?:(?!<\/button>).)*Diagnostics/);
    expect(html).not.toContain('href="/logs"');
    expect(html).toContain('href="/games"');
  });

  it("has no Pinned group until something is pinned", () => {
    const html = render();
    expect(html).not.toContain('data-testid="nav-pinned"');
    expect(html).not.toContain(">Pinned<");
    // Every section row offers a pin, next to its hide button.
    expect(html).toContain('aria-label="Pin Workshop"');
    expect(html).not.toContain('aria-label="Pin Home"');
  });

  it("shows pinned screens in pin order under Home and Bug report, and only there", async () => {
    const Pinned = await withLists([], [], ["/cheats", "/files"]);
    const html = render(<Pinned />);
    const group = html.slice(html.indexOf('data-testid="nav-pinned"'));
    const groupEnd = group.indexOf("</section>");
    const inGroup = group.slice(0, groupEnd);
    expect(html.indexOf('href="/bug-report"')).toBeLessThan(html.indexOf('data-testid="nav-pinned"'));
    expect(inGroup).toContain(">Pinned<");
    expect(inGroup.indexOf('href="/cheats"')).toBeLessThan(inGroup.indexOf('href="/files"'));
    expect(inGroup).toContain('aria-label="Unpin Cheats"');
    // Rows of the Pinned group drag to reorder, and have no hide button.
    expect(inGroup).toContain('draggable="true"');
    expect(inGroup).not.toContain("from the sidebar");
    // Listed once: their own sections leave them out while they are pinned.
    expect(html.split('href="/files"').length - 1).toBe(1);
    expect(html.split('href="/cheats"').length - 1).toBe(1);
  });

  it("shows a pinned screen that is hidden in Pinned only, and does not count it hidden", async () => {
    const Both = await withLists(["/cheats", "/shell"], [], ["/cheats"]);
    const html = render(<Both />);
    expect(html.split('href="/cheats"').length - 1).toBe(1);
    const group = html.slice(html.indexOf('data-testid="nav-pinned"'));
    expect(group.slice(0, group.indexOf("</section>"))).toContain('href="/cheats"');
    expect(html).toContain("1 hidden");
  });

  it("offers no more pins once twelve are pinned", async () => {
    const { NAV_ITEMS } = await import("./navItems");
    const twelve = NAV_ITEMS.filter((i) => !i.beta).slice(0, 12).map((i) => i.to);
    const Full = await withLists([], [], twelve);
    const html = render(<Full />);
    const unpinned = NAV_ITEMS.filter((i) => !i.beta && !twelve.includes(i.to))[0];
    const button = html.match(new RegExp(`<button[^>]*aria-label="Pin ${unpinned.fallback}"[^>]*>`))?.[0];
    expect(button).toContain('disabled=""');
    expect(button).toContain('title="You can pin up to 12 screens"');
  });

  it("puts pinned icons at the top of the collapsed rail, with tooltips", async () => {
    const Rail = await withLists([], [], ["/files"], true);
    const html = render(<Rail />);
    expect(html).toContain('data-collapsed="true"');
    const group = html.slice(html.indexOf('data-testid="nav-pinned"'));
    const inGroup = group.slice(0, group.indexOf("</section>"));
    expect(inGroup).toMatch(/<a[^>]*href="\/files"/);
    expect(inGroup).toContain('title="Files"');
    expect(inGroup).not.toContain(">Pinned<");
    expect(inGroup).not.toContain("<button");
    expect(inGroup).not.toContain("draggable");
    // Above the sections' icons.
    expect(html.indexOf('data-testid="nav-pinned"')).toBeLessThan(html.indexOf('href="/send"'));
  });

  it("offers no drag in the Windows app, whose window takes every drag", async () => {
    const Win = await withLists([], [], ["/files", "/send"], false, true);
    const html = render(<Win />);
    expect(html).toContain('data-testid="nav-pinned"');
    expect(html).not.toContain("draggable");
  });
});
