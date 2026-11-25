import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

// #928 on the helper's own page: the PS5's browser reads UTC whatever zone the console is set to,
// so a save backed up at 07:25:59 in Recife (UTC-3) was listed as 10:25:59. The helper now says the
// console's zone (/api/status tz_min) and the page shows times with it. A real browser with the
// PS5's user agent and a UTC clock, the helper's API stood in for. Needs `npm run build:console`.

const PS5_AGENT =
  "Mozilla/5.0 (PlayStation; PlayStation 5/13.60) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.4 Safari/605.1.15";
const ORIGIN = "http://console.test";
const USER = "1a2b3c4d";
const TITLE = "PPSA01650";

test.use({ userAgent: PS5_AGENT, viewport: { width: 1920, height: 1080 }, timezoneId: "UTC" });

const dir = (name: string) => ({ name, kind: "dir", size: 0, mtime: 0 });

/** The folders the Saves section reads: one user, one game with a save, two backups. */
const FOLDERS: Record<string, ReturnType<typeof dir>[]> = {
  "/user/home": [dir(USER)],
  [`/user/home/${USER}/savedata_prospero`]: [dir(TITLE)],
  [`/data/ps5upload/save-backups/${USER}/${TITLE}`]: [
    dir("2026-11-25_10-25-59"), // made by this browser before the fix: named in UTC, unmarked
    dir("2026-11-25_15-00-00Z"), // made since: UTC, marked
  ],
};

test("save backups show the console's time, not UTC", async ({ page }) => {
  const errors: string[] = [];
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
    if (url.pathname === "/api/status")
      return json({ version: "12.6.31", firmware: "13.60", model: "CFI-1215A", temp_c: 50, running: null, tz_min: -180 });
    if (url.pathname === "/api/users") return json({ users: [{ id: 0x1a2b3c4d, name: "Ana", foreground: true }] });
    if (url.pathname === "/api/volumes") return json({ volumes: [] });
    if (url.pathname === "/api/fs/list") {
      const path = (route.request().postDataJSON() as { path?: string } | null)?.path ?? url.searchParams.get("path") ?? "";
      const entries = FOLDERS[path];
      return entries ? json({ entries, path, truncated: false }) : json({ error: "not_found" }, 404);
    }
    return json({});
  });
  await page.goto(`${ORIGIN}/`);
  // The browser itself has no zone, as on the PS5.
  expect(await page.evaluate(() => new Date(Date.UTC(2026, 10, 25)).getTimezoneOffset())).toBe(0);

  await page.locator('[data-rail-id="saves"]').click();
  await page.locator(`[data-save-key="${USER}/${TITLE}"]`).click({ timeout: 20_000 });
  const old = page.locator('[data-backup="2026-11-25_10-25-59"]');
  await expect(old).toContainText("11/25/2026, 7:25:59 AM");
  await expect(old).not.toContainText("10:25:59");
  await expect(page.locator('[data-backup="2026-11-25_15-00-00Z"]')).toContainText("11/25/2026, 12:00:00 PM");
  expect(errors).toEqual([]);
});
