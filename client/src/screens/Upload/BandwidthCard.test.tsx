import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { BandwidthCard } from "./index";

// The speed limit box on the Upload screen. With no limit it held "0" (Settings holds nothing):
// React writes that 0 back into an empty number box, and a "." typed alone reads as empty, so
// typing ".5" over a limit left "05" in the box and set 5 MiB/s, ten times what was typed.
describe("the Upload screen's speed limit box", () => {
  it("is empty with no limit, as the one in Settings, so React puts no 0 back while typing", () => {
    const html = renderToStaticMarkup(<BandwidthCard />);
    const input = /<input[^>]*id="upload-bw-cap"[^>]*>/.exec(html)?.[0] ?? "";
    expect(input).toContain('type="number"');
    expect(input).toContain('value=""');
    expect(input).toContain('placeholder="0"');
    // Its label names it.
    expect(html).toContain('for="upload-bw-cap"');
  });
});
