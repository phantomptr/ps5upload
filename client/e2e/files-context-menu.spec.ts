import { expect, test, type Route } from "@playwright/test";

// The Files right-click menu offers New folder: on a row (with the row's own actions) and on
// the list's empty space (a folder menu: New folder, Refresh). The engine here is a fake.

const HOST = "192.168.0.5";

test("right-click offers New folder on a row and on empty space", async ({
  page,
}) => {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);

  const json = (route: Route, body: unknown) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      const p = url.pathname;
      if (p === "/api/version")
        return json(route, { caps: { rar: true }, version: "6.5.2" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          max_transfer_streams: 8,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60 Jul 17 2026 02:16:29",
          started_at_unix: 1791360198,
          ucred_elevated: true,
          version: "6.5.2",
        });
      if (p === "/api/ps5/volumes") return json(route, { volumes: [] });
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [
            { name: "homebrew", kind: "dir", size: 0, mtime: 1791360199 },
          ],
          truncated: false,
          total_scanned: 1,
          returned: 1,
        });
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  const row = main.getByText("homebrew", { exact: true });
  await expect(row).toBeVisible({ timeout: 30_000 });

  // On a row: the row's actions, with New folder among them.
  await row.click({ button: "right" });
  const menu = page.getByTestId("fs-row-menu");
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Open" })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "New folder" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);

  // On the empty space below the rows: the folder menu, without row actions.
  const area = page.getByTestId("fs-list-area");
  const box = await area.boundingBox();
  if (!box) throw new Error("list area has no box");
  await page.mouse.click(box.x + box.width / 2, box.y + box.height - 10, {
    button: "right",
  });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Open" })).toHaveCount(0);
  await expect(menu.getByRole("menuitem", { name: "Refresh" })).toBeVisible();

  // New folder opens the name field.
  await menu.getByRole("menuitem", { name: "New folder" }).click();
  await expect(menu).toHaveCount(0);
  await expect(main.getByPlaceholder("folder name")).toBeVisible();
});
