import { expect, test, type Page } from "@playwright/test";

// The bell is a peek at the same list the Notifications screen's "This app" tab shows,
// and on a phone its panel is a sheet that stays inside the screen.

const LONG = "UP0000-CUSA07842_00-JAKXCOMBATRACING-A0100-V0100-WITH-A-VERY-LONG-UNBROKEN-NAME.pkg";

async function seed(page: Page) {
  await page.addInitScript((long) => {
    const now = Date.now();
    const entries = Array.from({ length: 12 }, (_, i) => ({
      id: `n${i}`,
      ts: now - i * 60_000,
      level: i % 3 === 0 ? "error" : i % 3 === 1 ? "success" : "info",
      title: i === 0 ? `Install failed: ${long}` : `Upload ${i} finished`,
      body: i === 0 ? `Sony refused ${long} (0x80B2116F)` : undefined,
      link: i === 1 ? "/library" : undefined,
      read: i >= 3,
    }));
    window.localStorage.setItem("ps5upload.notifications.v1", JSON.stringify(entries));
    // One PS5 saved: with none, the app opens "Add a PS5" on its own, over the bell.
    if (!window.localStorage.getItem("ps5upload.roster.v1"))
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({ profiles: [{ id: "console-a", name: "PS5", host: "192.168.0.5" }], active_id: "console-a" }),
      );
  }, LONG);
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 503, contentType: "application/json", body: "{\"error\":\"down\"}" }),
  );
}

async function expectInside(page: Page, box: { x: number; y: number; width: number; height: number } | null) {
  if (!box) throw new Error("no box");
  const vp = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(vp.width);
  expect(box.y + box.height).toBeLessThanOrEqual(vp.height);
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
}

for (const [name, width, height] of [
  ["phone", 390, 844],
  ["small phone", 360, 640],
  ["desktop", 1280, 800],
] as const)
  test(`bell panel stays inside the window and See all opens the same list (${name})`, async ({
    page,
  }, info) => {
    await page.setViewportSize({ width, height });
    await seed(page);
    await page.goto("/", { waitUntil: "domcontentloaded" });

    const bell = page.getByRole("button", { name: "Open notifications" }).locator("visible=true");
    await expect(bell).toHaveCount(1, { timeout: 30_000 });
    await expect(bell.getByTestId("notifications-badge")).toHaveText("3");
    await bell.click();

    const panel = page.getByTestId("notifications-panel").locator("visible=true");
    await expect(panel).toBeVisible();
    // The newest few, not all twelve; the long name wraps instead of widening the panel.
    await expect(panel.getByRole("listitem")).toHaveCount(8);
    await expect(panel.getByText(/Install failed/)).toBeVisible();
    await expectInside(page, await panel.boundingBox());
    await page.screenshot({ path: info.outputPath(`bell-${width}.png`) });

    // Opening the bell marks the list read; the new ones keep their dot while it is open.
    await expect(bell.getByTestId("notifications-badge")).toHaveCount(0);
    await expect(panel.locator("li[data-new]")).toHaveCount(3);

    // The list scrolls inside the panel: its last row's footer link is reachable.
    const seeAll = panel.getByRole("button", { name: "See all 12" });
    await expect(seeAll).toBeVisible();
    await expectInside(page, await seeAll.boundingBox());
    await seeAll.click();

    await expect(page).toHaveURL(/\/notifications$/);
    await expect(panel).toHaveCount(0);
    const main = page.getByRole("main");
    await expect(main.getByRole("tab", { name: /This app/ })).toHaveAttribute("aria-selected", "true");
    await expect(main.getByTestId("notifications-app-list").getByRole("listitem")).toHaveCount(12);
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
    await page.screenshot({ path: info.outputPath(`screen-${width}.png`), fullPage: false });

    await main.getByRole("tab", { name: /On the PS5/ }).click();
    await expect(page).toHaveURL(/tab=ps5/);
    await main.getByRole("tab", { name: /Settings/ }).click();
    await expect(main.getByText("7 days")).toBeVisible();
  });
