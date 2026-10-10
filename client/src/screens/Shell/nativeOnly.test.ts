// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { NAV_ITEMS } from "../../layout/navItems";

const app: string = readFileSync(new URL("../../App.tsx", import.meta.url).pathname, "utf8");

// shell_run_cmd has no browser mapping, so the screen can do nothing there.
describe("Shell outside the desktop app", () => {
  it("is hidden from the browser build's nav", () => {
    expect(NAV_ITEMS.find((i) => i.to === "/shell")?.hideInBrowser).toBe(true);
  });

  it("is guarded by NativeOnlyRoute", () => {
    expect(app).toMatch(/path="\/shell"\s*element=\{\s*<NativeOnlyRoute>/);
  });
});

// The console takes a single fan target; the curve editor had nothing to drive.
describe("Fan Curve", () => {
  it("redirects to the Console screen and is not in the nav", () => {
    expect(app).toContain('<Route path="/fan-curve" element={<Navigate to="/console" replace />} />');
    expect(NAV_ITEMS.some((i) => i.to === "/fan-curve")).toBe(false);
  });
});
