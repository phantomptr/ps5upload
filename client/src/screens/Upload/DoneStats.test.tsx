import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { DoneStats } from "./QueuePanel";

describe("DoneStats (finished upload row)", () => {
  it("shows the average speed with a single /s", () => {
    // Seen on Android after a real upload: "556 MiB · 7.53 MiB/s/s avg" — the
    // code appended "/s" and every locale's template appends it again.
    const html = renderToStaticMarkup(
      <DoneStats bytesSent={583_000_000} bytesPerSec={7_900_000} />,
    );
    expect(html).toMatch(/\/s avg/);
    expect(html).not.toMatch(/\/s\/s/);
  });

  it("renders nothing without a measured speed", () => {
    expect(
      renderToStaticMarkup(<DoneStats bytesSent={10} bytesPerSec={0} />),
    ).toBe("");
  });
});
