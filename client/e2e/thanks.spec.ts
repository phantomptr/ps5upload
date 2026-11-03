import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";

const supporters = JSON.parse(
  readFileSync(new URL("../src/data/supporters.json", import.meta.url), "utf8"),
) as { tiers: Record<string, string[]>; anonymous: number };

// Help › Thank you: every supporter by tier, the anonymous ones counted, never an amount.
test("the Thank you tab lists supporters by tier, without amounts", async ({ page }) => {
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    (route) => route.fulfill({ contentType: "application/json", body: "{}" }),
  );
  await page.goto("/faq?tab=thanks", { waitUntil: "domcontentloaded" });
  const thanks = page.getByTestId("thanks");
  await expect(thanks).toBeVisible({ timeout: 30_000 });
  for (const [tier, names] of Object.entries(supporters.tiers)) {
    if (!names.length) continue;
    await expect(page.getByTestId(`thanks-${tier}`)).toContainText(names[0]);
  }
  await expect(page.getByTestId("thanks-anonymous")).toContainText(String(supporters.anonymous));
  await expect(thanks).not.toContainText(/\$|USD|€|\d+\.\d\d/);
  if (process.env.THANKS_SHOT) await page.screenshot({ path: process.env.THANKS_SHOT, fullPage: true });
});
