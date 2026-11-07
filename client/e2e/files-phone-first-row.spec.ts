import { expect, test, type Route } from "@playwright/test";

// Discord: on a phone, in a folder holding only files, Download on the first file could not be
// tapped (touch-and-hold worked). Neither side of that row had room for its ⋮ menu, which was then
// laid over the ⋮ itself. The desktop app is faked (Download exists only there), and its engine.

const HOST = "192.168.0.5";
const FILES = ["a-first.bin", "b-second.bin", "c-third.bin"];

test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });

test("phone width, only files: the first file's menu sits beside its ⋮ and Download takes the tap", async ({
  page,
}) => {
  await page.addInitScript((files) => {
    const w = window as unknown as Record<string, unknown>;
    const calls: string[] = [];
    w.__calls = calls;
    let id = 0;
    w.__TAURI_INTERNALS__ = {
      invoke: async (cmd: string) => {
        calls.push(cmd);
        if (cmd === "ps5_list_dir")
          return {
            entries: files.map((name) => ({ name, kind: "file", size: 1000, mtime: 1791360199 })),
            truncated: false,
          };
        if (cmd === "payload_check")
          return {
            ok: true,
            reachable: true,
            engine: true,
            status: { version: "7.0.29", instance_id: 1, ucred_elevated: true },
          };
        return null;
      },
      transformCallback: () => ++id,
      unregisterCallback: () => {},
      convertFileSrc: (p: string) => p,
      metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
      plugins: {},
    };
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  }, FILES);
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const p = new URL(route.request().url()).pathname;
      if (p === "/api/version") return json(route, { caps: {}, version: "7.0.29" });
      if (p === "/api/ps5/volumes") return json(route, { volumes: [] });
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );

  await page.goto("/files", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  const more = main.getByRole("button", { name: `Actions for ${FILES[0]}` });
  await expect(more).toBeVisible({ timeout: 30_000 });
  const box = await more.boundingBox();
  if (!box) throw new Error("the ⋮ has no box");
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  const menu = page.getByTestId("fs-row-menu");
  await expect(menu).toBeVisible();

  // The menu is beside the ⋮, never over it, and inside the screen.
  const m = await menu.boundingBox();
  if (!m) throw new Error("the menu has no box");
  expect(m.y + m.height <= box.y + 1 || m.y >= box.y + box.height - 1).toBe(true);
  expect(m.y).toBeGreaterThanOrEqual(0);
  expect(m.y + m.height).toBeLessThanOrEqual(800);

  // Download is the menu's own (nothing lies over it), and the tap on it starts the download.
  const download = menu.getByRole("menuitem", { name: "Download", exact: true });
  await expect(download).toBeVisible();
  const d = await download.boundingBox();
  if (!d) throw new Error("Download has no box");
  const mine = await download.evaluate((el, p) => el.contains(document.elementFromPoint(p.x, p.y)), {
    x: d.x + d.width / 2,
    y: d.y + d.height / 2,
  });
  expect(mine).toBe(true);
  const calls = () => page.evaluate(() => (window as unknown as { __calls: string[] }).__calls.slice());
  const before = (await calls()).length;
  await page.touchscreen.tap(d.x + d.width / 2, d.y + d.height / 2);
  await expect.poll(async () => (await calls()).slice(before)).toContain("plugin:dialog|open");
  await expect(menu).toHaveCount(0);
});
