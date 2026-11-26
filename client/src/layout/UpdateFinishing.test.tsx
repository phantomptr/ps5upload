import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import { useUpdateStore } from "../state/update";
import { UpdateFinishing, UpdateFinishingCard } from "./UpdateFinishing";

afterEach(() => {
  useUpdateStore.setState({ finishingUpdate: false });
});

describe("UpdateFinishing", () => {
  it("shows nothing unless an install is being waited for", () => {
    expect(renderToStaticMarkup(<UpdateFinishing />)).toBe("");
  });

  it("says the update is finishing while the app waits for the installer", () => {
    const html = renderToStaticMarkup(<UpdateFinishingCard />);
    expect(html).toContain('data-testid="update-finishing"');
    expect(html).toContain('role="status"');
    expect(html).toContain("Finishing the update…");
    expect(html).toContain("starts again by itself");
    // Not a failure: nothing here says the update did not install.
    expect(html).not.toMatch(/fail|didn/i);
  });
});
