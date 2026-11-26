import { expect, test, type Page } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// Upload & Install's folded sections ("From a Link or an Archive · Install from a URL, …"): on a
// 360 px phone the title shared its row with the summary and broke into three lines. There the
// summary goes under the title; wider windows keep the two side by side.

async function open(page: Page) {
  await seedConsole(page);
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/ps5/status") return { ok: true, version: "12.6.31", ucred_elevated: true };
    if (path === "/api/ps5/volumes") return { volumes: [] };
    return undefined;
  });
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  const text = page.getByTestId("fold-send-more").getByTestId("fold-heading-text");
  await expect(text).toBeVisible({ timeout: 60_000 });
  const title = text.locator("> span").first();
  const summary = text.locator("> span").nth(1);
  await expect(title).toHaveText("From a Link or an Archive");
  return { title: (await title.boundingBox())!, summary: (await summary.boundingBox())! };
}

test.describe("phone", () => {
  test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });
  test("the title stays on one line, its summary under it", async ({ page }) => {
    const { title, summary } = await open(page);
    expect(title.height).toBeLessThan(30);
    expect(summary.y).toBeGreaterThanOrEqual(title.y + title.height - 1);
    expect(summary.x + summary.width).toBeLessThanOrEqual(360);
  });
});

test.describe("computer", () => {
  test.use({ viewport: { width: 1280, height: 800 } });
  test("the summary stays beside the title", async ({ page }) => {
    const { title, summary } = await open(page);
    expect(title.height).toBeLessThan(30);
    expect(summary.x).toBeGreaterThan(title.x + title.width);
    expect(Math.abs(summary.y + summary.height - (title.y + title.height))).toBeLessThan(8);
  });
});
