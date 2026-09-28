import { expect, test } from "@playwright/test";

// The FAQ and What's New screens bundle the repo-root FAQ.md and CHANGELOG.md. On the dev server
// those sit outside the client folder, and Vite served them as raw markdown instead of modules,
// so both screens failed to load their document.
test("the FAQ and What's New screens load their documents", async ({ page }) => {
  await page.goto("/faq", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "What ps5upload does (and doesn't)" })).toBeVisible({
    timeout: 20_000,
  });
  await page.goto("/whats-new", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "5.37.1" }).first()).toBeVisible({ timeout: 20_000 });
});
