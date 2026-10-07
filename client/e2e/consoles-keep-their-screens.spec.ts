import { expect, test } from "@playwright/test";

// Switching console tabs used to rebuild every screen, so anything typed or loaded for one
// console was gone when you came back to it. Each console now keeps its own screens.

const A = { id: "console-a", name: "Living room", host: "192.168.0.5" };
const B = { id: "console-b", name: "Office", host: "192.168.0.6" };

test("each console keeps its own screens across a console switch", async ({
  page,
}) => {
  await page.addInitScript(
    ([a, b]) => {
      // Only on the first load: a reload must find what the app itself saved.
      if (window.localStorage.getItem("ps5upload.roster.v1")) return;
      window.localStorage.setItem("ps5upload.host", a.host);
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({ profiles: [a, b], active_id: a.id }),
      );
    },
    [A, B],
  );
  // No engine here: every engine route answers with nothing.
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) =>
      route.fulfill({ status: 200, contentType: "application/json", body: "{}" }),
  );

  await page.goto("/faq", { waitUntil: "domcontentloaded" });
  // Each console's tree has its own tab strip; only the one on show is visible.
  const tabA = page.locator(`[data-console-id="${A.id}"]:visible`);
  const tabB = page.locator(`[data-console-id="${B.id}"]:visible`);
  await expect(tabA).toHaveAttribute("aria-current", "page", { timeout: 30_000 });

  // The screen on show, whichever console's it is.
  const search = page
    .locator("[data-scroll-root]:visible")
    .getByPlaceholder("Search the FAQ…");
  await search.fill("rest mode");

  await tabB.click();
  await expect(tabB).toHaveAttribute("aria-current", "page");
  // B has its own screen: nothing of A's typing is in it.
  await expect(search).toHaveValue("");
  await search.fill("firewall");
  // One scroll root is on show: the selected console's.
  await expect(page.locator("[data-scroll-root]:visible")).toHaveCount(1);

  await tabA.click();
  await expect(tabA).toHaveAttribute("aria-current", "page");
  await expect(search).toHaveValue("rest mode");

  await tabB.click();
  await expect(tabB).toHaveAttribute("aria-current", "page");
  await expect(search).toHaveValue("firewall");
});
