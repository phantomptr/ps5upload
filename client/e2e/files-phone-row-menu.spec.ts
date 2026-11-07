import { expect, test, type Route } from "@playwright/test";

// #470: on a phone the Files row shows its name and one ⋮ that holds the row's
// actions; the selection bar stays one line. The engine here is a fake.

const HOST = "192.168.0.5";
const LONG = "UP0000-CUSA07842_00-JAKXCOMBATRACING-A0100-V0100.pkg";

test.use({ viewport: { width: 360, height: 800 }, hasTouch: true });

test("phone width: row names show, ⋮ opens the row's actions, the selection bar is one line", async ({
  page,
}) => {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      const p = url.pathname;
      if (p === "/api/version") return json(route, { caps: { rar: true }, version: "7.0.29" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          max_transfer_streams: 8,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60 Jul 17 2026 02:16:29",
          started_at_unix: 1791360198,
          ucred_elevated: true,
          version: "7.0.29",
        });
      if (p === "/api/ps5/volumes") return json(route, { volumes: [] });
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [
            { name: "homebrew", kind: "dir", size: 0, mtime: 1791360199 },
            { name: LONG, kind: "file", size: 101502976, mtime: 1791360199 },
          ],
          truncated: false,
          total_scanned: 2,
          returned: 2,
        });
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  const name = main.getByRole("button", { name: LONG, exact: true });
  await expect(name).toBeVisible({ timeout: 30_000 });
  // The name gets real room, not the zero width the action buttons left it.
  expect((await name.boundingBox())?.width ?? 0).toBeGreaterThan(100);
  await expect(name).toHaveAttribute("title", LONG);

  // The desktop icon buttons are hidden; the ⋮ holds them.
  await expect(main.getByTestId("fs-row-actions").first()).toBeHidden();
  const more = main.getByRole("button", { name: `Actions for ${LONG}` });
  await more.click();
  const menu = page.getByTestId("fs-row-menu");
  await expect(menu).toBeVisible();
  const items = menu.getByRole("menuitem");
  await expect(items.first()).toHaveText("Rename");
  await expect(menu.getByRole("menuitem", { name: "Properties & permissions" })).toBeVisible();
  await expect(items.last()).toHaveText("Delete");
  await expect(menu.getByRole("menuitem", { name: "Install" })).toBeVisible();
  const box = await menu.boundingBox();
  if (!box) throw new Error("menu has no box");
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(360);
  expect(box.y + box.height).toBeLessThanOrEqual(800);
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);

  // Rename from the menu opens the row's rename field, as the desktop button does.
  await more.click();
  await menu.getByRole("menuitem", { name: "Rename", exact: true }).click();
  await expect(main.locator(`input[value="${LONG}"]`)).toBeVisible();
  await page.keyboard.press("Escape");

  // Selecting a row: "1 selected" on one line.
  await main.locator("li input[type=checkbox]").first().check();
  const count = main.getByTestId("fs-selection-bar").getByText("1 selected");
  await expect(count).toBeVisible();
  const countBox = await count.boundingBox();
  expect(countBox?.height ?? 99).toBeLessThan(30);
});
