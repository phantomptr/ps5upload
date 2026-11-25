import { expect, test, type Page, type Route } from "@playwright/test";

// #923: on a phone, long file names were cut off after a few characters because the tag, size and
// buttons share the name's line. There the name now wraps and the tag and size sit below it; wider
// windows keep one line per row. #931: a phone held sideways is wider than `sm`, but it keeps the
// phone layout too (wrapped name, size below, actions behind ⋮), not the desktop row of buttons.

const HOST = "192.168.0.5";
const LONG = "Dead.or.Alive.5.Last.Round.CUSA01778.v1.19.All.DLC.Backport.Fixed.pkg";
const FILES = [LONG, "b-second.bin"];
// Too long for one line even across a phone held sideways.
const HUGE =
  "Marvels.Spider-Man.2.Digital.Deluxe.Edition.PPSA03016.v1.004.000.All.DLC.Backport.Fixed.By.Somebody.With.A.Very.Long.Release.Group.Name.exfat";

async function openFiles(page: Page, files: string[] = FILES) {
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
  }, files);
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
  const name = page.getByRole("main").getByRole("button", { name: LONG, exact: true });
  await expect(name).toBeVisible({ timeout: 30_000 });
  return name;
}

test.describe("phone width", () => {
  test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });

  test("a long file name wraps instead of being cut off, with its size below (#923)", async ({ page }) => {
    const name = await openFiles(page);
    const fits = await name.evaluate((el) => el.scrollWidth <= el.clientWidth + 1);
    expect(fits).toBe(true);
    const nameBox = await name.boundingBox();
    const meta = name.locator("xpath=following-sibling::div[1]");
    const metaBox = await meta.boundingBox();
    if (!nameBox || !metaBox) throw new Error("no box");
    // More than one line of text, and the size line starts below the name.
    expect(nameBox.height).toBeGreaterThan(24);
    expect(metaBox.y).toBeGreaterThanOrEqual(nameBox.y + nameBox.height - 1);
    await expect(meta).toContainText("1000 B");
    if (process.env.FS_SHOT) await page.screenshot({ path: process.env.FS_SHOT });
  });
});

async function expectPhoneRow(page: Page, name: ReturnType<Page["getByRole"]>) {
  const fits = await name.evaluate((el) => el.scrollWidth <= el.clientWidth + 1);
  expect(fits).toBe(true);
  const nameBox = await name.boundingBox();
  const meta = name.locator("xpath=following-sibling::div[1]");
  const metaBox = await meta.boundingBox();
  if (!nameBox || !metaBox) throw new Error("no box");
  expect(nameBox.height).toBeGreaterThan(24);
  expect(metaBox.y).toBeGreaterThanOrEqual(nameBox.y + nameBox.height - 1);
  await expect(meta).toContainText("1000 B");
  // No inline action buttons in this row: only the ⋮, which holds them all.
  const row = name.locator("xpath=ancestor::li[1]");
  await expect(row.getByTestId("fs-row-actions")).toBeHidden();
  const visibleButtons = await row
    .locator("button:visible")
    .evaluateAll((els) => els.map((el) => el.getAttribute("aria-label") ?? el.getAttribute("title") ?? ""));
  expect(visibleButtons).toEqual([LONG, `Actions for ${LONG}`]);
  const more = row.getByRole("button", { name: `Actions for ${LONG}` });
  await more.click();
  const menu = page.getByTestId("fs-row-menu");
  await expect(menu.getByRole("menuitem", { name: "Rename", exact: true })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Delete", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
}

for (const viewport of [
  { width: 915, height: 412 },
  { width: 844, height: 390 },
]) {
  test.describe(`phone held sideways ${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport, hasTouch: true, isMobile: true });

    test("keeps the phone row: the name wraps, size below, actions behind ⋮ (#931)", async ({ page }) => {
      const name = await openFiles(page, [...FILES, HUGE]);
      expect(await page.evaluate(() => matchMedia("(pointer: coarse)").matches)).toBe(true);
      await expectPhoneRow(page, name);
      // A name too long for one line really wraps (the 44 px touch height alone is not proof).
      const huge = page.getByRole("main").getByRole("button", { name: HUGE, exact: true });
      const lines = await huge.evaluate((el) => {
        const style = getComputedStyle(el);
        return {
          lines: el.getBoundingClientRect().height / parseFloat(style.lineHeight),
          fits: el.scrollWidth <= el.clientWidth + 1,
          whiteSpace: style.whiteSpace,
        };
      });
      expect(lines.fits).toBe(true);
      expect(lines.whiteSpace).toBe("normal");
      expect(lines.lines).toBeGreaterThanOrEqual(1.9);
      if (process.env.FS_SHOT) await page.screenshot({ path: process.env.FS_SHOT });
    });
  });
}

test.describe("desktop width", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("rows stay one line, the size beside the name", async ({ page }) => {
    const name = await openFiles(page);
    const nameBox = await name.boundingBox();
    const size = name.locator("xpath=following-sibling::div[1]").getByText("1000 B");
    const sizeBox = await size.boundingBox();
    if (!nameBox || !sizeBox) throw new Error("no box");
    expect(nameBox.height).toBeLessThan(28);
    expect(Math.abs(sizeBox.y + sizeBox.height / 2 - (nameBox.y + nameBox.height / 2))).toBeLessThan(6);
    // The actions stay buttons on the row, and there is no ⋮.
    const row = name.locator("xpath=ancestor::li[1]");
    await expect(row.getByTestId("fs-row-actions")).toBeVisible();
    await expect(row.getByRole("button", { name: "Rename", exact: true })).toBeVisible();
    await expect(row.getByTestId("fs-row-more")).toBeHidden();
  });
});
