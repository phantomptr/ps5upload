// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { NAV_ITEMS } from "../../layout/navItems";

// The sidebar said "FW Spoof" and the screen "Firmware Spoof Detection".
describe("Firmware spoof check naming", () => {
  it("uses the same key and English name in the nav and on the screen", () => {
    const nav = NAV_ITEMS.find((i) => i.to === "/fw-spoof");
    const screen: string = readFileSync(new URL("./index.tsx", import.meta.url).pathname, "utf8");
    expect(nav).toBeDefined();
    expect(screen).toContain(`tr("${nav!.key}", undefined, "${nav!.fallback}")`);
  });
});
