import { expect, test } from "@playwright/test";

// Switching screens used to throw each screen away, so anything typed, opened or scrolled
// was gone on return. Screens now stay alive, hidden, while another one is shown.

test("a screen keeps what was typed across a screen change", async ({
  page,
}) => {
  await page.goto("/faq", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");

  // Something typed that lives only in the screen.
  const search = main.getByPlaceholder("Search the FAQ…");
  await expect(search).toBeVisible({ timeout: 30_000 });
  await search.fill("rest mode");

  const primary = page.getByRole("navigation", { name: "Primary" });
  await primary.getByRole("link", { name: "Tasks", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Tasks" })).toBeVisible();
  // The screen left behind is out of the way: nothing of it shows.
  await expect(main.getByPlaceholder("Search the FAQ…")).toBeHidden();

  await page.goBack();
  await expect(main.getByPlaceholder("Search the FAQ…")).toHaveValue(
    "rest mode",
  );
});

test("each screen scrolls by itself and comes back where it was left", async ({
  page,
}) => {
  await page.goto("/faq", { waitUntil: "domcontentloaded" });
  const scroller = page.locator("[data-scroll-root]");
  await expect(scroller).toHaveCount(1, { timeout: 30_000 });
  await page.getByRole("main").getByRole("heading").first().waitFor();
  await scroller.evaluate((el) => {
    el.scrollTop = 600;
  });
  await expect
    .poll(() => scroller.evaluate((el) => el.scrollTop))
    .toBeGreaterThan(300);

  const primary = page.getByRole("navigation", { name: "Primary" });
  await primary.getByRole("link", { name: "Tasks", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Tasks" })).toBeVisible();
  // Only the screen on show is the scroll root, so a dialog's scroll lock finds the right one.
  await expect(page.locator("[data-scroll-root]")).toHaveCount(1);
  expect(
    await page.locator("[data-scroll-root]").evaluate((el) => el.scrollTop),
  ).toBe(0);

  await page.goBack();
  await expect
    .poll(() =>
      page.locator("[data-scroll-root]").evaluate((el) => el.scrollTop),
    )
    .toBeGreaterThan(300);
});
