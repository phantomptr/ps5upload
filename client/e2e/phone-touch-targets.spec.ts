import { expect, test, type Page } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// Small things a finger or a screen reader could not get hold of on a phone: the cross that
// closes a tip on Save Data was 14 px wide, and the first crumb of the Files path (an icon) had
// no name.

test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });

async function fakeEngine(page: Page) {
  await seedConsole(page);
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/ps5/status")
      return { ok: true, version: "12.6.31", ucred_elevated: true, ps5_kernel: "releases/10.00", instance_id: 1, command_count: 1 };
    if (path === "/api/ps5/saves/list")
      return {
        saves: [
          { title_id: "PPSA90001", user_id: 0x10000000, path: "/user/home/10000000/savedata_prospero/PPSA90001", size: 1_572_864, mtime: 1_761_800_000, kind: "ps5" },
        ],
      };
    if (path === "/api/ps5/volumes")
      return { volumes: [{ path: "/data", fs_type: "ufs", total_bytes: 825e9, free_bytes: 410e9, writable: true }] };
    if (path === "/api/ps5/list-dir")
      return { entries: [{ name: "notes.txt", kind: "file", size: 2048, mtime: 1_761_800_000 }], truncated: false };
    if (path === "/api/ps5/files/prefs") return { supported: true, favorites: [], recent: [], last: null };
    return undefined;
  });
}

test("Save Data: a tip's close button is wide enough for a finger", async ({ page }) => {
  await fakeEngine(page);
  await page.goto("/saves", { waitUntil: "domcontentloaded" });
  const main = page.locator("[data-scroll-root]:visible");
  const close = main.getByRole("button", { name: "Dismiss" }).first();
  await expect(close).toBeVisible({ timeout: 60_000 });
  const box = (await close.boundingBox())!;
  expect(box.width).toBeGreaterThanOrEqual(40);
  expect(box.height).toBeGreaterThanOrEqual(40);
  // Still inside its card and the screen.
  expect(box.x + box.width).toBeLessThanOrEqual(360);
  expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(0);
});

test("Files: the first crumb of the path has a name", async ({ page }) => {
  await fakeEngine(page);
  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const header = page.locator("[data-scroll-root]:visible").getByTestId("fs-header");
  await expect(header).toBeVisible({ timeout: 60_000 });
  await expect(header.getByRole("button", { name: "/", exact: true })).toBeVisible();
});
