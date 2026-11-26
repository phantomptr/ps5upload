import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { HelperUpdateNoticeView, type HelperUpdateNoticeViewProps } from "./HelperUpdatePrompt";

const view = (patch: Partial<HelperUpdateNoticeViewProps> = {}) =>
  renderToStaticMarkup(
    <HelperUpdateNoticeView
      view={{ host: "192.168.1.20", from: "12.6.30", to: "12.6.31" }}
      hold={null}
      send={null}
      onUpdate={() => {}}
      onDismiss={() => {}}
      {...patch}
    />,
  );

describe("Home's notice for an older helper (#941)", () => {
  it("says the helper is out of date, with both versions, one button and a way to dismiss it", () => {
    const html = view();
    expect(html).toContain("The PS5 helper is out of date");
    expect(html).toContain("This PS5 runs helper v12.6.30. This app is v12.6.31.");
    expect(html).toContain("Update helper");
    expect(html).toContain('data-testid="helper-update-now"');
    expect(html).toContain('aria-label="Dismiss"');
  });

  it("says why a press did not send", () => {
    const html = view({ hold: "transfer" });
    expect(html).toContain("A transfer or an install is running on this PS5");
    expect(html).toContain("Update the helper when it has finished.");
  });

  it("shows the send's step while it runs, with the button busy, and its failure after", () => {
    const busy = view({ send: { state: "busy", phase: null, msg: "Starting the helper…", startedAtMs: 1 }, hold: "transfer" });
    expect(busy).toContain("Starting the helper…");
    expect(busy).toMatch(/<button[^>]*disabled[^>]*data-testid="helper-update-now"|data-testid="helper-update-now"[^>]*disabled/);
    // The hold from an earlier press is not shown over a send that is running.
    expect(busy).not.toContain("A transfer or an install is running");
    const failed = view({ send: { state: "fail", phase: null, msg: "An install is running on the PS5.", startedAtMs: 1 } });
    expect(failed).toContain("An install is running on the PS5.");
  });

  it("is nothing when there is nothing to say", () => {
    expect(view({ view: null })).toBe("");
  });

  it("never names another tool", () => {
    expect(view()).not.toMatch(/payload|elfldr|loader/i);
  });
});
