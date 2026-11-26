import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

// The helper's own page opened from a phone over the LAN, the helper's API stood in for. Needs
// `npm run build:console` first.

const ORIGIN = "http://console.test";
const G = 1024 ** 3;

test.use({ viewport: { width: 360, height: 800 }, isMobile: true, hasTouch: true });

const BODIES: Record<string, unknown> = {
  "/api/token": { token: "test" },
  "/api/status": { version: "12.6.32", firmware: "13.60", model: "CFI-1215A", temp_c: 54, running: null, tz_min: 0 },
  "/api/volumes": {
    volumes: [
      { path: "/data", mount_from: "/dev/ssd0.user", total_bytes: 800 * G, free_bytes: 163 * G, allocatable_bytes: 161 * G, writable: true },
      { path: "/mnt/usb0", mount_from: "/dev/da0s1", total_bytes: 2000 * G, free_bytes: 1200 * G, allocatable_bytes: 1200 * G, writable: true },
    ],
  },
  // A running copy with a long name: its row is wider than the phone.
  "/api/jobs": {
    jobs: [
      {
        job: "a".repeat(32),
        kind: "copy",
        state: "running",
        files_done: 3,
        files_total: 10,
        bytes_done: 4 * G,
        bytes_total: 12 * G,
        mine: true,
        name: "/data/homebrew/A-Rather-Long-Folder-Name/AnotherLongGameFileName.pkg",
      },
    ],
    uploading: [],
  },
  "/api/fs/list": { entries: [], path: "/data", truncated: false },
};

test("a running task's long name does not push the page past a phone's edge; the sections are along the bottom", async ({ page }) => {
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
    return route.fulfill({ contentType: "application/json", body: JSON.stringify(BODIES[url.pathname] ?? {}) });
  });
  await page.goto(`${ORIGIN}/`);
  await expect(page.getByText("From this console")).toBeVisible({ timeout: 20_000 });
  for (const section of ["home", "files", "games", "saves", "settings"]) {
    await page.locator(`[data-rail-id="${section}"]`).click();
    await page.waitForTimeout(300);
    const widths = await page.evaluate(() => {
      const content = document.querySelector(".cw-content");
      return { page: document.documentElement.scrollWidth, content: content ? content.getBoundingClientRect().right : 0 };
    });
    expect(widths, section).toEqual({ page: 360, content: 360 });
  }
  // The sections in a bar along the bottom (the rail took a fifth of the width), no controller hints.
  const rail = (await page.locator(".cw-rail").boundingBox())!;
  expect(rail.width).toBe(360);
  expect(rail.y + rail.height).toBeGreaterThan(790);
  await expect(page.locator(".cw-hints")).toBeHidden();
  expect(errors).toEqual([]);
});
