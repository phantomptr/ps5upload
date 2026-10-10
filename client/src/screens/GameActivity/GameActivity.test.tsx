import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// A connected console, so the screen renders past its connection gate.
vi.mock("../../state/connection", async (orig) => {
  const real = await orig<typeof import("../../state/connection")>();
  const state = { host: "192.168.1.100", payloadStatus: "up", engineStatus: "up" };
  const hook = Object.assign(
    (sel?: (s: typeof state) => unknown) => (sel ? sel(state) : state),
    { getState: () => state, subscribe: () => () => {}, setState: () => {} },
  );
  return { ...real, useConnectionStore: hook };
});

import GameActivityScreen from "./index";

const html = () =>
  renderToStaticMarkup(
    <MemoryRouter>
      <GameActivityScreen />
    </MemoryRouter>,
  );

describe("Game Activity", () => {
  it("has the tracked and console tabs, not the title-id-ordered Recently Played", () => {
    const out = html();
    expect(out).toContain("Tracked Playtime");
    expect(out).toContain("Console Play Time");
    expect(out).not.toContain("Recently Played");
  });

  it("says where each tab's numbers come from", () => {
    expect(html()).toContain("counted by the ps5upload helper");
  });

  it("offers Reset on the tracked tab it clears", () => {
    expect(html()).toContain("Reset play time");
  });
});
