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
  it("defaults to labeled navigation with Install Package directly visible", () => {
    const html = renderToStaticMarkup(
      <MemoryRouter initialEntries={["/home"]}>
        <Sidebar />
      </MemoryRouter>,
    );

    expect(html).toContain('data-collapsed="false"');
    expect(html).toContain("Files");
    expect(html).toContain("Install Package");
    expect(html).toContain('href="/install-package"');
  });
});
