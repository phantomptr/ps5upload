import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

/** The newest released version's heading in CHANGELOG.md (an "## Unreleased" heading is skipped):
 *  the screen shows the newest sections, so a fixed old version drops out of it over time. */
function newestReleased(): string {
  const text = readFileSync(fileURLToPath(new URL("../../CHANGELOG.md", import.meta.url)), "utf8");
  const m = text.match(/^## (\d+\.\d+\.\d+)\s*$/m);
  if (!m) throw new Error("no released version heading in CHANGELOG.md");
  return m[1];
}

// The FAQ and What's New screens bundle the repo-root FAQ.md and CHANGELOG.md. On the dev server
// those sit outside the client folder, and Vite served them as raw markdown instead of modules,
// so both screens failed to load their document.
test("the FAQ and What's New screens load their documents", async ({ page }) => {
  await page.goto("/faq", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "What ps5upload does (and doesn't)" })).toBeVisible({
    timeout: 20_000,
  });
  await page.goto("/faq?tab=whats-new", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: newestReleased() }).first()).toBeVisible({ timeout: 20_000 });
});
