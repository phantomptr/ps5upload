import { expect, test } from "@playwright/test";
import { routeEngine, seedConsole } from "./fixtures/engine";

// Backup & Restore (#707), on a mocked engine: the checklist with its sizes, a save with nowhere to
// go yet said plainly, and the guide for a PS5 that won't start.

const status = (over: Record<string, unknown> = {}) => ({
  state: "done",
  kind: "restoring",
  host: "192.168.1.100",
  current: "",
  step: 3,
  steps: 3,
  bytes: 0,
  total_bytes: 0,
  dir: "",
  outcomes: [],
  error: null,
  groups: [
    { id: "saves", bytes: 6_000_000_000, items: 17, folders: [] },
    { id: "settings", bytes: 35_000_000, items: 58, folders: [{ path: "/data/shadowmount", bytes: 1, files: 1 }] },
    { id: "other", bytes: 1_000, items: 2, folders: [{ path: "/data/playden", bytes: 1, files: 1 }] },
  ],
  ...over,
});

test("the checklist shows what each group takes", async ({ page }) => {
  await seedConsole(page);
  await routeEngine(page, (path) => (path === "/api/console-backup/status" ? status() : {}));
  await page.goto("/console-backup", { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("cb-group-saves")).toContainText(/6(\.\d+)? ?GB|5\.\d+ ?GiB/);
  await expect(page.getByText("playden")).toBeVisible();
  await expect(page.getByText("What stays out, and why")).toBeVisible();
  // Nothing to back up into yet.
  await expect(page.getByTestId("cb-start-backup")).toBeDisabled();
  await page.screenshot({ path: "../output/playwright/console-backup.png", fullPage: true });
});

test("a save waiting on its game is told apart from a failure", async ({ page }) => {
  await seedConsole(page);
  await routeEngine(page, (path) =>
    path === "/api/console-backup/status"
      ? status({
          outcomes: [
            { group: "saves", item: "PPSA1", result: "needs_save", detail: "x" },
            { group: "settings", item: "/data/etaHEN", result: "failed", detail: "disk full" },
          ],
        })
      : {},
  );
  await page.goto("/console-backup", { waitUntil: "domcontentloaded" });
  const results = page.getByTestId("cb-results");
  await expect(results).toContainText("PPSA1");
  await expect(results).toContainText("run this restore again");
  await expect(results).toContainText("/data/etaHEN");
});

test("the guide puts the steps that erase nothing first", async ({ page }) => {
  await seedConsole(page);
  await routeEngine(page, (path) => (path === "/api/console-backup/status" ? status() : {}));
  await page.goto("/console-backup", { waitUntil: "domcontentloaded" });
  await page.getByTestId("cb-tab-guide").click();
  const guide = page.getByTestId("cb-guide");
  await expect(guide).toContainText("PS5UPDATE.PUP");
  const text = (await guide.textContent()) ?? "";
  expect(text.indexOf("Repair Console Storage")).toBeLessThan(text.lastIndexOf("Reset PS5 (Reinstall"));
  await expect(guide.getByText("Erases your data")).toHaveCount(2);
  await page.screenshot({ path: "../output/playwright/console-backup-guide.png", fullPage: true });
});
