import { expect, test, type Page } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// Places where a long translation did not fit: German's "Zuletzt verwendet" in the phone's Files
// path row, German and Russian action names in the phone's selection bar, Russian's
// "Рекомендуется" in Workshop's three choices, and German's "Hochladen & installieren" under the
// sidebar's pin and hide buttons.

// A wide font, as on Linux and some Android phones: the app's own font stack falls to whatever
// the system has, and labels that fit in a Mac's letters were cut there.
const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";

async function open(page: Page, lang: string, route: string) {
  await seedConsole(page);
  await page.addInitScript((lang) => window.localStorage.setItem("ps5upload.lang", lang), lang);
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/ps5/status")
      return { ok: true, version: "12.6.31", ucred_elevated: true, ps5_kernel: "releases/10.00", instance_id: 1, command_count: 1 };
    if (path === "/api/ps5/volumes")
      return { volumes: [{ path: "/data", fs_type: "ufs", total_bytes: 825e9, free_bytes: 410e9, writable: true }] };
    if (path === "/api/ps5/list-dir")
      return { entries: [{ name: "notes.txt", kind: "file", size: 2048, mtime: 1_761_800_000 }], truncated: false };
    if (path === "/api/ps5/files/prefs") return { supported: true, favorites: [], recent: [], last: null };
    return undefined;
  });
  await page.goto(route, { waitUntil: "domcontentloaded" });
  return page.locator("[data-scroll-root]:visible");
}

test.describe("phone", () => {
  test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });

  test("Files in German: Recent is an icon, and the open folder stays in the path row", async ({ page }) => {
    await page.addInitScript(() =>
      window.localStorage.setItem("ps5upload.fs.lastPath", JSON.stringify({ "192.168.1.100": "/data/homebrew" })),
    );
    const main = await open(page, "de", "/files");
    const recent = main.getByTestId("fs-recent-open");
    await expect(recent).toBeVisible({ timeout: 60_000 });
    await expect(recent).toHaveAccessibleName(/\S/);
    await expect(recent.locator("span")).toBeHidden();
    expect((await recent.boundingBox())!.width).toBeLessThan(60);
    // The last crumb, the folder that is open, is on show in the row.
    const crumb = main.getByTestId("fs-header").getByRole("button", { name: "homebrew", exact: true });
    await expect(crumb).toBeInViewport({ ratio: 1 });
  });

  test("Files in German and Russian: the selection bar shows each action's whole name", async ({ page }) => {
    for (const [lang, wide] of [["de", false], ["ru", false], ["de", true], ["ru", true]] as const) {
      const main = await open(page, lang, "/files");
      if (wide) await page.addStyleTag({ content: WIDE_FONT });
      await main.getByTestId("fs-phone-more").click({ timeout: 60_000 });
      // "Select" in the header's menu: selection mode with nothing picked yet.
      await page.getByRole("menuitem").nth(4).click();
      const bar = page.getByTestId("fs-selection-actions-bar");
      await expect(bar).toBeVisible();
      const cut = await bar.locator("button > span").evaluateAll((els) =>
        els
          .filter((el) => el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1)
          .map((el) => el.textContent),
      );
      expect(cut, `${lang}${wide ? " (wide font)" : ""}`).toEqual([]);
      const box = (await bar.boundingBox())!;
      for (const b of await bar.locator("button").all()) {
        const r = (await b.boundingBox())!;
        expect(r.y).toBeGreaterThanOrEqual(box.y);
        expect(r.y + r.height).toBeLessThanOrEqual(box.y + box.height + 0.5);
      }
    }
  });

  test("Workshop in Russian: the three choices are one under another, each on one line", async ({ page }) => {
    const main = await open(page, "ru", "/workshop");
    const tiles = main.getByTestId("make-choice").getByRole("radio");
    await expect(tiles).toHaveCount(3, { timeout: 60_000 });
    const boxes = await Promise.all((await tiles.all()).map(async (t) => (await t.boundingBox())!));
    expect(boxes[1].y).toBeGreaterThan(boxes[0].y + boxes[0].height - 1);
    expect(boxes[2].y).toBeGreaterThan(boxes[1].y + boxes[1].height - 1);
    for (const b of boxes) expect(b.height).toBeLessThan(60);
    // Nothing inside a choice runs out of it.
    const out = await tiles.evaluateAll((els) =>
      els.filter((el) => Array.from(el.querySelectorAll("*")).some((c) => c.getBoundingClientRect().right > el.getBoundingClientRect().right + 0.5)).length,
    );
    expect(out).toBe(0);
  });
});

test.describe("computer", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("Workshop keeps its three choices side by side", async ({ page }) => {
    const main = await open(page, "en", "/workshop");
    const tiles = main.getByTestId("make-choice").getByRole("radio");
    await expect(tiles).toHaveCount(3, { timeout: 60_000 });
    const boxes = await Promise.all((await tiles.all()).map(async (t) => (await t.boundingBox())!));
    expect(Math.abs(boxes[0].y - boxes[2].y)).toBeLessThan(2);
    expect(boxes[1].x).toBeGreaterThan(boxes[0].x + boxes[0].width);
  });

  test("Files keeps the word Recent", async ({ page }) => {
    const main = await open(page, "en", "/files");
    await expect(main.getByTestId("fs-recent-open")).toHaveText("Recent", { timeout: 60_000 });
  });

  test("the sidebar in German: a long name ends before the row's pin and hide buttons", async ({ page }) => {
    await open(page, "de", "/home");
    const link = page.getByTestId("sidebar-link").filter({ hasText: "Hochladen & installieren" }).first();
    await expect(link).toBeVisible({ timeout: 60_000 });
    await link.hover();
    const row = link.locator("xpath=..");
    const pin = row.getByRole("button").first();
    await expect(pin).toBeVisible();
    const label = link.locator("span.truncate");
    const l = (await label.boundingBox())!;
    const p = (await pin.boundingBox())!;
    expect(l.x + l.width).toBeLessThanOrEqual(p.x + 0.5);
  });
});
