import { expect, test, type Locator, type Page, type Route } from "@playwright/test";

// #936, the reporter's last two asks on a phone:
// - The Files header is pinned for good: it is outside what scrolls (the list under it is the only
//   scroller), so dragging the list past its ends (Android's overscroll stretch) cannot move or
//   stretch it. Playwright has no stretch; what is checked is where the header sits in the page and
//   that its box does not change when the list is scrolled to its end and further.
// - What is staged to copy or move shows in a bar at the bottom (Paste here, Cancel) instead of a
//   banner over the list; it goes once the paste is done (a copy as well as a cut, and when the
//   source is deleted), a toast says so and goes by itself, and Back keeps it while going up folders
//   and lets go of it at the drive's root.
// Tablets and computers keep the banner, which now goes after a paste as well.

const HOST = "192.168.0.5";
const START = "/mnt/usb0/homebrew";
const FILES = Array.from({ length: 40 }, (_, i) => `file-${String(i + 1).padStart(2, "0")}.pkg`);
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
const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";

interface FsOp {
  op: string;
  body: Record<string, unknown>;
}

/** Files in the Android app with a fake engine. `failCopy`: the console refuses copies. Returns
 *  the file operations the engine was asked for. */
async function openFiles(page: Page, opts: { failCopy?: boolean; wideFont?: boolean } = {}): Promise<FsOp[]> {
  const ops: FsOp[] = [];
  await page.addInitScript(
    ({ listings, volumes }) => {
      const w = window as unknown as Record<string, unknown>;
      let id = 0;
      w.__TAURI_INTERNALS__ = {
        invoke: async (cmd: string, args?: { path?: string }) => {
          if (cmd === "ps5_list_dir")
            return {
              // What a delete took is gone from the listing (`__gone`, set by the engine's fake).
              entries: (listings[args?.path ?? ""] ?? [])
                .filter((e) => !((w.__gone as string[] | undefined) ?? []).includes(`${args?.path}/${e.name}`))
                .map((e) => ({ ...e, size: 1000, mtime: 1791360199 })),
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
  await page.addInitScript(
    ({ host, start }) => {
      window.localStorage.setItem("ps5upload.host", host);
      window.localStorage.setItem("ps5upload.fs.lastPath", JSON.stringify({ [host]: start }));
    },
    { host: HOST, start: START },
  );
  const json = (route: Route, body: unknown, status = 200) =>
    route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
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
      const fsOp = /^\/api\/ps5\/fs\/(copy|move|delete)$/.exec(p);
      if (fsOp) {
        const body = route.request().postDataJSON() as Record<string, unknown>;
        ops.push({ op: fsOp[1], body });
        if (fsOp[1] === "delete")
          await page.evaluate((gone) => {
            const w = window as unknown as { __gone?: string[] };
            w.__gone = [...(w.__gone ?? []), gone];
          }, String(body.path));
        if (opts.failCopy && fsOp[1] === "copy") return json(route, { error: "fs_copy_failed: no space left" }, 500);
        return json(route, { ok: true });
      }
      if (p === "/api/jobs") return json(route, []);
      return json(route, {});
    },
  );
  await page.goto("/files", { waitUntil: "domcontentloaded" });
  await expect(row(page, "file-01.pkg")).toBeVisible({ timeout: 30_000 });
  if (opts.wideFont) await page.addStyleTag({ content: WIDE_FONT });
  return ops;
}

const main = (page: Page) => page.getByRole("main");
const title = (page: Page) => main(page).getByTestId("fs-phone-title");
const row = (page: Page, name: string) => main(page).locator(`li[data-fs-name="${name}"]`);
const header = (page: Page) => main(page).getByTestId("fs-header");
const actionsBar = (page: Page) => main(page).getByRole("toolbar", { name: "Actions for the selected items" });
const stagedBar = (page: Page) => main(page).getByRole("toolbar", { name: "Items to paste" });
const banner = (page: Page) => main(page).getByTestId("fs-staged-banner");
const menu = (page: Page) => page.getByTestId("fs-row-menu");
const back = (page: Page) => page.evaluate(() => window.__ps5uBack?.() ?? null);

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

/** Until a scroller has stopped moving (a fling goes on after the finger lifts). */
async function settle(scroller: Locator) {
  let last = -1;
  await expect
    .poll(async () => {
      const now = await scroller.evaluate((el) => el.scrollTop);
      const still = now === last;
      last = now;
      return still;
    }, { intervals: [250] })
    .toBe(true);
}

async function shot(page: Page, name: string) {
  const dir = process.env.FILES_PHONE_SHOTS;
  if (dir) await page.screenshot({ path: `${dir}/${name}.png` });
}

/** Stage `names` for `op` the way a phone does: a long press, then Copy on the bar or Cut in More. */
async function stage(page: Page, op: "copy" | "cut", names: string[]) {
  await longPress(page, row(page, names[0]));
  for (const name of names.slice(1)) await row(page, name).tap();
  if (op === "copy") await actionsBar(page).getByRole("button", { name: "Copy", exact: true }).tap();
  else {
    await actionsBar(page).getByRole("button", { name: "More", exact: true }).tap();
    await menu(page).getByRole("menuitem", { name: "Cut", exact: true }).click();
  }
  await expect(actionsBar(page)).toHaveCount(0);
}

async function openGames(page: Page) {
  await row(page, "games").getByRole("button", { name: "games", exact: true }).tap();
  await expect(title(page)).toHaveText("games");
  await expect(row(page, "inner.pkg")).toBeVisible();
}

const PHONES = [
  { width: 360, height: 800 },
  { width: 844, height: 390 },
];

for (const viewport of PHONES) {
  for (const wideFont of [false, true]) {
    const tag = `${viewport.width}x${viewport.height}${wideFont ? " wide font" : ""}`;
    const file = `${viewport.width}x${viewport.height}${wideFont ? "-wide" : ""}`;
    test.describe(`phone ${tag}`, () => {
      test.use({ viewport, hasTouch: true, isMobile: true });

      test("the header is outside what scrolls and stays put at and past the list's end", async ({ page }) => {
        await openFiles(page, { wideFont });
        const list = main(page).locator("[data-files-scroll]");
        // The list is the scroller: it scrolls, the page around it does not.
        expect(await list.evaluate((el) => getComputedStyle(el).overflowY)).toBe("auto");
        expect(await list.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(true);
        // No ancestor of the header scrolls (or could be stretched with it).
        const scrollingAncestors = await header(page).evaluate((h) => {
          const out: string[] = [];
          for (let p = h.parentElement; p; p = p.parentElement) {
            const o = getComputedStyle(p).overflowY;
            if ((o === "auto" || o === "scroll") && p.scrollHeight > p.clientHeight) out.push(p.className);
          }
          return out;
        });
        expect(scrollingAncestors).toEqual([]);
        // The room under the end is the list's own now (where the bottom bar floats), not the page's.
        expect(await page.locator("[data-scroll-root]").evaluate((el) => getComputedStyle(el).paddingBottom)).toBe("0px");
        if (viewport.width < 768)
          expect(parseFloat(await list.evaluate((el) => getComputedStyle(el).paddingBottom))).toBeGreaterThanOrEqual(88);
        expect(await header(page).evaluate((h) => h.closest("[data-files-scroll]") === null)).toBe(true);
        // Keyboard and screen-reader order: the header, then the list.
        expect(
          await header(page).evaluate(
            (h, l) => !!(h.compareDocumentPosition(l!) & Node.DOCUMENT_POSITION_FOLLOWING),
            await list.elementHandle(),
          ),
        ).toBe(true);

        const before = (await header(page).boundingBox())!;
        // To the end, then a finger dragging further up, past it (an overscroll on a phone).
        await list.evaluate((el) => (el.scrollTop = el.scrollHeight));
        await expect(row(page, "file-40.pkg")).toBeInViewport();
        const l = (await list.boundingBox())!;
        const cdp = await page.context().newCDPSession(page);
        await cdp.send("Input.synthesizeScrollGesture", {
          x: Math.round(l.x + l.width / 2),
          y: Math.round(l.y + l.height / 2),
          yDistance: -600,
          gestureSourceType: "touch",
          speed: 2000,
        });
        expect((await header(page).boundingBox())!).toEqual(before);
        // And down past the top.
        await list.evaluate((el) => (el.scrollTop = 0));
        await cdp.send("Input.synthesizeScrollGesture", {
          x: Math.round(l.x + l.width / 2),
          y: Math.round(l.y + l.height / 2),
          yDistance: 600,
          gestureSourceType: "touch",
          speed: 2000,
        });
        await cdp.detach();
        expect((await header(page).boundingBox())!).toEqual(before);
        // The fling after the finger lifts has ended: the header is where it was all along.
        await settle(list);
        expect((await header(page).boundingBox())!).toEqual(before);
        expect(await page.locator("[data-scroll-root]").evaluate((el) => el.scrollTop)).toBe(0);
        // At the end, what is last in the list clears the bottom navigation.
        // (Twice: what the font changes under it can still move the end once.)
        for (let i = 0; i < 2; i++) {
          await list.evaluate((el) => (el.scrollTop = el.scrollHeight));
          await settle(list);
        }
        expect(await list.evaluate((el) => Math.abs(el.scrollTop + el.clientHeight - el.scrollHeight))).toBeLessThan(2);
        const last = (await main(page).getByTestId("fs-list-area").boundingBox())!;
        const nav = page.locator("[data-bottom-nav]");
        if (await nav.isVisible()) expect(last.y + last.height).toBeLessThanOrEqual((await nav.boundingBox())!.y);
        await shot(page, `pinned-end-${file}`);
      });

      test("copy: staged in a bar at the bottom, pasted, then the bar goes and a toast says so", async ({ page }) => {
        const ops = await openFiles(page, { wideFont });
        await stage(page, "copy", ["file-01.pkg"]);
        const bar = stagedBar(page);
        await expect(bar).toBeInViewport();
        await expect(bar.getByTestId("fs-staged-count")).toContainText("Items to copy: 1");
        await expect(bar.getByTestId("fs-staged-count")).toContainText(`From ${START}`);
        // The count whole, in a wide font too (two lines if it must).
        expect(
          await bar
            .getByTestId("fs-staged-what")
            .evaluate((el) => el.scrollHeight <= el.clientHeight + 1 && el.scrollWidth <= el.clientWidth + 1),
        ).toBe(true);
        // Not the banner over the list, and the bar is where the navigation was.
        await expect(banner(page)).toBeHidden();
        await expect(page.locator("[data-bottom-nav]")).toBeHidden();
        // Its buttons fit, each label whole.
        for (const name of ["Cancel", "Paste here"]) {
          const b = bar.getByRole("button", { name, exact: true });
          await expect(b).toBeVisible();
          expect(await b.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
        }
        const bb = (await bar.boundingBox())!;
        expect(bb.x).toBeGreaterThanOrEqual(0);
        expect(bb.x + bb.width).toBeLessThanOrEqual(viewport.width);
        expect(bb.y + bb.height).toBeLessThanOrEqual(viewport.height);
        // Somewhere else, Paste here.
        await openGames(page);
        await expect(bar).toBeVisible();
        await shot(page, `staged-copy-${file}`);
        await bar.getByRole("button", { name: "Paste here", exact: true }).tap();
        await expect(bar).toHaveCount(0);
        const toast = page.getByText("Items copied: 1", { exact: true });
        await expect(toast).toBeVisible();
        await shot(page, `pasted-toast-${file}`);
        expect(ops).toEqual([
          {
            op: "copy",
            body: expect.objectContaining({ from: `${START}/file-01.pkg`, to: `${START}/games/file-01.pkg` }),
          },
        ]);
        // Nothing left to paste: not in the bar, not in the folder's menu.
        await expect(banner(page)).toBeHidden();
        if (viewport.width < 768) await expect(page.locator("[data-bottom-nav]")).toBeVisible();
        await header(page).getByRole("button", { name: "More" }).tap();
        await expect(menu(page).getByRole("menuitem", { name: /^Paste/ })).toHaveCount(0);
        expect(await back(page)).toBe(true);
        // The toast goes by itself.
        await expect(toast).toHaveCount(0, { timeout: 8_000 });
      });

      test("cut: pasted items leave the clipboard; Back keeps what is staged up to the drive's root", async ({ page }) => {
        const ops = await openFiles(page, { wideFont });
        await stage(page, "cut", ["file-02.pkg", "file-03.pkg"]);
        const bar = stagedBar(page);
        await expect(bar.getByTestId("fs-staged-count")).toContainText("Items to move: 2");
        await openGames(page);
        await bar.getByRole("button", { name: "Paste here", exact: true }).tap();
        await expect(bar).toHaveCount(0);
        await expect(page.getByText("Items moved: 2", { exact: true })).toBeVisible();
        expect(ops.map((o) => [o.op, o.body.from])).toEqual([
          ["move", `${START}/file-02.pkg`],
          ["move", `${START}/file-03.pkg`],
        ]);

        // Back: the staged items go first, and the folder stays.
        expect(await back(page)).toBe(true);
        await expect(title(page)).toHaveText("homebrew");
        await stage(page, "copy", ["file-04.pkg"]);
        await expect(bar).toBeVisible();
        // Back goes up a folder and keeps what is staged: that is how one gets to the folder to
        // paste into.
        expect(await back(page)).toBe(true);
        await expect(title(page)).toHaveText("usb0");
        await expect(bar).toBeVisible();
        // At the drive's root, Back lets go of it, and the folder stays.
        expect(await back(page)).toBe(true);
        await expect(bar).toHaveCount(0);
        await expect(title(page)).toHaveText("usb0");
        if (viewport.width < 768) await expect(page.locator("[data-bottom-nav]")).toBeVisible();

        // Cancel on the bar lets go too.
        await row(page, "homebrew").getByRole("button", { name: "homebrew", exact: true }).tap();
        await stage(page, "copy", ["file-05.pkg"]);
        await bar.getByRole("button", { name: "Cancel", exact: true }).tap();
        await expect(bar).toHaveCount(0);
      });

      test("a staged item deleted before the paste leaves the bar; a failed paste stays staged", async ({ page }) => {
        const ops = await openFiles(page, { failCopy: true, wideFont });
        await stage(page, "copy", ["file-06.pkg"]);
        await expect(stagedBar(page)).toBeVisible();
        // Deleted: there is nothing left to paste.
        await longPress(page, row(page, "file-06.pkg"));
        await actionsBar(page).getByRole("button", { name: "Delete", exact: true }).tap();
        await page.getByRole("alertdialog").getByRole("button", { name: "Delete", exact: true }).tap();
        await expect.poll(() => ops.map((o) => o.op)).toEqual(["delete"]);
        await expect(row(page, "file-06.pkg")).toHaveCount(0);
        await expect(stagedBar(page)).toHaveCount(0);

        // The console refuses the copy: still staged, to try again, and a toast says so.
        await stage(page, "copy", ["file-07.pkg"]);
        await openGames(page);
        await stagedBar(page).getByRole("button", { name: "Paste here", exact: true }).tap();
        await expect(page.getByText("Items not pasted: 1", { exact: true })).toBeVisible();
        await expect(stagedBar(page).getByTestId("fs-staged-count")).toContainText("Items to copy: 1");
      });
    });
  }
}

// A tablet or a computer keeps the banner over the list, and it goes after a paste too.
test.describe("wide window", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("the banner stays a banner, and goes once the paste is done", async ({ page }) => {
    const ops = await openFiles(page);
    await row(page, "file-08.pkg").getByRole("checkbox").check();
    await main(page).getByRole("button", { name: "Copy", exact: true }).click();
    await expect(banner(page)).toBeVisible();
    await expect(stagedBar(page)).toBeHidden();
    // The list is not a scroller of its own here.
    expect(await main(page).locator("[data-files-scroll]").evaluate((el) => getComputedStyle(el).overflowY)).toBe(
      "visible",
    );
    await row(page, "games").getByRole("button", { name: "games", exact: true }).click();
    await expect(row(page, "inner.pkg")).toBeVisible();
    await banner(page).getByRole("button", { name: "Paste here" }).click();
    await expect(banner(page)).toHaveCount(0);
    await expect(page.getByText("Items copied: 1", { exact: true })).toBeVisible();
    expect(ops.map((o) => o.op)).toEqual(["copy"]);
  });
});
