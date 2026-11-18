import { expect, test } from "@playwright/test";

// A Windows user found uploads held to 1.5 MB/s with no idea why: the speed limit box steps by 0.5,
// and in Chromium (the Windows app's WebView2) a mouse wheel over the focused box changes it.
// Three notches while scrolling Settings set 1.5. The wheel must scroll, not change the limit.

test("the mouse wheel over the focused speed limit box leaves it unchanged", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 700 });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => {
      const path = new URL(route.request().url()).pathname;
      if (["/api/consoles", "/api/mesh/", "/api/shared/"].some((p) => path.startsWith(p))) {
        return route.fulfill({ status: 404, contentType: "application/json", body: '{"error":"not found"}' });
      }
      return route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
    },
  );
  await page.goto("/settings", { waitUntil: "domcontentloaded" });
  const box = page.locator("#bw-cap:visible");
  await box.scrollIntoViewIfNeeded({ timeout: 30_000 });
  await box.click();
  await expect(box).toBeFocused();

  const b = (await box.boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  for (let i = 0; i < 3; i++) await page.mouse.wheel(0, -100);

  await expect(box).not.toBeFocused();
  await expect(box).toHaveValue("");
  expect(await page.evaluate(() => window.localStorage.getItem("ps5upload.bandwidth_cap_mbps"))).toBeNull();

  // Typing still sets it, and the arrow keys still step it.
  await box.click();
  await box.fill("2");
  await box.press("ArrowUp");
  await expect(box).toHaveValue("2.5");
});
