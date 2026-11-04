import { expect, test, type Page } from "@playwright/test";

// The activity bar: the desktop strip names the job it shows with its figures and a slim bar,
// and opens a panel of running, waiting and finished work; a phone gets a pill above the tab
// bar that opens a bottom sheet inside the viewport. Jobs are put in the task store directly
// (the module the app itself loaded), so no engine or console is needed.

const SHOTS = process.env.ACTIVITY_SHOTS_DIR;

async function boot(page: Page) {
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.0.5");
  });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 503, contentType: "application/json", body: '{"error":"down"}' }),
  );
  await page.goto("/connection", { waitUntil: "domcontentloaded" });
  await page.locator("#main").waitFor();
}

async function addJobs(page: Page) {
  await page.evaluate(async () => {
    // The same module URL the app loaded, so this is the app's own store.
    const url = "/src/state/tasks.ts";
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const mod: any = await import(/* @vite-ignore */ url);
    const s = mod.useTaskStore.getState();
    const a = s.registerTask({
      kind: "pkg-install",
      origin: "e2e",
      label: "Astro's Playroom update with a very long name that has to be cut short",
      consoleId: "192.168.0.5",
    });
    s.updateTask(a, {
      stage: "Sending",
      progress: { current: 3 * 1024 ** 3, total: 12 * 1024 ** 3, unit: "bytes" },
      rate: { bytesPerSec: 48 * 1024 ** 2 },
      eta: 190,
    });
    s.registerTask({ kind: "app-dump", origin: "e2e", label: "Dump Bloodborne", consoleId: "192.168.0.5" });
    const done = s.registerTask({ kind: "fpkg-convert", origin: "e2e", label: "Convert Gravity Rush", consoleId: "" });
    s.finishTask(done, "done");
    const failed = s.registerTask({ kind: "save-backup", origin: "e2e", label: "Save backup PPSA01", consoleId: "" });
    s.finishTask(failed, "failed", { lastError: { code: "X", message: "USB gone", recoverable: false } });
  });
}

async function noSideScroll(page: Page) {
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
}

test("desktop: the strip names the job and opens the activity panel", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await boot(page);
  const toggle = page.locator("[data-activity-toggle]:visible");
  await expect(toggle).toContainText("Nothing running");

  await addJobs(page);
  await expect(toggle).toContainText("Install");
  await expect(page.getByTestId("activity-primary-figures").filter({ visible: true })).toHaveText(
    /25% · 48\.0 MiB\/s · 3m 10s left/,
  );
  await expect(toggle).toContainText("+1");
  await expect(toggle).toContainText("1 failed");

  await toggle.click();
  const sheet = page.getByTestId("activity-sheet");
  await expect(sheet).toBeVisible();
  await expect(sheet).toContainText("Running");
  await expect(sheet).toContainText("3.00 GiB of 12.0 GiB");
  await expect(sheet).toContainText("Dump Bloodborne");
  await expect(sheet).toContainText("Just finished");
  await expect(sheet).toContainText("USB gone");
  // The badge clears once the panel has shown the failure.
  await expect(toggle).not.toContainText("failed");
  const box = await sheet.boundingBox();
  const vp = page.viewportSize()!;
  expect(box!.x + box!.width).toBeLessThanOrEqual(vp.width);
  expect(box!.y + box!.height).toBeLessThanOrEqual(vp.height);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/activity-desktop.png` });

  await page.keyboard.press("Escape");
  await expect(sheet).toBeHidden();

  // A job's row opens the screen that owns it.
  await toggle.click();
  await sheet.getByText("Dump Bloodborne").click();
  await expect(page).toHaveURL(/\/games/);
  await expect(sheet).toBeHidden();
});

test("phone: a pill above the tab bar opens a bottom sheet inside the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 720 });
  await boot(page);
  // Quiet when there is nothing to say.
  await expect(page.getByTestId("activity-pill")).toHaveCount(0);

  await addJobs(page);
  const pill = page.getByTestId("activity-pill");
  await expect(pill).toBeVisible();
  await expect(pill).toContainText("Install");
  await expect(pill).toContainText("25% · 3m 10s left");
  const pb = (await pill.boundingBox())!;
  expect(pb.x).toBeGreaterThanOrEqual(0);
  expect(pb.x + pb.width).toBeLessThanOrEqual(375);
  await noSideScroll(page);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/activity-phone-pill.png` });

  await pill.locator("[data-activity-toggle]").click();
  const sheet = page.getByTestId("activity-sheet");
  await expect(sheet).toBeVisible();
  await expect(sheet).toContainText("Dump Bloodborne");
  // Measured once the slide-in has brought it fully on screen.
  await expect
    .poll(async () => {
      const sb = (await sheet.boundingBox())!;
      return sb.x >= 0 && sb.x + sb.width <= 375 && sb.y + sb.height <= 720 + 1;
    })
    .toBe(true);
  await noSideScroll(page);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/activity-phone-sheet.png` });

  // Tapping outside closes it.
  await page.getByTestId("activity-backdrop").click({ position: { x: 10, y: 10 } });
  await expect(sheet).toBeHidden();
});
