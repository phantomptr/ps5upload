import { expect, test } from "@playwright/test";

test("guides a disconnected user through primary navigation and recovery", async ({
  page,
}) => {
  // Each Playwright context starts with empty storage, so no console host is
  // selected and this journey cannot issue a PS5-targeted operation. Vite
  // serves any same-origin /api probe as inert app HTML in this test setup.
  await page.goto("/home", { waitUntil: "domcontentloaded" });

  // The sidebar pins Home and nothing else until the user stars screens in
  // More, so a fresh profile sees exactly one favourite plus the More
  // escape hatch. (The nav landmark keeps its "Primary" name deliberately —
  // "Favorites" is the visible heading, not the landmark's accessible name.)
  const primary = page.getByRole("navigation", { name: "Primary" });
  await expect(primary.getByRole("link", { name: "Home" })).toBeVisible();
  await expect(primary.getByText("Star screens in More")).toBeVisible();
  for (const notPinned of ["Games", "Files", "Console", "Tasks"]) {
    await expect(primary.getByRole("link", { name: notPinned })).toHaveCount(0);
  }

  await expect(
    page.getByRole("heading", { name: "Connect a PS5 to get started" }),
  ).toBeVisible();
  await expect(page.getByRole("link", { name: "Connect PS5" })).toBeVisible();
  await expect(page.getByText("Connect to your PS5 to see live telemetry.")).toBeVisible();
  // Anchored, because `hasText` is a case-insensitive substring match: the
  // blocker line under every disabled tile reads "The ps5upload engine is
  // offline.", so a bare "Upload" filter also matches Install PKG, Files,
  // Games, Backup saves and Start FTP — a strict-mode violation whenever the
  // engine probe happens to land as offline.
  await expect(
    page.locator('[aria-disabled="true"]').filter({ hasText: /^Upload/ }),
  ).toBeVisible();

  // Everything unpinned stays one click away through More.
  await page.getByRole("link", { name: "More" }).click();
  await page.getByRole("link", { name: "Tasks" }).click();
  await expect(page.getByRole("heading", { name: "Tasks" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Tasks", pressed: true })).toBeVisible();
  await expect(page.getByRole("button", { name: /history/i })).toBeVisible();
  await expect(page.getByText("No activity yet")).toBeVisible();

  await page.getByRole("link", { name: "More" }).click();
  await expect(page.getByRole("heading", { name: "More" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Install Package" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Settings" })).toBeVisible();

  // Regression: this screen used to draw its entire staging UI with no console
  // connected — a small "No PS5 host set" warning, and then the full body
  // anyway. ConnectionGate must now own the body. Which rung of the ladder it
  // lands on depends on the environment (a Playwright run has no engine
  // either), so accept any of them; what matters is that the body is gone.
  await page.goto("/install-package", { waitUntil: "domcontentloaded" });
  // /install-package is lazily imported, and each Playwright run gets a cold
  // vite server that has to transform the chunk on first request — well over
  // the 5s default expect timeout on a big screen like this one.
  await expect(
    page.getByRole("heading", {
      name: /No PS5 connected yet|Transfer engine isn't running|Helper isn't running/,
    }),
  ).toBeVisible({ timeout: 20_000 });
  await expect(page.getByText("How installing works")).toHaveCount(0);
});
