import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { TouchModeToggle } from "./TouchpadOverlay";

// No DOM here (vitest runs in node): static markup.
const html = (hidden?: boolean) =>
  renderToStaticMarkup(<TouchModeToggle mode="controller" onChange={() => {}} blocked={null} hidden={hidden} />);

describe("the Controller/Touchpad switch", () => {
  it("shows and takes taps by default (outside full screen)", () => {
    const out = html();
    expect(out).toContain("pointer-events-auto opacity-100");
    expect(out).not.toMatch(/role="radiogroup"[^>]*aria-hidden/);
  });

  it("hides with the stream's controls in full screen, out of the way of the picture (#722)", () => {
    const out = html(true);
    expect(out).toContain("pointer-events-none opacity-0");
    expect(out).toMatch(/role="radiogroup"[^>]*aria-hidden="true"/);
  });
});
