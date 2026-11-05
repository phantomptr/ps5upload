import { expect, test, type Page } from "@playwright/test";
import { routeEngine, seedConsole } from "./fixtures/engine";

// #637: the stream keeps showing in a small floating window on the app's other screens. Its one
// canvas moves there and back (never in two places), a drag snaps it to a corner (remembered), a
// tap goes back to Play, × hides it until Play is opened again, and the speaker mutes. The
// engine socket is faked; the browser is muted (playwright.config.ts).

async function fakeEngine(page: Page): Promise<{ starts: () => number; stops: () => number }> {
  await seedConsole(page);
  await routeEngine(page, (path) => {
    let body: unknown = {};
    if (path === "/api/version") body = { version: "10.7.6", caps: {} };
    if (path === "/api/ps5/rp/session") body = { host: null };
    return body;
  });
  let starts = 0;
  let stops = 0;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) {
        starts++;
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
      if (msg.stop) {
        stops++;
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
    });
  });
  return { starts: () => starts, stops: () => stops };
}

async function go(page: Page, path: string): Promise<void> {
  await page.evaluate((p) => {
    window.history.pushState({}, "", p);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, path);
}

/** Where the stream's canvas is: in the mini player, on the Play screen, or elsewhere. */
const canvasPlace = (page: Page) =>
  page.evaluate(() => {
    const all = document.querySelectorAll("canvas[data-testid=rp-canvas]");
    if (all.length !== 1) return `canvases:${all.length}`;
    const c = all[0];
    if (c.closest("[data-testid=rp-mini]")) return "mini";
    if (c.closest('[data-screen="/play"]')) return "play";
    return c.isConnected ? "elsewhere" : "detached";
  });

async function startOnPlay(page: Page, engine: { starts: () => number }): Promise<void> {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toContainText("Living room");
  await expect.poll(engine.starts).toBeGreaterThan(0);
}

test("the stream follows you to other screens in a mini player", async ({ page }) => {
  const engine = await fakeEngine(page);
  await startOnPlay(page, engine);
  await expect.poll(() => canvasPlace(page)).toBe("play");
  await expect(page.getByTestId("rp-mini")).toHaveCount(0);

  // Another screen: the picture moves into the mini player, bottom-right by default.
  await go(page, "/games");
  const mini = page.getByTestId("rp-mini");
  await expect(mini).toBeVisible();
  await expect(mini).toHaveAttribute("data-corner", "br");
  await expect.poll(() => canvasPlace(page)).toBe("mini");
  const box = (await mini.boundingBox())!;
  expect(Math.round(box.width)).toBe(320);
  expect(Math.round(box.height)).toBe(180);
  const vp = page.viewportSize()!;
  expect(box.x + box.width).toBeLessThanOrEqual(vp.width);
  expect(box.y + box.height).toBeLessThanOrEqual(vp.height);

  // The speaker mutes and unmutes the stream (and stays on the mini player).
  const prefs = () => page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_prefs.v1") ?? "{}"));
  await page.getByTestId("rp-mini-mute").click();
  await expect.poll(async () => (await prefs()).muted).toBe(true);
  await expect(page).toHaveURL(/\/games$/);
  await page.getByTestId("rp-mini-mute").click();
  await expect.poll(async () => (await prefs()).muted).toBe(false);

  // Dragged towards the top-left, it snaps to that corner, remembered.
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(vp.width * 0.3, vp.height * 0.3, { steps: 8 });
  await page.mouse.move(80, 120, { steps: 8 });
  await page.mouse.up();
  await expect(mini).toHaveAttribute("data-corner", "tl");
  await expect(page).toHaveURL(/\/games$/);
  expect(await page.evaluate(() => localStorage.getItem("ps5upload.rp_mini_corner.v1"))).toBe("tl");
  await expect
    .poll(async () => {
      const b = (await mini.boundingBox())!;
      return b.x < vp.width / 2 && b.y < vp.height / 2;
    })
    .toBe(true);

  // A tap: back to Play, with the picture back in its place.
  await mini.click();
  await expect(page).toHaveURL(/\/play$/);
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
  await expect(mini).toHaveCount(0);
  await expect.poll(() => canvasPlace(page)).toBe("play");

  // × hides it (the stream keeps going, and the bar still leads back) until Play is opened again.
  await go(page, "/tasks");
  await expect(mini).toBeVisible();
  await expect(mini).toHaveAttribute("data-corner", "tl");
  await page.getByTestId("rp-mini-close").click();
  await expect(mini).toHaveCount(0);
  await expect(page.getByTestId("rp-banner")).toBeVisible();
  await go(page, "/games");
  await expect(mini).toHaveCount(0);
  await go(page, "/play");
  await expect.poll(() => canvasPlace(page)).toBe("play");
  await go(page, "/games");
  await expect(mini).toBeVisible();
  await expect.poll(() => canvasPlace(page)).toBe("mini");
  expect(engine.stops()).toBe(0);

  // Stop (from the bar): the mini player goes with the stream.
  await page.getByTestId("rp-banner").getByRole("button", { name: "Stop" }).click();
  await expect.poll(engine.stops).toBe(1);
  await expect(mini).toHaveCount(0);
});

test("the mini player can be turned off", async ({ page }) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.rp_prefs.v1", JSON.stringify({ miniPlayer: false }));
  });
  const engine = await fakeEngine(page);
  await startOnPlay(page, engine);
  await go(page, "/games");
  await expect(page.getByTestId("rp-banner")).toBeVisible();
  await expect(page.getByTestId("rp-mini")).toHaveCount(0);
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test("the mini player is small and clear of the bottom tab bar", async ({ page }) => {
    const engine = await fakeEngine(page);
    await startOnPlay(page, engine);
    await go(page, "/games");
    const mini = page.getByTestId("rp-mini");
    await expect(mini).toBeVisible();
    await expect.poll(() => canvasPlace(page)).toBe("mini");
    const box = (await mini.boundingBox())!;
    expect(Math.round(box.width)).toBe(Math.round(390 * 0.4));
    const nav = (await page.locator("nav.fixed").filter({ visible: true }).first().boundingBox())!;
    expect(box.y + box.height).toBeLessThanOrEqual(nav.y);
    // A tap goes back to Play.
    await mini.tap();
    await expect(page).toHaveURL(/\/play$/);
    await expect(mini).toHaveCount(0);
  });
});
