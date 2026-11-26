import { expect, test } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// Files › Snapshots: a restore that left some files out is told as a warning, with how many,
// not in the green box of a restore that brought every file back.

const SNAPSHOT = { tag: "saves", timestamp: 1_790_000_000, files: 6, bytes: 4096 };

async function restore(page: import("@playwright/test").Page, answer: Record<string, unknown>) {
  await seedConsole(page);
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/ps5/status") return { ok: true, version: "12.6.31", ucred_elevated: true };
    if (path === "/api/ps5/backup/list") return { snapshots: [SNAPSHOT] };
    if (path === "/api/ps5/backup/restore") return { ok: true, tag: SNAPSHOT.tag, err: "", ...answer };
    return undefined;
  });
  await page.goto("/files?view=snapshots", { waitUntil: "domcontentloaded" });
  const button = page.getByTestId("backup-restore");
  // An icon button: its name is what a screen reader has.
  await expect(button).toHaveAccessibleName("Restore", { timeout: 60_000 });
  await button.click();
  await page.getByTestId("confirm-ok").click();
  return page.getByTestId("backup-action-msg");
}

test("a restore that left files out says so as a warning", async ({ page }) => {
  const msg = await restore(page, { restored: 2, skipped: 1, failed: 3, partial: true });
  await expect(msg).toHaveText("Restored 2 files; 4 could not be restored");
  await expect(msg).toHaveAttribute("data-tone", "warn");
  await expect(msg).toHaveRole("status");
});

test("a whole restore is a success", async ({ page }) => {
  const msg = await restore(page, { restored: 6, skipped: 0, failed: 0, partial: false });
  await expect(msg).toHaveText("Restored 6 files");
  await expect(msg).toHaveAttribute("data-tone", "good");
});
