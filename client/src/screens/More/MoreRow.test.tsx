import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () => (key: string, vars?: Record<string, string | number>, fallback?: string) => {
    let out = fallback ?? key;
    for (const [k, v] of Object.entries(vars ?? {})) out = out.replace(`{${k}}`, String(v));
    return out;
  },
}));
const store = vi.hoisted(() => ({ hidden: ["/cheats"], pinned: [] as string[] }));
vi.mock("../../state/navSidebar", () => ({
  useNavSidebarStore: (selector: (s: Record<string, unknown>) => unknown) =>
    selector({
      hidden: store.hidden,
      pinned: store.pinned,
      toggleHidden: () => {},
      togglePinned: () => {},
      movePinned: () => {},
    }),
}));
vi.mock("../../layout/RosterPicker", () => ({ default: () => null }));
vi.mock("../../layout/NotificationInbox", () => ({ default: () => null }));

import { NAV_ITEMS } from "../../layout/navItems";
import MoreScreen, { MoreRow } from "./index";

const row = (to: string, order?: { first: boolean; last: boolean }) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <ul>
        <MoreRow
          item={NAV_ITEMS.find((i) => i.to === to)!}
          errorCount={0}
          updateAvailable={false}
          order={order}
        />
      </ul>
    </MemoryRouter>,
  );

const button = (html: string, label: string) =>
  html.match(new RegExp(`<button[^>]*aria-label="${label}"[^>]*>`))?.[0];

beforeEach(() => {
  store.hidden = ["/cheats"];
  store.pinned = [];
});

describe("More's sidebar switch", () => {
  it("shows a hidden screen as hidden, with the way to bring it back", () => {
    const out = row("/cheats");
    expect(button(out, "Show [^\"]+ in the sidebar")).toContain('aria-pressed="false"');
    expect(out).toMatch(/aria-label="Show [^"]+ in the sidebar"/);
    expect(out).toContain("Hidden from the sidebar");
  });

  it("shows a visible screen as shown, with the way to hide it", () => {
    const out = row("/games");
    expect(button(out, "Hide [^\"]+ from the sidebar")).toContain('aria-pressed="true"');
    expect(out).toMatch(/aria-label="Hide [^"]+ from the sidebar"/);
    expect(out).not.toContain("Hidden from the sidebar");
  });

  it("offers it only where there is a sidebar", () => {
    // Phones navigate with tabs and this list; the switch would change nothing there.
    expect(row("/games")).toMatch(/<button[^>]*class="[^"]*\bhidden\b[^"]*\bmd:flex\b/);
  });
});

describe("More's pin switch", () => {
  it("pins from every row, on phones too", () => {
    const pin = button(row("/files"), "Pin Files");
    expect(pin).toContain('aria-pressed="false"');
    // Not desktop-only like the hide switch: Pinned shows on this screen as well.
    expect(pin).not.toMatch(/\bhidden\b/);
    expect(pin).not.toContain('disabled=""');
  });

  it("unpins a pinned row", () => {
    store.pinned = ["/files"];
    expect(button(row("/files"), "Unpin Files")).toContain('aria-pressed="true"');
  });

  it("can't pin a thirteenth screen, but can still unpin", () => {
    store.pinned = NAV_ITEMS.filter((i) => i.to !== "/files" && !i.beta).slice(0, 12).map((i) => i.to);
    const pin = button(row("/files"), "Pin Files");
    expect(pin).toContain('disabled=""');
    expect(pin).toContain('title="You can pin up to 12 screens"');
    expect(button(row(store.pinned[0]), "Unpin [^\"]+")).not.toContain('disabled=""');
  });

  it("moves a pinned row up and down, not past the ends", () => {
    store.pinned = ["/files", "/send"];
    const first = row("/files", { first: true, last: false });
    expect(button(first, "Move Files up")).toContain('disabled=""');
    expect(button(first, "Move Files down")).not.toContain('disabled=""');
    // The Pinned group's rows have no hide switch.
    expect(first).not.toContain("from the sidebar");
    const last = row("/send", { first: false, last: true });
    expect(button(last, "Move [^\"]+ up")).not.toContain('disabled=""');
    expect(button(last, "Move [^\"]+ down")).toContain('disabled=""');
    // Arrows take the chevron's place while editing.
    expect(first).not.toContain("lucide-chevron-right");
    // Not editing (and outside the Pinned group): no move buttons, the chevron is back.
    expect(row("/files")).not.toContain("Move Files");
    expect(row("/files")).toContain("lucide-chevron-right");
  });
});

describe("More's Pinned group", () => {
  const screen = () => renderToStaticMarkup(<MemoryRouter><MoreScreen /></MemoryRouter>);

  it("is absent until something is pinned", () => {
    expect(screen()).not.toContain('data-testid="more-pinned"');
  });

  it("lists pins in pin order under Bug report, and keeps them in their sections", () => {
    store.pinned = ["/processes", "/files"];
    const html = screen();
    const at = html.indexOf('data-testid="more-pinned"');
    expect(at).toBeGreaterThan(html.indexOf('href="/bug-report"'));
    const group = html.slice(at, html.indexOf("</section>", at));
    expect(group.indexOf('href="/processes"')).toBeLessThan(group.indexOf('href="/files"'));
    // Until Edit: plain rows (name, chevron, pin), no arrows.
    expect(group).toMatch(/<button[^>]*aria-pressed="false"[^>]*>Edit<\/button>/);
    expect(group).not.toContain("Move ");
    expect(group).toContain("lucide-chevron-right");
    expect(html.split('href="/files"').length - 1).toBe(2);
  });
});
