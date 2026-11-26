import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

// Home's "USB drive connected" → Find games on the helper's own page, the helper's API stood in
// for. Needs `npm run build:console` first.
// - The section opened scanning showed "Reading the installed games…" through and after the scan (the scan was its
//   first load, and only a load counted as one).
// - Switching to Installed and back scanned every source again, on every switch.

const PS5_AGENT =
  "Mozilla/5.0 (PlayStation; PlayStation 5/13.60) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.4 Safari/605.1.15";
const ORIGIN = "http://console.test";
const G = 1024 ** 3;

test.use({ userAgent: PS5_AGENT, viewport: { width: 1920, height: 1080 } });

const DATA = { path: "/data", mount_from: "/dev/ssd0.user", total_bytes: 800 * G, free_bytes: 163 * G, writable: true };
const USB = { path: "/mnt/usb0", mount_from: "/dev/da0s1", total_bytes: 64 * G, free_bytes: 20 * G, writable: true };
const SCAN = {
  source: "console",
  scanned: 1764000000,
  complete: true,
  stopped: null,
  entries: 3,
  items: [{ kind: "folder", path: "/mnt/usb0/homebrew/PPSA01650", name: "PPSA01650", title: "Astro Bot", title_id: "PPSA01650", content_id: "", version: "01.000", category: "gd", type: "game", platform: "ps5", fw: "", fake: false, size: 0, icon: "" }],
};

test("Find games on a new USB drive shows what it found, and scans once", async ({ page }) => {
  const errors: string[] = [];
  let usb = false;
  let scans = 0;
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
    switch (url.pathname) {
      case "/api/token":
        return json({ token: "test" });
      case "/api/volumes":
        return json({ volumes: usb ? [DATA, USB] : [DATA] });
      case "/api/smb/shares":
        return json({ shares: [] });
      case "/api/library/scan":
        scans++;
        return json({ job: "j1" });
      case "/api/smb/job":
        return json({ job: "j1", kind: "download", state: "done", bytes_done: 0, bytes_total: 0, files_done: 3, files_total: 3, scanning: false, current: "", error: null, failed: "", reconnects: 0 });
      case "/api/fs/read":
        return scans ? route.fulfill({ contentType: "application/octet-stream", body: JSON.stringify(SCAN) }) : json({ error: "not_found" }, 404);
      default:
        return json({});
    }
  });
  await page.goto(`${ORIGIN}/`);
  await expect(page.locator('[data-rail-id="home"]')).toBeVisible({ timeout: 20_000 });
  await page.clock.runFor(2_000);
  usb = true;
  // Home reads the drives again every 15 s.
  await page.clock.runFor(16_000);
  await page.locator('[data-action="usb-find-games"]').click({ timeout: 10_000 });
  for (let i = 0; i < 10 && !(await page.locator("[data-lib-key]").count()); i++) await page.clock.runFor(1_500);
  await expect(page.locator("[data-lib-key]")).toHaveCount(1);
  await expect(page.getByText("Reading the installed games…")).toHaveCount(0);
  expect(scans).toBe(1);

  await page.locator('[data-action="games-view-installed"]').click();
  await page.locator('[data-action="games-view-library"]').click();
  await page.clock.runFor(3_000);
  await expect(page.locator("[data-lib-key]")).toHaveCount(1);
  expect(scans).toBe(1);
  expect(errors).toEqual([]);
});
