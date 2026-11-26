import { expect, test, type Page } from "@playwright/test";
import { routeEngine } from "./fixtures/engine";

// Upload & Install on a phone: the actions are one row (the main action, the rest behind ⋯)
// that sticks above the tab bar and the activity pill, not behind them, and nothing of the
// "Name on the PS5" card is left under that row: its hint and its "Name changed." line can be
// read. Until 12.6.32 three stacked buttons and the pill covered about a third of the screen.
// Tablets and computers keep their row of buttons.
//
// UPLOAD_PHONE_SHOTS=<dir> saves screenshots.

const SHOTS = process.env.UPLOAD_PHONE_SHOTS;
// A wide font, as Linux CI has (the label specs do the same).
const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";

async function open(page: Page) {
  await page.addInitScript(() => window.localStorage.setItem("ps5upload.host", "192.168.1.100"));
  let name = "A Game-example.invalid";
  await routeEngine(page, (path, req) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/ps5/status") return { ok: true };
    if (path === "/api/ps5/volumes") return { volumes: [] };
    if (path === "/api/files/upload") return { upload_id: "up-a", path: "/tmp/staged/GameA.pkg", size: 5, filename: "GameA.pkg" };
    if (path === "/api/pkg/parse-split")
      return {
        head: { content_id: "UP0000-PPSA01234_00-TESTGAME00000000", title: name, category: "gd", platform: "ps5" },
        parts: [],
        total_size: 1 << 30,
      };
    if (path === "/api/pkg/name" && req.method() === "GET")
      return {
        platform: "ps5",
        content_id: "UP0000-PPSA01234_00-TESTGAME00000000",
        name,
        entries: [{ key: "en-US", name }],
        other_names: 0,
        refusal: null,
        max_bytes: 127,
        room: { default_name: 60, all_languages: 60 },
      };
    if (path === "/api/pkg/name" && req.method() === "POST") {
      const previous = name;
      name = (req.postDataJSON() as { name: string }).name;
      return { platform: "ps5", previous, name, changed: 1, written_bytes: 2000 };
    }
    return {};
  });
  await page.goto("/send", { waitUntil: "domcontentloaded" });
  await page.addStyleTag({ content: WIDE_FONT });
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "From this device…", exact: true }).click();
  await (await chooser).setFiles([{ name: "GameA.pkg", mimeType: "application/octet-stream", buffer: Buffer.from("GameA") }]);
  await expect(page.getByTestId("pkg-name-card")).toBeVisible({ timeout: 20_000 });
}

/** The top of whatever floats over the page's bottom: the tab bar, and the activity pill. */
const floatTop = (page: Page) =>
  page.evaluate(() => {
    let top = window.innerHeight;
    for (const sel of ["[data-bottom-nav]", '[data-testid="activity-pill"]']) {
      const el = document.querySelector(sel);
      const b = el?.getBoundingClientRect();
      if (b && b.height > 0) top = Math.min(top, b.top);
    }
    return top;
  });

for (const viewport of [
  { width: 390, height: 844 },
  { width: 360, height: 740 },
  { width: 412, height: 915 },
]) {
  test.describe(`phone ${viewport.width}x${viewport.height}`, () => {
    test.use({ viewport, hasTouch: true, isMobile: true });

    test("one row of actions above the tab bar, and the name card is never under it", async ({ page }) => {
      await open(page);
      const bar = page.getByTestId("upload-actions");
      await expect(bar).toHaveAttribute("data-compact", "");
      // The main action and ⋯, in one row.
      const main = bar.getByTestId("upload-stream-install");
      await expect(main).toBeVisible();
      await expect(bar.getByRole("button", { name: "More actions" })).toBeVisible();
      await expect(bar.getByRole("button", { name: "Change source" })).toHaveCount(0);
      const box = (await bar.boundingBox())!;
      // One row (two would be about 100px).
      expect(box.height).toBeLessThan(72);
      // Not behind the tab bar or the pill.
      expect(box.y + box.height).toBeLessThanOrEqual((await floatTop(page)) + 0.5);
      // The label fits its button.
      expect(await main.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);

      // Change the name: its "Name changed." line and the hint are in view, clear of the row.
      const card = page.getByTestId("pkg-name-card");
      await card.getByTestId("pkg-name-input").fill("A Game");
      await card.getByTestId("pkg-name-save").click();
      const saved = card.getByTestId("pkg-name-saved");
      await expect(saved).toHaveText("Name changed.");
      await page.waitForTimeout(300);
      const barTop = (await bar.boundingBox())!.y;
      const savedBox = (await saved.boundingBox())!;
      expect(savedBox.y + savedBox.height).toBeLessThanOrEqual(barTop + 0.5);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/upload-${viewport.width}x${viewport.height}.png` });

      // Scrolled to the end, nothing at all is under the row or the tab bar.
      const hidden = await page.evaluate(() => {
        const root = document.querySelector<HTMLElement>("[data-scroll-root]")!;
        root.scrollTop = root.scrollHeight;
        const bar = document.querySelector('[data-testid="upload-actions"]')!.getBoundingClientRect();
        const nav = document.querySelector("[data-bottom-nav]")!.getBoundingClientRect();
        const cut: string[] = [];
        for (const el of Array.from(root.querySelectorAll<HTMLElement>("p, label, h2, h3, button, input"))) {
          if (el.closest('[data-testid="upload-actions"]')) continue;
          const b = el.getBoundingClientRect();
          if (!b.height || !b.width) continue;
          const under = (r: DOMRect) => b.bottom > r.top + 1 && b.top < r.bottom - 1;
          if (under(bar) || under(nav)) cut.push(el.textContent?.trim().slice(0, 40) ?? el.tagName);
        }
        return cut;
      });
      expect(hidden).toEqual([]);

      // The rest is behind ⋯.
      await bar.getByRole("button", { name: "More actions" }).click();
      const menu = page.getByRole("menu");
      for (const item of ["Upload & install", "Add to queue", "Change source"]) {
        await expect(menu.getByRole("menuitem", { name: new RegExp(`^${item.replace("&", "\\&")}`) })).toBeVisible();
      }
      // The menu sits whole on screen.
      const m = (await menu.boundingBox())!;
      expect(m.x).toBeGreaterThanOrEqual(0);
      expect(m.x + m.width).toBeLessThanOrEqual(viewport.width + 0.5);
      if (SHOTS) await page.screenshot({ path: `${SHOTS}/upload-${viewport.width}x${viewport.height}-menu.png` });
      await menu.getByRole("menuitem", { name: /^Change source/ }).click();
      await expect(page.getByTestId("pkg-name-card")).toHaveCount(0);
    });
  });
}

for (const viewport of [
  { width: 820, height: 1180 },
  { width: 1280, height: 800 },
]) {
  test(`${viewport.width}px keeps its row of buttons`, async ({ page }) => {
    await page.setViewportSize(viewport);
    await open(page);
    const bar = page.getByTestId("upload-actions");
    await expect(bar).not.toHaveAttribute("data-compact", "");
    for (const name of ["Change source", "Add to queue", "Upload & install", "Stream & install"]) {
      await expect(bar.getByRole("button", { name, exact: true })).toBeVisible();
    }
  });
}
