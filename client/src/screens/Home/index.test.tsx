import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr:
    () =>
    (key: string, vars?: Record<string, string | number> | string, fallback?: string) =>
      typeof vars === "string" ? vars : (fallback ?? key),
}));
vi.mock("./ServersCard", () => ({ ServersCard: () => <div data-servers-card /> }));
vi.mock("./HealthCard", () => ({ HealthCard: () => null }));
vi.mock("../Connection/PowerControl", () => ({ default: () => null }));
vi.mock("../../state/sensors", () => ({ useSensors: () => ({ sample: null, history: [] }) }));

import HomeScreen from "./index";

const html = () =>
  renderToStaticMarkup(
    <MemoryRouter>
      <HomeScreen />
    </MemoryRouter>,
  );

describe("Home", () => {
  it("shows the servers card exactly once, outside the empty-state boxes", () => {
    // With no activity and no notifications, both cards show their empty state; the
    // servers card used to ride inside that box and so appeared twice.
    const out = html();
    expect(out.match(/data-servers-card/g)?.length).toBe(1);
  });

  it("has no separate 'Connect a server' tile next to the servers card", () => {
    expect(html()).not.toContain("Connect a server");
  });

  it("links recent activity to /tasks, not the /activity redirect", () => {
    const out = html();
    expect(out).toContain('href="/tasks"');
    expect(out).not.toContain('href="/activity"');
  });
});
