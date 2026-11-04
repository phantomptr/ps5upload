import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

const platform = { mobile: true };
vi.mock("../lib/platform", async (orig) => ({
  ...(await orig<typeof import("../lib/platform")>()),
  isMobile: () => platform.mobile,
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true, safeUnlisten: () => {} }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => new Promise(() => {})) }));

import { EngineProblem } from "./EngineProblem";

function render(): string {
  return renderToStaticMarkup(
    <MemoryRouter>
      <EngineProblem />
    </MemoryRouter>,
  );
}

afterEach(() => {
  platform.mobile = true;
});

describe("the engine problem card", () => {
  it("on a phone says to reopen the app, with no computer advice and no Restart", () => {
    const html = render();
    expect(html).toContain("Close the app fully and open it again.");
    expect(html).not.toContain("on this computer");
    expect(html).not.toContain("Restart engine");
  });

  it("keeps Restart on the desktop", () => {
    platform.mobile = false;
    const html = render();
    expect(html).toContain("on this computer");
    expect(html).toContain("Restart engine");
  });
});
