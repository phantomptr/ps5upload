import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import GameFpsPanel from "./GameFpsPanel";

// The first render, before any reply: the panel is there, the readout is off until the user turns
// it on, a capture can be started and there is nothing to export yet.
describe("GameFpsPanel", () => {
  it("renders with the live readout off and nothing captured", () => {
    const html = renderToStaticMarkup(<GameFpsPanel host="192.0.2.5" />);
    expect(html).toContain("Game frame rate");
    expect(html).toContain("Live readout");
    expect(html).toContain('aria-checked="false"');
    expect(html).toContain("Start capture");
    expect(html).toMatch(/<button[^>]*disabled[^>]*>(?:(?!<\/button>).)*Export CSV/s);
    expect(html).not.toContain("FPS (sampled)");
    expect(html).toContain("not on the TV");
  });
});
