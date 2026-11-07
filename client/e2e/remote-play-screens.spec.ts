import { expect, test, type Page } from "@playwright/test";
import { routeEngine, seedConsole } from "./fixtures/engine";

// The on-screen controller and dialogs on phones and tablets. An iPhone Duo has a column of system
// icons down one side, reported as a safe-area inset on that side only (sizes and insets read off
// a real one, #675); the rest are common sizes, with the insets simulated.

async function insets(page: Page, i: { top: number; right: number; bottom: number; left: number }) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setSafeAreaInsetsOverride" as never, { insets: i } as never);
}

async function mockConsole(page: Page) {
  await seedConsole(page);
  await routeEngine(page, (path) => {
    let body: unknown = {};
    if (path === "/api/version") body = { version: "8.11.0", caps: {} };
    if (path === "/api/ps5/rp/session") body = { host: null };
    return body;
  });
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });
}

async function playFull(page: Page) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  const stage = page.getByTestId("rp-stage");
  // A phone held sideways goes full screen by itself.
  const fills = await expect(stage)
    .toHaveAttribute("data-full", "immersive", { timeout: 2000 })
    .then(() => true)
    .catch(() => false);
  if (!fills) await page.getByTestId("rp-fullscreen").tap();
  await expect(stage).toHaveAttribute("data-full", "immersive");
  await expect(page.getByTestId("rp-touch")).toBeVisible();
}

type Box = { name: string; x: number; y: number; right: number; bottom: number };

/** Where the controller's parts are drawn. */
const parts = (page: Page) =>
  page.evaluate(() => {
    const group = (name: string, label: string): Box => {
      const el = document.querySelector(`[data-testid="rp-touch"] [aria-label="${label}"]`)!;
      const r = el.getBoundingClientRect();
      return { name, x: r.left, y: r.top, right: r.right, bottom: r.bottom };
    };
    const seen: Box[] = [];
    for (const b of document.querySelectorAll('[data-testid="rp-touch"] button, [data-testid="rp-touch"] [aria-label]')) {
      const name = b.getAttribute("aria-label");
      if (!name) continue;
      seen.push(group(name, name));
    }
    const t = document.querySelector('[data-testid="rp-touch-mode"]')!.getBoundingClientRect();
    seen.push({ name: "mode", x: t.left, y: t.top, right: t.right, bottom: t.bottom });
    return seen;
  });

const overlap = (a: Box, b: Box) =>
  a.x < b.right - 1 && b.x < a.right - 1 && a.y < b.bottom - 1 && b.y < a.bottom - 1;

// An iPhone Duo folded and open, and a spread of other phones and tablets, each way up.
const SCREENS = [
  { name: "Duo folded", width: 466, height: 678, i: { top: 44, right: 93, bottom: 20, left: 0 } },
  { name: "Duo open", width: 669, height: 951, i: { top: 44, right: 0, bottom: 20, left: 0 } },
  { name: "small phone", width: 360, height: 640, i: { top: 24, right: 0, bottom: 0, left: 0 } },
  { name: "Android phone", width: 412, height: 915, i: { top: 32, right: 0, bottom: 24, left: 0 } },
  { name: "iPhone", width: 393, height: 852, i: { top: 59, right: 0, bottom: 34, left: 0 } },
  { name: "iPhone Pro Max", width: 440, height: 956, i: { top: 62, right: 0, bottom: 34, left: 0 } },
  { name: "foldable phone open", width: 673, height: 841, i: { top: 28, right: 0, bottom: 16, left: 0 } },
  { name: "iPad mini", width: 744, height: 1133, i: { top: 24, right: 0, bottom: 20, left: 0 } },
  { name: "iPad", width: 820, height: 1180, i: { top: 24, right: 0, bottom: 20, left: 0 } },
  { name: "iPad Pro", width: 1024, height: 1366, i: { top: 24, right: 0, bottom: 20, left: 0 } },
];

/** Turned a quarter: the top inset goes to one side (a notch), the home bar stays at the bottom. */
const sideways = (s: (typeof SCREENS)[number]) => ({
  name: `${s.name} sideways`,
  width: s.height,
  height: s.width,
  i: s.name.startsWith("Duo")
    ? { top: 0, right: s.i.right || 62, bottom: s.i.bottom, left: 0 }
    : { top: 0, right: s.i.top > 40 ? s.i.top : 0, bottom: s.i.bottom ? 20 : 0, left: s.i.top > 40 ? s.i.top : 0 },
});

/** A hit area reaches past a key drawn small: what counts is the middle of it. */
const shrunk = (b: Box): Box => ({ ...b, x: b.x + 8, right: b.right - 8, y: b.y + 8, bottom: b.bottom - 8 });

for (const screen of [...SCREENS, ...SCREENS.map(sideways)]) {
  test.describe(screen.name, () => {
    test.use({ viewport: { width: screen.width, height: screen.height }, hasTouch: true, isMobile: true });

    test("nothing on the controller sits on anything else, and all of it is on screen and clear of the insets", async ({ page }) => {
      await mockConsole(page);
      await insets(page, screen.i);
      await playFull(page);
      await page.waitForTimeout(200);
      const all = await parts(page);
      expect(all.length).toBeGreaterThan(15);
      const clashes: string[] = [];
      for (const b of all.filter((p) => /stick|mode/i.test(p.name))) {
        for (const o of all) {
          // The switch sits on the touchpad strip by design.
          if (b.name === "mode" && o.name === "Touchpad") continue;
          if (o !== b && overlap(b, o)) clashes.push(`${b.name} on ${o.name}`);
        }
      }
      // The keys that make up the two clusters and the row at the bottom.
      const keys = all.filter((p) => /^(L1|L2|R1|R2|↑|↓|←|→|△|□|○|✕|Create|Options|PS)$/.test(p.name));
      for (const b of keys) {
        for (const o of keys) {
          if (b.name < o.name && overlap(shrunk(b), shrunk(o))) clashes.push(`${b.name} on ${o.name}`);
        }
      }
      expect(clashes).toEqual([]);
      for (const p of all) {
        // Sideways the shoulder buttons are turned a little toward the thumbs, and stick out by that.
        const turned = /^[LR][12]$/.test(p.name) ? 12 : 1;
        expect(p.right, `${p.name}`).toBeLessThanOrEqual(screen.width - screen.i.right + turned);
        expect(p.x, `${p.name}`).toBeGreaterThanOrEqual(screen.i.left - turned);
        expect(p.bottom, `${p.name}`).toBeLessThanOrEqual(screen.height + 1);
      }
      await page.screenshot({ path: `../output/playwright/rp-screens/${screen.name}.png` });
    });
  });
}

test.describe("a dialog and the notifications, folded", () => {
  test.use({ viewport: { width: 466, height: 678 }, hasTouch: true, isMobile: true });

  test("keep clear of the column of icons", async ({ page }) => {
    await mockConsole(page);
    await insets(page, { top: 44, right: 93, bottom: 20, left: 0 });
    await page.goto("/home", { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: /notifications/i }).first().tap();
    const panel = page.getByRole("dialog").or(page.getByTestId("notification-panel")).first();
    await expect(panel).toBeVisible();
    const box = (await panel.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(466 - 93 + 1);
  });

  test("the banner of a running stream, with its Stop, stays out from under the icons", async ({ page }) => {
    await mockConsole(page);
    await insets(page, { top: 44, right: 93, bottom: 20, left: 0 });
    await page.goto("/play", { waitUntil: "domcontentloaded" });
    await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
    await page.getByTestId("rp-start").click();
    await expect(page.getByTestId("rp-stage")).toBeVisible();
    await page.locator('[data-bottom-nav] a[href="/home"]').tap();
    const banner = page.getByTestId("rp-banner");
    await expect(banner).toBeVisible();
    const box = (await banner.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(466 - 93);
    const stop = (await banner.getByRole("button", { name: "Stop" }).boundingBox())!;
    expect(stop.x + stop.width).toBeLessThanOrEqual(466 - 93);
    await page.screenshot({ path: "../output/playwright/rp-screens/Duo folded banner.png" });
  });
});

// Read off a real one (#675): an open Duo held sideways has large insets top and bottom, which cut
// the picture to a band across the middle.
test.describe("an open Duo held sideways", () => {
  test.use({ viewport: { width: 951, height: 669 }, hasTouch: true, isMobile: true });

  test("shows the whole picture at the screen's full width", async ({ page }) => {
    await mockConsole(page);
    await insets(page, { top: 156, right: 0, bottom: 109, left: 0 });
    await playFull(page);
    const box = (await page.locator('[data-testid="rp-stage"] canvas').first().boundingBox())!;
    expect(box.width).toBeCloseTo(951, 0);
    expect(box.height).toBeCloseTo((951 * 9) / 16, 0);
    expect(box.y).toBeCloseTo((669 - (951 * 9) / 16) / 2, 0);
  });
});

test.describe("the tabs on a folded Duo", () => {
  test.use({ viewport: { width: 466, height: 678 }, hasTouch: true, isMobile: true });

  test("go down the empty column, under the system's icons", async ({ page }) => {
    await mockConsole(page);
    await insets(page, { top: 44, right: 93, bottom: 20, left: 0 });
    await page.goto("/home", { waitUntil: "domcontentloaded" });
    const nav = page.locator("[data-bottom-nav]");
    await expect(nav).toHaveAttribute("data-rail", "right");
    const box = (await nav.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(466 - 93);
    expect(box.x + box.width).toBeLessThanOrEqual(466);
    expect(box.y).toBeGreaterThanOrEqual(44 + 100);
    expect(box.y + box.height).toBeLessThanOrEqual(678 - 20);
    const tabs = await nav.locator("a").all();
    expect(tabs.length).toBe(6);
    for (const t of tabs) expect((await t.boundingBox())!.height).toBeGreaterThanOrEqual(44);
    await page.screenshot({ path: "../output/playwright/rp-screens/Duo folded tabs.png" });
  });

  test("stay along the bottom on a phone with no column", async ({ page }) => {
    await mockConsole(page);
    await insets(page, { top: 59, right: 0, bottom: 34, left: 0 });
    await page.goto("/home", { waitUntil: "domcontentloaded" });
    await expect(page.locator("[data-bottom-nav]")).toBeVisible();
    await expect(page.locator("[data-bottom-nav]")).not.toHaveAttribute("data-rail");
  });
});
