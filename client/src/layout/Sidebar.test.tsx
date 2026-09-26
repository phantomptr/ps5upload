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
    (key: string, _vars?: Record<string, string | number>, fallback?: string) =>
      fallback ?? key,
}));
vi.mock("../state/logs", () => ({
  useLogsStore: (
    selector: (state: { entries: Array<{ level: string }> }) => unknown,
  ) => selector({ entries: [] }),
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

describe("Sidebar", () => {
  it("assumes nothing beyond Home, and always keeps More reachable", () => {
    // safeStorage is mocked empty above, so this is a first run: no
    // favorites stored. The sidebar used to hardcode five screens; now it
    // picks exactly one for the user and offers the rest via More.
    const html = renderToStaticMarkup(
      <MemoryRouter initialEntries={["/home"]}>
        <Sidebar />
      </MemoryRouter>,
    );

    expect(html).toContain('data-collapsed="false"');
    expect(html).toContain('href="/home"');
    // Not assumed on the user's behalf any more.
    expect(html).not.toContain('href="/games"');
    expect(html).not.toContain('href="/files"');
    expect(html).not.toContain('href="/console"');
    expect(html).not.toContain('href="/tasks"');
    // The escape hatch must survive, or an empty Favorites list would
    // strand the user on Home with no way to reach anything else.
    expect(html).toContain('href="/more"');
    expect(html).not.toContain('href="/install-package"');
  });

  it("shows the hint that explains how to fill the sidebar", () => {
    const html = renderToStaticMarkup(
      <MemoryRouter initialEntries={["/home"]}>
        <Sidebar />
      </MemoryRouter>,
    );
    expect(html).toContain("Star screens in More");
  });

  it("renders stored favorites after Home", () => {
    vi.resetModules();
    vi.doMock("../state/navFavorites", () => ({
      useNavFavoritesStore: (
        selector: (state: {
          favorites: string[];
          hintDismissed: boolean;
          dismissHint: () => void;
        }) => unknown,
      ) =>
        selector({
          favorites: ["/files", "/games"],
          hintDismissed: true,
          dismissHint: () => {},
        }),
    }));
    return import("./Sidebar").then(({ default: Pinned }) => {
      const html = renderToStaticMarkup(
        <MemoryRouter initialEntries={["/home"]}>
          <Pinned />
        </MemoryRouter>,
      );
      expect(html).toContain('href="/home"');
      expect(html).toContain('href="/files"');
      expect(html).toContain('href="/games"');
      // Hint retires once something is pinned.
      expect(html).not.toContain("Star screens in More");
      // Order follows the stored list, not NAV_ITEMS order.
      expect(html.indexOf('href="/files"')).toBeLessThan(
        html.indexOf('href="/games"'),
      );
    });
  });
});
