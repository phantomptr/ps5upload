import { expect, test, type Route } from "@playwright/test";

// A folder of thousands of entries: only the rows near the screen exist, the scroll bar is still
// the whole folder's, and scrolling reaches the end. The engine here is a fake.

const HOST = "192.168.0.5";
const COUNT = 5000;
const nameOf = (i: number) => `file-${String(i).padStart(5, "0")}.bin`;

test("a large folder renders a window of its rows and scrolls to the end", async ({
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
        const offset = Number(url.searchParams.get("offset") ?? 0);
        const limit = Number(url.searchParams.get("limit") ?? 256);
        const end = Math.min(COUNT, offset + limit);
        const entries = [];
        for (let i = offset; i < end; i++) {
          entries.push({
            name: nameOf(i),
            kind: "file",
            size: 1024,
            mtime: 1791360199,
          });
        }
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries,
          truncated: end < COUNT,
          total_scanned: end,
          returned: entries.length,
        });
      }
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const rows = page.locator("[data-fs-name]");
  await expect(rows.first()).toHaveAttribute("data-fs-name", nameOf(0), {
    timeout: 30_000,
  });

  // Wait for every page of the listing: the list's own height says so.
  const list = rows.first().locator("xpath=..");
  await expect
    .poll(async () => list.evaluate((el) => (el as HTMLElement).offsetHeight), {
      timeout: 30_000,
    })
    .toBeGreaterThan(COUNT * 20);

  // A window, not the folder.
  expect(await rows.count()).toBeLessThan(300);

  // The scroll bar reaches the last entry, which only then exists.
  await expect(
    page.locator(`[data-fs-name="${nameOf(COUNT - 1)}"]`),
  ).toHaveCount(0);
  await list.evaluate((el) => {
    let p = el.parentElement;
    while (p && !/auto|scroll/.test(getComputedStyle(p).overflowY))
      p = p.parentElement;
    if (p) p.scrollTop = p.scrollHeight;
  });
  await expect(
    page.locator(`[data-fs-name="${nameOf(COUNT - 1)}"]`),
  ).toBeVisible({ timeout: 10_000 });
  expect(await rows.count()).toBeLessThan(300);
  await expect(page.locator(`[data-fs-name="${nameOf(0)}"]`)).toHaveCount(0);

  // And back.
  await list.evaluate((el) => {
    let p = el.parentElement;
    while (p && !/auto|scroll/.test(getComputedStyle(p).overflowY))
      p = p.parentElement;
    if (p) p.scrollTop = 0;
  });
  await expect(page.locator(`[data-fs-name="${nameOf(0)}"]`)).toBeVisible({
    timeout: 10_000,
  });
});
