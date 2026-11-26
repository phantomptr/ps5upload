import { expect, test, type Locator, type Page } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// #936: on a phone, Files' New folder, Rename, Go to path and Download from a link put their
// field on a line of its own across the screen, with the buttons under it, each on one line. In
// one row the name box was a few pixels wide and "Create" / "Cancel" broke mid-word. Also in a
// wide font and in German and Russian. Enter does it, Back closes it. A computer keeps its row.

const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";
const PHONE_UA =
  "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36";
const FILE = "notes.txt";
const TYPED = "My new folder for the games";
/** Screenshots go here when set (a folder of the one running the suite). */
const SHOTS = process.env.PS5UPLOAD_E2E_SHOTS;

const NEW_FOLDER: Record<string, string> = { en: "New folder", de: "Neuer Ordner", ru: "Новая папка" };
const LINK: Record<string, string> = {
  en: "Download from a link",
  de: "Von einem Link herunterladen",
  ru: "Скачать по ссылке",
};

async function open(page: Page, lang: string, mkdirs: string[] = []) {
  await seedConsole(page);
  await page.addInitScript((lang) => {
    window.localStorage.setItem("ps5upload.lang", lang);
    window.localStorage.setItem("ps5upload.fs.lastPath", JSON.stringify({ "192.168.1.100": "/mnt/usb0" }));
  }, lang);
  await routeEngine(page, (path, req) => {
    if (path === "/api/version") return { version: "12.6.34", caps: {} };
    if (path === "/api/ps5/status")
      return { ok: true, version: "12.6.34", ucred_elevated: true, ps5_kernel: "releases/10.00", instance_id: 1, command_count: 1 };
    if (path === "/api/ps5/volumes")
      return {
        volumes: [{ path: "/mnt/usb0", fs_type: "exfatfs", total_bytes: 1e12, free_bytes: 5e11, writable: true }],
      };
    if (path === "/api/ps5/list-dir")
      return { entries: [{ name: FILE, kind: "file", size: 2048, mtime: 1_761_800_000 }], truncated: false };
    if (path === "/api/ps5/files/prefs") return { supported: true, favorites: [], recent: [], last: null };
    if (path === "/api/ps5/fs/mkdir") mkdirs.push((req.postDataJSON() as { path: string }).path);
    return undefined;
  });
  await page.goto("/files", { waitUntil: "domcontentloaded" });
  return page.getByRole("main");
}

/** Lines each button's label takes (1 = whole on one line). */
async function labelLines(buttons: Locator): Promise<Record<string, number>> {
  return buttons.evaluateAll((els) =>
    Object.fromEntries(
      els.map((b) => {
        const r = document.createRange();
        r.selectNodeContents(b);
        const tops = new Set(Array.from(r.getClientRects()).map((c) => Math.round(c.top)));
        return [(b.textContent ?? "").trim(), tops.size];
      }),
    ),
  );
}

/** The field is most of the screen's width and what was typed shows. */
async function expectWideField(field: Locator, width: number, min = 0.7) {
  const box = (await field.boundingBox())!;
  expect(box.width, "field width").toBeGreaterThanOrEqual(width * min);
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(width + 0.5);
  await expect(field).toBeInViewport({ ratio: 1 });
  // Where the typing is shows: the whole value, or (a long link in a wide font) its end.
  const endShows = await field.evaluate(
    (el: HTMLInputElement) => el.scrollLeft + el.clientWidth >= el.scrollWidth - 1,
  );
  expect(endShows, "the end of what was typed shows").toBe(true);
}

async function shot(page: Page, name: string) {
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}.png` });
}

for (const viewport of [
  { width: 360, height: 800 },
  { width: 390, height: 844 },
  { width: 844, height: 390 },
]) {
  test.describe(`phone ${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport, hasTouch: true, isMobile: true, userAgent: PHONE_UA });

    for (const [lang, wide] of [["en", false], ["de", false], ["de", true], ["ru", true]] as const) {
      const tag = `${viewport.width}x${viewport.height}-${lang}${wide ? "-wide" : ""}`;

      test(`New folder: a full-width name, buttons whole, Enter makes it, Back closes it (${lang}${wide ? ", wide font" : ""})`, async ({
        page,
      }) => {
        const mkdirs: string[] = [];
        const main = await open(page, lang, mkdirs);
        if (wide) await page.addStyleTag({ content: WIDE_FONT });
        await main.getByTestId("fs-phone-more").click({ timeout: 60_000 });
        await page.getByRole("menuitem", { name: NEW_FOLDER[lang], exact: true }).click();
        const card = main.getByTestId("fs-mkdir");
        const field = card.getByTestId("fs-mkdir-name");
        await expect(field).toBeFocused();
        await field.fill(lang === "en" ? TYPED : "Neuer Ordner für die Spiele");
        // Narrower in landscape: the card shares the width with nothing, but a 844 px screen is wide.
        await expectWideField(field, viewport.width, viewport.width > 600 ? 0.6 : 0.7);
        const lines = await labelLines(card.getByRole("button"));
        for (const [label, n] of Object.entries(lines)) expect(n, label).toBe(1);
        // Under the field, not beside it.
        const fieldBox = (await field.boundingBox())!;
        for (const b of await card.getByRole("button").all())
          expect((await b.boundingBox())!.y).toBeGreaterThan(fieldBox.y + fieldBox.height - 1);
        await shot(page, `mkdir-${tag}`);

        await page.goBack();
        await expect(card).toHaveCount(0);
        expect(new URL(page.url()).pathname).toBe("/files");

        await main.getByTestId("fs-phone-more").click();
        await page.getByRole("menuitem", { name: NEW_FOLDER[lang], exact: true }).click();
        await card.getByTestId("fs-mkdir-name").fill("games");
        await card.getByTestId("fs-mkdir-name").press("Enter");
        await expect.poll(() => mkdirs).toEqual(["/mnt/usb0/games"]);
      });

      test(`Rename, Go to path and Download from a link: a wide field (${lang}${wide ? ", wide font" : ""})`, async ({
        page,
      }) => {
        const main = await open(page, lang);
        if (wide) await page.addStyleTag({ content: WIDE_FONT });

        // Rename, from the row's ⋮.
        await main.getByTestId("fs-row-end").getByRole("button").first().click({ timeout: 60_000 });
        await page.getByRole("menuitem").first().click();
        const rename = main.getByTestId("fs-rename-input");
        await expect(rename).toBeFocused();
        await rename.fill("notes from the console.txt");
        await expectWideField(rename, viewport.width, 0.6);
        await shot(page, `rename-${tag}`);
        await page.goBack();
        await expect(rename).toHaveCount(0);

        // Go to path: the pencil in the path row.
        await main.getByTestId("fs-path-edit").click();
        const pathField = main.getByTestId("fs-path-input");
        await expect(pathField).toBeFocused();
        await pathField.fill("/mnt/usb0/homebrew");
        await expectWideField(pathField, viewport.width, 0.6);
        await shot(page, `path-${tag}`);
        await page.goBack();
        await expect(pathField).toHaveCount(0);

        // Download from a link, from the page's ⋮.
        await main.getByTestId("fs-phone-more").click();
        await page.getByRole("menuitem", { name: LINK[lang], exact: true }).click();
        const card = main.getByTestId("fs-link-download");
        const url = card.getByTestId("fs-link-url");
        await expect(url).toBeFocused();
        await url.fill("https://example.com/game.pkg");
        await expectWideField(url, viewport.width, viewport.width > 600 ? 0.6 : 0.7);
        const lines = await labelLines(card.getByRole("button"));
        for (const [label, n] of Object.entries(lines)) expect(n, label).toBe(1);
        await shot(page, `link-${tag}`);
        await page.goBack();
        await expect(card).toHaveCount(0);
      });
    }
  });
}

test.describe("desktop width", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("New folder keeps its one row", async ({ page }) => {
    const main = await open(page, "en");
    await main.getByRole("button", { name: "New folder", exact: true }).first().click({ timeout: 60_000 });
    const card = main.getByTestId("fs-mkdir");
    const field = card.getByTestId("fs-mkdir-name");
    await expect(field).toBeFocused();
    const fieldBox = (await field.boundingBox())!;
    for (const b of await card.getByRole("button").all()) {
      const box = (await b.boundingBox())!;
      expect(Math.abs(box.y + box.height / 2 - (fieldBox.y + fieldBox.height / 2))).toBeLessThan(4);
    }
    await shot(page, "mkdir-desktop-1280");
  });
});
