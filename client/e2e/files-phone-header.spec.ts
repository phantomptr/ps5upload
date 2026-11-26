import { expect, test, type Locator, type Page, type Route } from "@playwright/test";

// #936: on a phone, Files has a compact header pinned over the list (the folder, search, the drive
// and ⋮ on top, the path under it); a long press starts selection mode, whose actions take the
// bottom navigation's place. #932: system Back closes an open menu and stays in the folder, leaves
// selection mode, then goes up one folder. Tablets and computers keep their layout.

const HOST = "192.168.0.5";
const START = "/mnt/usb0/homebrew";
const FILES = Array.from({ length: 30 }, (_, i) => `file-${String(i + 1).padStart(2, "0")}.pkg`);
const LISTINGS: Record<string, { name: string; kind: "dir" | "file" }[]> = {
  "/mnt/usb0/homebrew": [
    { name: "games", kind: "dir" },
    { name: "apps", kind: "dir" },
    ...FILES.map((name) => ({ name, kind: "file" as const })),
  ],
  "/mnt/usb0/homebrew/games": [{ name: "inner.pkg", kind: "file" }],
  "/mnt/usb0": [{ name: "homebrew", kind: "dir" }],
  "/data": [{ name: "data-file.bin", kind: "file" }],
};
const VOLUMES = [
  { path: "/data", fs_type: "nullfs", total_bytes: 900e9, free_bytes: 400e9, writable: true, is_placeholder: false },
  { path: "/mnt/usb0", fs_type: "exfatfs", total_bytes: 1e12, free_bytes: 5e11, writable: true, is_placeholder: false },
];

/** Files with a fake engine. `app`: the Android app (a fake Tauri, whose shell asks
 *  window.__ps5uBack on Back); else the web UI in a phone's browser (Back moves the history). */
async function openFiles(page: Page, { app = true }: { app?: boolean } = {}) {
  if (app) {
    await page.addInitScript(
      ({ listings, volumes }) => {
        const w = window as unknown as Record<string, unknown>;
        let id = 0;
        w.__TAURI_INTERNALS__ = {
          invoke: async (cmd: string, args?: { path?: string }) => {
            if (cmd === "ps5_list_dir")
              return {
                entries: (listings[args?.path ?? ""] ?? []).map((e) => ({ ...e, size: 1000, mtime: 1791360199 })),
                truncated: false,
              };
            if (cmd === "ps5_volumes") return { volumes };
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
      },
      { listings: LISTINGS, volumes: VOLUMES },
    );
  }
  await page.addInitScript(
    ({ host, start }) => {
      window.localStorage.setItem("ps5upload.host", host);
      window.localStorage.setItem("ps5upload.fs.lastPath", JSON.stringify({ [host]: start }));
    },
    { host: HOST, start: START },
  );
  const json = (route: Route, body: unknown) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const url = new URL(route.request().url());
      const p = url.pathname;
      if (p === "/api/version") return json(route, { caps: {}, version: "7.0.29" });
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
      if (p === "/api/ps5/volumes") return json(route, { volumes: VOLUMES });
      if (p === "/api/ps5/list-dir") {
        const path = url.searchParams.get("path") ?? "/data";
        const entries = (LISTINGS[path] ?? []).map((e) => ({ ...e, size: 1000, mtime: 1791360199 }));
        return json(route, { path, entries, truncated: false, total_scanned: entries.length, returned: entries.length });
      }
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );
  await page.goto("/files", { waitUntil: "domcontentloaded" });
  await expect(row(page, "file-01.pkg")).toBeVisible({ timeout: 30_000 });
}

const main = (page: Page) => page.getByRole("main");
const title = (page: Page) => main(page).getByTestId("fs-phone-title");
const count = (page: Page) => main(page).getByTestId("fs-phone-count");
const row = (page: Page, name: string) => main(page).locator(`li[data-fs-name="${name}"]`);
const actionsBar = (page: Page) => main(page).getByRole("toolbar", { name: "Actions for the selected items" });
const menu = (page: Page) => page.getByTestId("fs-row-menu");
const back = (page: Page) => page.evaluate(() => window.__ps5uBack?.() ?? null);

/** A finger held on `target`, as the phone sends it (touch events: Chrome makes the pointer
 *  events, and its own long press, from them). */
async function longPress(page: Page, target: Locator) {
  await target.scrollIntoViewIfNeeded();
  const box = await target.boundingBox();
  if (!box) throw new Error("no box");
  const x = box.x + Math.min(40, box.width / 2);
  const y = box.y + box.height / 2;
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
  await page.waitForTimeout(800);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await cdp.detach();
}

async function shot(page: Page, name: string) {
  const dir = process.env.FILES_PHONE_SHOTS;
  if (dir) await page.screenshot({ path: `${dir}/${name}.png` });
}

const PHONES = [
  { name: "portrait", viewport: { width: 360, height: 800 } },
  { name: "sideways 915x412", viewport: { width: 915, height: 412 } },
  { name: "sideways 844x390", viewport: { width: 844, height: 390 } },
];

for (const phone of PHONES) {
  test.describe(`phone ${phone.name}`, () => {
    test.use({ viewport: phone.viewport, hasTouch: true, isMobile: true });

    test("the compact header stays pinned over the list, with the path (#936)", async ({ page }) => {
      await openFiles(page);
      await expect(title(page)).toHaveText("homebrew");
      // The page header and the drive chips give way to the compact header.
      await expect(main(page).getByRole("button", { name: "Add files", exact: true })).toBeHidden();
      await expect(main(page).getByRole("button", { name: /^\/mnt\/usb0/ })).toBeHidden();
      const header = main(page).getByTestId("fs-header");
      await expect(header.getByRole("button", { name: "Search" })).toBeVisible();
      await expect(header.getByRole("button", { name: "Drives" })).toBeVisible();
      await expect(header.getByRole("button", { name: "More" })).toBeVisible();
      await expect(header.getByRole("button", { name: "usb0" })).toBeVisible();
      // No boxes until selecting.
      await expect(row(page, "file-01.pkg").getByRole("checkbox")).toBeHidden();
      // One bar at the top: no Back and Forward arrows (the phone's Back goes back), and one
      // bell, in this header. Below 768 px the app's own top bar gives way; held sideways, the
      // row beside the sidebar does (its search, reload and theme buttons go too): with both
      // rows there, about two files were left in view.
      await expect(page.getByRole("navigation", { name: "View history" }).locator("visible=true")).toHaveCount(0);
      const bell = page.getByRole("button", { name: "Open notifications" }).locator("visible=true");
      await expect(bell).toHaveCount(1);
      {
        await expect(header.getByRole("button", { name: "Open notifications" })).toBeVisible();
        await expect(page.locator("[data-top-bar]").getByText("PS5Upload")).toBeHidden();
        await expect(page.getByRole("button", { name: "Reload page" }).locator("visible=true")).toHaveCount(0);
        await expect(page.getByRole("button", { name: "Search screens and commands" }).locator("visible=true")).toHaveCount(0);
        // At the top (held sideways, under the page's own top margin only: it was under a row
        // of buttons, at about 88 px).
        expect((await header.boundingBox())!.y).toBeLessThan(phone.viewport.width < 768 ? 24 : 40);
        // Rows of files in view under the header (two, before).
        const shown = await main(page)
          .locator("li[data-fs-name]")
          .evaluateAll((rows) => rows.filter((r) => {
            const b = r.getBoundingClientRect();
            return b.top >= 0 && b.bottom <= window.innerHeight;
          }).length);
        expect(shown).toBeGreaterThanOrEqual(3);
        // Its panel hangs under the header, inside the window.
        await bell.tap();
        const panel = page.getByTestId("notifications-panel").locator("visible=true");
        await expect(panel).toBeVisible();
        const p = (await panel.boundingBox())!;
        const h = (await header.boundingBox())!;
        expect(p.y).toBeGreaterThanOrEqual(h.y + h.height);
        expect(p.x).toBeGreaterThanOrEqual(0);
        expect(p.x + p.width).toBeLessThanOrEqual(phone.viewport.width);
        await expect(panel).toHaveCSS("opacity", "1");
        await shot(page, `bell-${phone.viewport.width}x${phone.viewport.height}`);
        expect(await back(page)).toBe(true);
        await expect(panel).toHaveCount(0);
      }
      await shot(page, `header-${phone.viewport.width}x${phone.viewport.height}`);

      // The list scrolls by itself under the header (#936): the header is outside every scroller,
      // so the page never moves it.
      const before = (await header.boundingBox())!;
      const scroller = main(page).locator("[data-files-scroll]");
      await scroller.evaluate((el) => (el.scrollTop = 600));
      await expect.poll(() => scroller.evaluate((el) => el.scrollTop)).toBeGreaterThan(300);
      await expect.poll(() => page.locator("[data-scroll-root]").evaluate((el) => el.scrollTop)).toBe(0);
      const box = (await header.boundingBox())!;
      expect(box).toEqual(before);
      await expect(header.getByRole("button", { name: "usb0" })).toBeInViewport();
      // Room for the list: two short rows; held sideways, where height is short, one (the path
      // beside the folder instead of under it).
      const sideways = phone.viewport.width > phone.viewport.height;
      expect(box.height).toBeLessThan(sideways ? 70 : 120);
      const path = (await header.getByRole("button", { name: "usb0" }).boundingBox())!;
      const folder = (await title(page).boundingBox())!;
      if (sideways) expect(path.x).toBeGreaterThan(folder.x + folder.width);
      else expect(path.y).toBeGreaterThan(folder.y + folder.height);
      await shot(page, `header-scrolled-${phone.viewport.width}x${phone.viewport.height}`);
    });

    test("a long press selects; the bar of actions takes the navigation's place; Back leaves selection, then goes up (#936, #932)", async ({
      page,
    }) => {
      await openFiles(page);
      // Where each row's parts are before selecting: starting selection mode moves none of them.
      const parts = async (name: string) => ({
        row: await row(page, name).boundingBox(),
        icon: await row(page, name).locator("svg").first().boundingBox(),
        name: await row(page, name).locator("[data-fs-open]").boundingBox(),
      });
      const before = { games: await parts("games"), file: await parts("file-01.pkg") };
      const dots = (await main(page).getByRole("button", { name: "Actions for file-01.pkg" }).boundingBox())!;
      // A folder: held, it is selected, not opened.
      await longPress(page, row(page, "games"));
      await expect(count(page)).toHaveText("1 selected");
      await expect(title(page)).toBeHidden();
      await expect(row(page, "games").getByRole("checkbox")).toBeChecked();
      await expect(row(page, "file-01.pkg").getByRole("checkbox")).toBeVisible();
      expect({ games: await parts("games"), file: await parts("file-01.pkg") }).toEqual(before);
      // The box is where ⋮ was, which is out of sight and out of reach meanwhile.
      await expect(main(page).getByRole("button", { name: "Actions for file-01.pkg" })).toBeHidden();
      const tick = (await row(page, "file-01.pkg").getByRole("checkbox").boundingBox())!;
      const mid = (b: { x: number; y: number; width: number; height: number }) => ({
        x: b.x + b.width / 2,
        y: b.y + b.height / 2,
      });
      expect(Math.abs(mid(tick).x - mid(dots).x)).toBeLessThan(4);
      expect(Math.abs(mid(tick).y - mid(dots).y)).toBeLessThan(2);
      expect(tick.x).toBeGreaterThan(before.file.name!.x + before.file.name!.width - 1);
      // One item selected: More has what that row's ⋮ has, less what the bar shows as buttons.
      await actionsBar(page).getByRole("button", { name: "More", exact: true }).tap();
      for (const name of ["Cut", "Set permissions (777)", "Properties & permissions", "What takes space?", "Snapshot…"])
        await expect(menu(page).getByRole("menuitem", { name, exact: true })).toBeVisible();
      for (const name of ["Rename", "Delete"])
        await expect(menu(page).getByRole("menuitem", { name, exact: true })).toHaveCount(0);
      await shot(page, `selection-more-one-${phone.viewport.width}x${phone.viewport.height}`);
      // Picking one of them acts on the item and leaves selection mode.
      await menu(page).getByRole("menuitem", { name: "What takes space?", exact: true }).click();
      await expect(main(page).getByTestId("fs-view")).toBeVisible();
      await expect(actionsBar(page)).toHaveCount(0);
      await main(page).getByRole("button", { name: "Back to files" }).tap();
      await expect(title(page)).toHaveText("homebrew");
      await expect(row(page, "file-01.pkg").getByRole("checkbox")).toBeHidden();
      await longPress(page, row(page, "games"));
      await expect(count(page)).toHaveText("1 selected");
      await expect(page.locator("[data-bottom-nav]")).toBeHidden();
      const bar = actionsBar(page);
      await expect(bar).toBeInViewport();
      // The finger lifting is not a tap on what selecting put under it (held low on a short
      // screen, the bar comes up under the finger): nothing was copied.
      await expect(main(page).getByRole("button", { name: "Paste here" })).toHaveCount(0);
      for (const name of ["Copy", "Move to…", "Rename", "Delete", "More"])
        await expect(bar.getByRole("button", { name, exact: true })).toBeVisible();

      // Selecting, a tap ticks a row instead of opening it; a file's name too (a disabled button,
      // which would swallow the tap).
      await row(page, "apps").getByRole("button", { name: "apps", exact: true }).tap();
      await expect(count(page)).toHaveText("2 selected");
      await expect(title(page)).toBeHidden();
      await row(page, "file-02.pkg").getByText("file-02.pkg").tap({ force: true });
      await expect(count(page)).toHaveText("3 selected");
      await shot(page, `selecting-${phone.viewport.width}x${phone.viewport.height}`);
      // A long press on a file's name ticks it as well.
      await row(page, "file-02.pkg").tap();
      await expect(count(page)).toHaveText("2 selected");
      await longPress(page, row(page, "file-02.pkg").getByText("file-02.pkg"));
      await expect(count(page)).toHaveText("3 selected");

      // More: the selection's other actions, in a menu that Back closes first.
      await bar.getByRole("button", { name: "More", exact: true }).tap();
      await expect(menu(page).getByRole("menuitem", { name: "Cut" })).toBeVisible();
      await expect(menu(page).getByRole("menuitem", { name: "Set permissions (777)" })).toBeVisible();
      await expect(menu(page).getByRole("menuitem", { name: "Snapshot…" })).toBeVisible();
      // Several selected: nothing that acts on one item.
      for (const name of ["Properties & permissions", "What takes space?", "Replace file", "Install"])
        await expect(menu(page).getByRole("menuitem", { name, exact: true })).toHaveCount(0);
      expect(await back(page)).toBe(true);
      await expect(menu(page)).toHaveCount(0);
      await expect(count(page)).toHaveText("3 selected");

      await main(page).getByRole("button", { name: "Select all" }).tap();
      await expect(count(page)).toHaveText(`${LISTINGS[START].length} selected`);

      // Back: selection mode ends, the folder stays.
      expect(await back(page)).toBe(true);
      await expect(title(page)).toHaveText("homebrew");
      await expect(row(page, "file-01.pkg").getByRole("checkbox")).toBeHidden();
      await expect(actionsBar(page)).toHaveCount(0);
      if (phone.viewport.width < 768) await expect(page.locator("[data-bottom-nav]")).toBeVisible();

      // Back: up one folder; at the drive's root the press is the app's (it leaves Files).
      expect(await back(page)).toBe(true);
      await expect(title(page)).toHaveText("usb0");
      expect(await back(page)).toBe(false);
    });

    test("Back closes a row's menu and stays in the folder (#932)", async ({ page }) => {
      await openFiles(page);
      await main(page).getByRole("button", { name: "Actions for file-01.pkg" }).tap();
      await expect(menu(page).getByRole("menuitem", { name: "Rename", exact: true })).toBeVisible();
      expect(await back(page)).toBe(true);
      await expect(menu(page)).toHaveCount(0);
      await expect(title(page)).toHaveText("homebrew");
      await expect(row(page, "file-01.pkg")).toBeVisible();
    });

    test("⋮ holds the page's actions and Select; the drive button switches drives", async ({ page }) => {
      await openFiles(page);
      const header = main(page).getByTestId("fs-header");
      await header.getByRole("button", { name: "More" }).tap();
      for (const name of ["Add files", "Add folder", "New folder", "Refresh", "Select", "See snapshots"])
        await expect(menu(page).getByRole("menuitem", { name, exact: true })).toBeVisible();
      await shot(page, `more-menu-${phone.viewport.width}x${phone.viewport.height}`);
      // Select: selection mode with nothing ticked yet, for a keyboard or a D-pad.
      await menu(page).getByRole("menuitem", { name: "Select", exact: true }).click();
      await expect(count(page)).toHaveText("0 selected");
      const box = row(page, "file-03.pkg").getByRole("checkbox");
      await box.focus();
      await page.keyboard.press("Space");
      await expect(count(page)).toHaveText("1 selected");
      // Rename with one item: its name in place.
      await actionsBar(page).getByRole("button", { name: "Rename", exact: true }).tap();
      await expect(main(page).locator('input[value="file-03.pkg"]')).toBeVisible();
      expect(await back(page)).toBe(true);
      await expect(main(page).locator('input[value="file-03.pkg"]')).toHaveCount(0);

      await header.getByRole("button", { name: "Drives" }).tap();
      const current = menu(page).getByRole("menuitem", { name: /^\/mnt\/usb0 ·/ });
      await expect(current).toHaveAttribute("aria-current", "true");
      await expect(menu(page).getByRole("menuitem", { name: "Eject /mnt/usb0" })).toBeVisible();
      await menu(page).getByRole("menuitem", { name: /^\/data ·/ }).click();
      await expect(title(page)).toHaveText("data");
    });
  });
}

// The ⋮ menu's pairs read as twins before: "What takes space here?" beside "What takes space?",
// "Snapshot this folder" beside "Snapshots". Now each says what it is about (this folder, this
// drive) and what it does (make a snapshot, see them), in groups with a line between them. In a
// wide font (Linux CI's), every item is one line and the menu is whole on screen.
for (const viewport of [
  { width: 360, height: 800 },
  { width: 844, height: 390 },
]) {
  test.describe(`the ⋮ menu's groups, ${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport, hasTouch: true, isMobile: true });

    test("each folder and drive item says which it is, in groups", async ({ page }) => {
      await openFiles(page);
      await page.addStyleTag({ content: "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}" });
      await main(page).getByTestId("fs-header").getByRole("button", { name: "More" }).tap();
      const items = menu(page).getByRole("menuitem");
      const labels = (await items.allTextContents()).map((t) => t.trim());
      expect(labels).toEqual([
        "Add files",
        "Add folder",
        "New folder",
        "Refresh",
        "Select",
        "What takes space in this folder?",
        "Make a snapshot of this folder",
        "Download from a link",
        "What takes space on this drive?",
        "See snapshots",
        "Search all drives",
        "Search this folder",
      ]);
      // Groups: the page's actions, this folder, a link, the drive.
      const separators = menu(page).getByRole("separator");
      await expect(separators).toHaveCount(3);
      const order = await menu(page).evaluate((m) =>
        Array.from(m.querySelectorAll('[role="menuitem"], [role="separator"]')).map((e) =>
          e.getAttribute("role") === "separator" ? "|" : (e.textContent ?? "").trim(),
        ),
      );
      expect(order.indexOf("|")).toBe(order.indexOf("What takes space in this folder?") - 1);
      expect(order.lastIndexOf("|")).toBe(order.indexOf("What takes space on this drive?") - 1);
      // One line each (as tall as "Refresh", which is one line), and the menu whole on screen.
      const line = (await items.filter({ hasText: /^Refresh$/ }).boundingBox())!.height;
      for (const item of await items.all()) {
        const h = (await item.boundingBox())!.height;
        expect(h, await item.textContent()).toBeLessThanOrEqual(line + 1);
      }
      const box = (await menu(page).boundingBox())!;
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.width).toBeLessThanOrEqual(viewport.width + 0.5);
      await shot(page, `more-menu-groups-${viewport.width}x${viewport.height}`);
      // Each opens what it says.
      await menu(page).getByRole("menuitem", { name: "See snapshots", exact: true }).click();
      await expect(page).toHaveURL(/view=snapshots/);
    });
  });
}

test.describe("phone browser (the web UI)", () => {
  test.use({
    viewport: { width: 360, height: 800 },
    hasTouch: true,
    isMobile: true,
    userAgent:
      "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36",
  });

  test("Back closes the menu, then goes up a folder, before it leaves Files (#932)", async ({ page }) => {
    // Somewhere to go back to first, as a person arrives at Files.
    await openFiles(page, { app: false });
    await main(page).getByRole("button", { name: "Actions for file-01.pkg" }).tap();
    await expect(menu(page)).toBeVisible();
    await page.goBack();
    await expect(menu(page)).toHaveCount(0);
    await expect(title(page)).toHaveText("homebrew");
    await page.goBack();
    await expect(title(page)).toHaveText("usb0");
    expect(new URL(page.url()).pathname).toBe("/files");
  });
});

test.describe("desktop width", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("keeps the page header, the drive chips and the boxes; no phone bars", async ({ page }) => {
    await openFiles(page);
    await expect(main(page).getByRole("button", { name: "Add files", exact: true })).toBeVisible();
    await expect(main(page).getByRole("button", { name: /^\/mnt\/usb0/ })).toBeVisible();
    await expect(main(page).getByTestId("fs-phone-row")).toBeHidden();
    await expect(row(page, "file-01.pkg").getByRole("checkbox")).toBeVisible();
    await row(page, "file-01.pkg").getByRole("checkbox").check();
    await expect(main(page).getByTestId("fs-selection-bar")).toBeVisible();
    await expect(actionsBar(page)).toHaveCount(0);
    // The box before the icon, the arrows, and the bell in the top row: as they were.
    const box = (await row(page, "file-01.pkg").getByRole("checkbox").boundingBox())!;
    const icon = (await row(page, "file-01.pkg").locator("svg").first().boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(icon.x);
    const arrows = page.getByRole("navigation", { name: "View history" }).locator("visible=true");
    await expect(arrows).toHaveCount(1);
    await expect(arrows.getByRole("button", { name: "Back" })).toBeVisible();
    await expect(arrows.getByRole("button", { name: "Forward" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Open notifications" }).locator("visible=true")).toHaveCount(1);
    await expect(main(page).getByTestId("fs-header").getByRole("button", { name: "Open notifications" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Reload page" })).toBeVisible();
    await shot(page, "desktop-1280x800");
  });
});

test.describe("a tablet (700x1000, touch)", () => {
  test.use({ viewport: { width: 700, height: 1000 }, hasTouch: true, isMobile: true });

  test("keeps the app's top bar with its arrows, and the page header", async ({ page }) => {
    await openFiles(page);
    const bar = page.locator("[data-top-bar]");
    await expect(bar.getByText("PS5Upload")).toBeVisible();
    await expect(bar.getByRole("button", { name: "Back" })).toBeVisible();
    await expect(bar.getByRole("button", { name: "Forward" })).toBeVisible();
    await expect(bar.getByRole("button", { name: "Open notifications" })).toBeVisible();
    await expect(main(page).getByRole("button", { name: "Add files", exact: true })).toBeVisible();
    await expect(main(page).getByTestId("fs-header").getByRole("button", { name: "Open notifications" })).toHaveCount(0);
    await expect(row(page, "file-01.pkg").getByRole("checkbox")).toBeVisible();
    await shot(page, "tablet-700x1000");
  });
});

test.describe("an iPhone (no Back button)", () => {
  test.use({
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    userAgent:
      "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1",
  });

  test("keeps the arrows, and so the app's top bar over Files", async ({ page }) => {
    await openFiles(page, { app: false });
    const bar = page.locator("[data-top-bar]");
    await expect(bar.getByRole("button", { name: "Back" })).toBeVisible();
    await expect(bar.getByRole("button", { name: "Open notifications" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Open notifications" }).locator("visible=true")).toHaveCount(1);
    await expect(title(page)).toHaveText("homebrew");
    // The arrows go back: from another screen to Files.
    await page.locator("[data-bottom-nav]").getByRole("link", { name: "More" }).tap();
    await expect(page).toHaveURL(/\/more$/);
    await bar.getByRole("button", { name: "Back" }).tap();
    await expect(page).toHaveURL(/\/files/);
    await shot(page, "iphone-390x844");
  });
});

test.describe("the app's top bar on a phone, on the other screens (#936)", () => {
  test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });

  test("a short bar with the bell and no arrows; it comes back when Files is left", async ({ page }) => {
    await page.addInitScript(() => {
      window.localStorage.setItem(
        "ps5upload.notifications.v1",
        JSON.stringify([{ id: "n1", ts: Date.now(), level: "success", title: "Upload finished", read: false }]),
      );
    });
    await openFiles(page, { app: false });
    const bar = page.locator("[data-top-bar]");
    const header = main(page).getByTestId("fs-header");
    // In Files the bell, with what is unread, is in the header.
    await expect(header.getByRole("button", { name: "Open notifications" })).toBeVisible();
    await expect(header.getByTestId("notifications-badge")).toHaveText("1");
    await expect(bar.getByText("PS5Upload")).toBeHidden();

    await page.locator("[data-bottom-nav]").getByRole("link", { name: "More" }).tap();
    await expect(page).toHaveURL(/\/more$/);
    await expect(bar.getByText("PS5Upload")).toBeVisible();
    await expect(bar.getByRole("button", { name: "Back" })).toHaveCount(0);
    const bell = bar.getByRole("button", { name: "Open notifications" });
    await expect(bell).toBeVisible();
    await expect(bell.getByTestId("notifications-badge")).toHaveText("1");
    // Short, and the bell still a comfortable tap.
    expect((await bar.boundingBox())!.height).toBeLessThanOrEqual(50);
    const b = (await bell.boundingBox())!;
    expect(b.width).toBeGreaterThanOrEqual(44);
    expect(b.height).toBeGreaterThanOrEqual(44);
    await shot(page, "top-bar-more-360x800");
    await bell.tap();
    await expect(page.getByTestId("notifications-panel").locator("visible=true")).toBeVisible();
    await shot(page, "top-bar-bell-360x800");
    await page.keyboard.press("Escape");

    // The phone's Back goes back through the screens, as the arrows did.
    await page.goBack();
    await expect(page).toHaveURL(/\/files/);
    await expect(header.getByRole("button", { name: "Open notifications" })).toBeVisible();
    await expect(bar.getByText("PS5Upload")).toBeHidden();
  });
});
