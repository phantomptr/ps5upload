import { expect, test } from "@playwright/test";

// #856: with several PS5s the console strip at the top scrolls sideways, and each tab's
// screen-reader label (absolutely placed) sat at its unscrolled spot, far past the right edge.
// The page grew wider than the screen, and Android's WebView grew its viewport to match: the tab
// bar went off the bottom and the app was drawn shrunk into the top-left corner.
test("phone: many consoles keep the page as wide as the screen", async ({ page }) => {
  await page.addInitScript(() => {
    const hosts = ["192.168.68.101", "192.168.68.102", "192.168.68.103", "192.168.68.104"];
    const profiles = hosts.map((host, i) => ({ id: `c${i}`, name: `PS5 (${host})`, host }));
    profiles.push({ id: "c9", name: "1", host: "192.168.68.110" });
    window.localStorage.setItem("ps5upload.roster.v1", JSON.stringify({ profiles, active_id: "c9" }));
  });
  await page.setViewportSize({ width: 412, height: 900 });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.locator("[data-console-id]:visible")).toHaveCount(5, { timeout: 30_000 });
  const wider = () =>
    page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  await expect.poll(wider).toBeLessThanOrEqual(0);
  const nav = await page.locator("[data-bottom-nav]:visible").boundingBox();
  expect(nav).not.toBeNull();
  expect(nav!.y + nav!.height).toBeLessThanOrEqual(900);
});
