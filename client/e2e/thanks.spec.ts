import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";

const supporters = JSON.parse(
  readFileSync(new URL("../src/data/supporters.json", import.meta.url), "utf8"),
) as { names: string[]; anonymous: number };

// Thanks & Support: every supporter, most first, the anonymous ones counted, never an amount.
test("Thanks & Support lists supporters most first, without amounts", async ({ page }) => {
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    (route) => route.fulfill({ contentType: "application/json", body: "{}" }),
  );
  await page.goto("/thanks", { waitUntil: "domcontentloaded" });
  const thanks = page.getByTestId("thanks");
  await expect(thanks).toBeVisible({ timeout: 30_000 });
  // Everyone, most first.
  await expect(page.getByTestId("thanks-names").locator("li")).toHaveText(supporters.names);
  await expect(page.getByTestId("thanks-anonymous")).toContainText(String(supporters.anonymous));
  await expect(thanks).not.toContainText(/\$|USD|€|\d+\.\d\d/);
  // Crypto: each coin's address, a copy button and its network.
  await page.getByTestId("thanks-coin-eth").click();
  await expect(page.getByTestId("thanks-address")).toHaveAttribute("aria-label", "0x2e787654db543B69FB011E16995C274dFAfc2286");
  await expect(page.getByTestId("thanks-network-warning")).toContainText("Ethereum");
  await page.getByTestId("thanks-coin-btc").click();
  await expect(page.getByTestId("thanks-kofi")).toBeVisible();
  // In the sidebar, right under Bug Report.
  const nav = page.getByRole("navigation").first();
  const bug = await nav.locator('a[href="/bug-report"]').boundingBox();
  const thanksLink = await nav.locator('a[href="/thanks"]').boundingBox();
  if (bug && thanksLink) expect(thanksLink.y).toBeGreaterThan(bug.y);
  if (process.env.THANKS_SHOT) {
    await page.setViewportSize({ width: 1280, height: 2000 });
    await page.screenshot({ path: process.env.THANKS_SHOT });
  }
});
