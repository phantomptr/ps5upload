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

function render(el: React.ReactElement = <Sidebar />) {
  return renderToStaticMarkup(<MemoryRouter initialEntries={["/home"]}>{el}</MemoryRouter>);
}

/** A sidebar whose store holds these lists. */
async function withLists(hidden: string[], closedSections: string[]) {
  vi.resetModules();
  vi.doMock("../state/navSidebar", () => ({
    useNavSidebarStore: (
      selector: (state: {
        hidden: string[];
        closedSections: string[];
        toggleHidden: () => void;
        toggleSection: () => void;
      }) => unknown,
    ) => selector({ hidden, closedSections, toggleHidden: () => {}, toggleSection: () => {} }),
  }));
  return (await import("./Sidebar")).default;
}

describe("Sidebar", () => {
  it("shows every screen from the start, in its sections, and keeps More", () => {
    // safeStorage is mocked empty: a first run, nothing hidden.
    const html = render();
    expect(html).toContain('data-collapsed="false"');
    for (const to of ["/home", "/upload", "/install-package", "/convert", "/files", "/games", "/console", "/settings"]) {
      expect(html).toContain(`href="${to}"`);
    }
    expect(html).toContain("Files &amp; storage");
    expect(html).toContain('href="/more"');
    // The old favorites hint is gone.
    expect(html).not.toContain("Star screens in More");
  });

  it("offers to hide each screen but Home", () => {
    const html = render();
    expect(html).toContain('aria-label="Hide Convert Games from the sidebar"');
    expect(html).not.toContain('aria-label="Hide Home from the sidebar"');
  });

  it("leaves hidden screens out and says how many, with the way back", async () => {
    const Hidden = await withLists(["/cheats", "/shell"], []);
    const html = render(<Hidden />);
    expect(html).not.toContain('href="/cheats"');
    expect(html).not.toContain('href="/shell"');
    expect(html).toContain('href="/games"');
    expect(html).toContain("2 hidden");
  });

  it("folds a closed section away but keeps its header", async () => {
    const Folded = await withLists([], ["nav_section_diagnostics"]);
    const html = render(<Folded />);
    expect(html).toContain("Diagnostics");
    expect(html).toMatch(/aria-expanded="false"[^>]*>(?:(?!<\/button>).)*Diagnostics/);
    expect(html).not.toContain('href="/logs"');
    expect(html).toContain('href="/games"');
  });
});
