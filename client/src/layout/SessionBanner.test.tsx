import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { SessionBannerView } from "./SessionBanner";

const view = (over: Partial<Parameters<typeof SessionBannerView>[0]>) =>
  renderToStaticMarkup(
    <SessionBannerView
      session={null}
      wedged={false}
      busy={false}
      error={null}
      onPair={() => {}}
      onUpdate={() => {}}
      {...over}
    />,
  );

describe("session banner", () => {
  it("offers Pair when the console has not accepted this app", () => {
    const html = view({ session: "needs_pairing" });
    expect(html).toContain("has not accepted this app yet");
    expect(html).toContain("Pair…");
  });

  it("offers a one-click update for an older helper", () => {
    const html = view({ session: "helper_old" });
    expect(html).toContain("This PS5 is running an older helper. Update it.");
    expect(html).toContain("Update helper");
  });

  it("says to restart the console when the old helper would not exit, with no update button", () => {
    const html = view({ session: "helper_old", wedged: true });
    expect(html).toContain("did not exit");
    expect(html).not.toContain("Update helper");
  });

  it("shows why an update could not run", () => {
    const html = view({ session: "helper_old", error: "Wait a minute." });
    expect(html).toContain("Wait a minute.");
    expect(html).toContain("Update helper");
  });

  it("is silent when connected, down, or not probed yet", () => {
    for (const s of ["connected", "down", null] as const) {
      expect(view({ session: s })).toBe("");
    }
  });
});
