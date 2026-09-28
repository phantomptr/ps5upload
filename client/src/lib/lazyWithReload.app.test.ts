import { describe, expect, it } from "vitest";

// A bare React.lazy screen shows the crash screen when its code can't be fetched after an update.
const APP = Object.values(
  import.meta.glob("../App.tsx", { query: "?raw", import: "default", eager: true }) as Record<string, string>,
)[0];

describe("screens loaded on demand", () => {
  it("all go through lazyWithReload", () => {
    expect(APP.length).toBeGreaterThan(1000);
    expect(APP).not.toMatch(/(?<![A-Za-z])lazy\(\s*\(\)\s*=>\s*import/);
    expect((APP.match(/lazyWithReload\(\(\) => import\(/g) ?? []).length).toBeGreaterThan(30);
  });
});
