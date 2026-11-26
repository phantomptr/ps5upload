import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

// A device asking to use this PS5, on the helper's own page, the helper's API stood in for.
// Needs `npm run build:console` first. One failed read of /api/mesh (a busy helper, a timeout)
// took the Allow / Decline prompt away until the next good one; only a helper without the route
// (404) means there is nothing to show.

const ORIGIN = "http://console.test";

test.use({ viewport: { width: 1280, height: 800 } });

test("a device's ask stays on Home through a failed read", async ({ page }) => {
  const errors: string[] = [];
  let reads = 0;
  await page.clock.install();
  await page.addInitScript(() => localStorage.setItem("ps5upload.console.tourSeen", "1"));
  page.on("pageerror", (e) => errors.push(e.message));
  const html = readFileSync(resolve(process.cwd(), "dist-console/index.html"), "utf8");
  await page.route(`${ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (!url.pathname.startsWith("/api/")) {
      if (url.pathname === "/" || url.pathname.endsWith(".html")) return route.fulfill({ contentType: "text/html", body: html });
      return route.fulfill({ status: 404, body: "" });
    }
    const json = (body: unknown, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
    if (url.pathname === "/api/token") return json({ token: "test" });
    if (url.pathname === "/api/mesh") {
      reads++;
      // the first read answers; every later one fails as a busy helper does
      return reads === 1 ? json({ devices: [], asks: [{ id: "d1", name: "Ana's phone" }] }) : json({ error: "busy" }, 503);
    }
    return json({});
  });
  await page.goto(`${ORIGIN}/`);
  const ask = page.getByText("Ana's phone", { exact: false });
  await expect(ask).toBeVisible({ timeout: 20_000 });
  await page.clock.runFor(9_000);
  await expect.poll(() => reads).toBeGreaterThan(1);
  await expect(ask).toBeVisible();
  expect(errors).toEqual([]);
});
