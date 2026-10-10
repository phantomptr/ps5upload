import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { SessionBannerView } from "./SessionBanner";

const view = (over: Partial<Parameters<typeof SessionBannerView>[0]>) =>
  renderToStaticMarkup(
    <SessionBannerView session={null} onPair={() => {}} {...over} />,
  );

describe("session banner", () => {
  it("offers Pair when the console has not accepted this app", () => {
    const html = view({ session: "needs_pairing" });
    expect(html).toContain("has not accepted this app yet");
    expect(html).toContain("Pair…");
  });

  it("is silent when connected, down, or not probed yet", () => {
    for (const s of ["connected", "down", null] as const) {
      expect(view({ session: s })).toBe("");
    }
  });
});
