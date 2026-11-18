import { expect, test, type Route } from "@playwright/test";

// Going back out of a folder brings the folder you came out of into view, focuses it (Enter opens
// it again) and lights it for a moment, without selecting it. The engine here is a fake.

const HOST = "192.168.0.5";

const LISTINGS: Record<string, { name: string; kind: "dir" | "file" }[]> = {
  "/data": [
    ...Array.from({ length: 40 }, (_, i) => ({
      name: `aaa-${String(i).padStart(2, "0")}`,
      kind: "dir" as const,
    })),
    { name: "games", kind: "dir" },
    { name: "notes.txt", kind: "file" },
  ],
  "/data/games": [{ name: "CUSA00001", kind: "dir" }],
  "/data/games/CUSA00001": [{ name: "eboot.bin", kind: "file" }],
};

test("going back highlights and focuses the folder you came out of", async ({
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
      if (p === "/api/ps5/list-dir") {
        const path = url.searchParams.get("path") ?? "/data";
        const entries = (LISTINGS[path] ?? []).map((e) => ({
          ...e,
          size: 1024,
          mtime: 1791360199,
        }));
        return json(route, {
          path,
          entries,
          truncated: false,
          total_scanned: entries.length,
          returned: entries.length,
        });
      }
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/files?path=/data", { waitUntil: "domcontentloaded" });
  const games = page.locator('[data-fs-name="games"]');
  await expect(games).toBeVisible({ timeout: 30_000 });
  // Going into a folder lights nothing.
  await expect(page.locator("[data-fs-revealed]")).toHaveCount(0);

  // In, then back out with Backspace (the keyboard's Up).
  await games.locator("button[data-fs-open]").click();
  await expect(page.locator('[data-fs-name="CUSA00001"]')).toBeVisible();
  await page.locator("body").click({ position: { x: 1, y: 1 } });
  await page.keyboard.press("Backspace");

  await expect(games).toHaveAttribute("data-fs-revealed", "", { timeout: 10_000 });
  await expect(games).toHaveClass(/fs-reveal/);
  await expect(games.locator("button[data-fs-open]")).toBeFocused();
  await expect(games).toBeInViewport();
  // Not selected: the checkbox stays clear.
  await expect(games.locator('input[type="checkbox"]')).not.toBeChecked();
  // The light goes after a moment.
  await expect(page.locator("[data-fs-revealed]")).toHaveCount(0, { timeout: 5_000 });

  // Enter opens it again; ↓ / ↑ move from the focused row.
  await page.keyboard.press("Enter");
  await expect(page.locator('[data-fs-name="CUSA00001"]')).toBeVisible();
  await page.keyboard.press("Backspace");
  await expect(games.locator("button[data-fs-open]")).toBeFocused({ timeout: 10_000 });
  await page.keyboard.press("ArrowDown");
  await expect(page.locator('[data-fs-name="notes.txt"]')).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(games.locator("button[data-fs-open]")).toBeFocused();

  // Two levels down, then the "data" breadcrumb: the folder on the way is the one lit.
  await page.keyboard.press("Enter");
  await page.locator('[data-fs-name="CUSA00001"] button[data-fs-open]').click();
  await expect(page.locator('[data-fs-name="eboot.bin"]')).toBeVisible();
  await page.getByRole("button", { name: "data", exact: true }).click();
  await expect(games).toHaveAttribute("data-fs-revealed", "", { timeout: 10_000 });
  await expect(games.locator("button[data-fs-open]")).toBeFocused();
  await expect(page.locator("[data-fs-revealed]")).toHaveCount(1);
});
