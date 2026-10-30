import { expect, test, type Page } from "@playwright/test";

// Remote Play on a phone: upright, the picture is on top with its bar and controller under it
// (every action reachable at 390 px); sideways or with Full screen, the picture fills the
// screen with one small menu handle, and taps on the picture never leave full screen or stop.

async function mockConsole(page: Page) {
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [
          {
            id: "p1",
            name: "Living room",
            host: "192.168.1.100",
            wake_regist_key: "00112233",
            wake_rp_key: "00112233445566778899aabbccddeeff",
          },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "8.11.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    },
  );
  const counts = { starts: 0, stops: 0 };
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) {
        counts.starts++;
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
      if (msg.stop) {
        counts.stops++;
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
    });
  });
  return counts;
}

async function startPlaying(page: Page) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({
    timeout: 30_000,
  });
  await page.getByTestId("rp-start").click();
}

async function noSideScroll(page: Page) {
  const over = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(over).toBeLessThanOrEqual(0);
}

async function inViewport(page: Page, testId: string) {
  const vp = page.viewportSize()!;
  const box = await page
    .getByTestId(testId)
    .filter({ visible: true })
    .boundingBox();
  expect(box, testId).not.toBeNull();
  expect(box!.x, `${testId} left`).toBeGreaterThanOrEqual(0);
  expect(box!.y, `${testId} top`).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width, `${testId} right`).toBeLessThanOrEqual(
    vp.width + 0.5,
  );
  expect(box!.y + box!.height, `${testId} bottom`).toBeLessThanOrEqual(
    vp.height + 0.5,
  );
}

/** Taps (and a double tap) on the picture, away from the controller and the handle. */
async function tapPicture(page: Page) {
  const vp = page.viewportSize()!;
  const spots: Array<[number, number]> = [
    [vp.width / 2, vp.height / 2],
    [vp.width * 0.35, vp.height * 0.55],
    [vp.width * 0.65, vp.height * 0.45],
  ];
  // The picture itself (upright full screen puts it at the top).
  const pic = (await page
    .getByTestId("rp-stage")
    .locator("> div")
    .first()
    .boundingBox())!;
  spots.push(
    [pic.x + pic.width / 2, pic.y + pic.height / 2],
    [pic.x + pic.width * 0.2, pic.y + pic.height * 0.7],
  );
  for (const [x, y] of spots) await page.touchscreen.tap(x, y);
  await page.touchscreen.tap(vp.width / 2, vp.height / 2);
  await page.touchscreen.tap(vp.width / 2, vp.height / 2);
  // A double click from a finger (some browsers synthesise one from a double tap).
  await page
    .getByTestId("rp-stage")
    .dispatchEvent("pointerdown", { pointerType: "touch" });
  await page.getByTestId("rp-stage").dispatchEvent("dblclick");
}

test.describe("phone upright", () => {
  test.use({
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
  });

  test("bar under the picture, every action reachable, full screen holds against taps", async ({
    page,
  }) => {
    const counts = await mockConsole(page);
    await startPlaying(page);
    const stage = page.getByTestId("rp-stage");
    const toolbar = page.getByTestId("rp-toolbar");
    await expect(toolbar).toContainText("Living room");

    // The bar sits under the picture, not on it. The page is view only: the controller lives in
    // full screen, and the page says so.
    await expect(page.getByTestId("rp-touch")).toHaveCount(0);
    await expect(page.getByTestId("rp-touch-in-full")).toContainText("View only");
    // Measured together (the page may still be scrolling the picture into view).
    const barBelowPicture = await page.evaluate(() => {
      const r = (id: string) =>
        document
          .querySelector(`[data-testid="${id}"]`)!
          .getBoundingClientRect();
      return r("rp-toolbar").top >= r("rp-stage").bottom - 1;
    });
    expect(barBelowPicture).toBe(true);
    await noSideScroll(page);
    for (const id of ["rp-fullscreen", "rp-more", "rp-stop"])
      await inViewport(page, id);

    // More: the rest, finger-sized, inside the screen.
    await page.getByTestId("rp-more").tap();
    for (const id of [
      "rp-toggle-stats",
      "rp-screenshot",
      "rp-record",
      "rp-open-settings",
      "rp-mute",
      "rp-volume",
    ]) {
      await page.getByTestId(id).scrollIntoViewIfNeeded();
      await inViewport(page, id);
    }
    await page.getByTestId("rp-toggle-stats").tap();
    await expect(page.getByTestId("rp-toggle-stats")).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await noSideScroll(page);
    await page.screenshot({
      path: "../output/playwright/rp-portrait-page.png",
      fullPage: true,
    });

    // Full screen: the picture fills the screen, one small handle, the controller over it.
    await page.getByTestId("rp-fullscreen").tap();
    await expect(stage).toHaveAttribute("data-full", "immersive");
    // Upright: the picture across the top, the controller on the black below it.
    const touch = page.getByTestId("rp-touch");
    await expect(touch).toHaveAttribute("data-layout", "deck");
    const split = await page.evaluate(() => {
      const pic = document
        .querySelector('[data-testid="rp-stage"] > div')!
        .getBoundingClientRect();
      const pad = document
        .querySelector('[data-testid="rp-touch"]')!
        .getBoundingClientRect();
      const handle = document
        .querySelector('[data-testid="rp-menu-handle"]')!
        .getBoundingClientRect();
      return {
        picTop: pic.top,
        picWidth: pic.width,
        padBelow: pad.top >= pic.bottom - 1,
        handleAbove: handle.bottom <= pic.top + 1,
      };
    });
    expect(split).toMatchObject({
      picWidth: 390,
      padBelow: true,
      handleAbove: true,
    });
    await expect(page.getByTestId("rp-menu-handle")).toBeVisible();
    await expect(page.getByTestId("rp-toolbar")).toHaveCount(0);
    await page.screenshot({
      path: "../output/playwright/rp-portrait-full.png",
    });

    // Taps on the picture and the controller are play: still full screen, still streaming.
    await tapPicture(page);
    await page.getByRole("button", { name: "✕" }).tap();
    await page.getByRole("button", { name: "R1" }).tap();
    await expect(stage).toHaveAttribute("data-full", "immersive");
    expect(counts.stops).toBe(0);

    // System back leaves full screen and nothing else.
    await page.goBack();
    await expect(stage).not.toHaveAttribute("data-full", "immersive");
    await expect(page).toHaveURL(/\/play$/);
    await expect(toolbar).toBeVisible();
    expect(counts.stops).toBe(0);

    // The handle's menu leaves full screen too.
    await page.getByTestId("rp-fullscreen").tap();
    await page.getByTestId("rp-menu-handle").tap();
    await inViewport(page, "rp-menu");
    await page.getByTestId("rp-fullscreen").tap();
    await expect(stage).not.toHaveAttribute("data-full", "immersive");
    await expect(page).toHaveURL(/\/play$/);
    expect(counts.stops).toBe(0);

    await page.getByTestId("rp-stop").tap();
    await expect.poll(() => counts.stops).toBe(1);
    await expect(page.getByTestId("rp-start")).toBeVisible();
  });
});

test.describe("small phone upright", () => {
  test.use({
    viewport: { width: 360, height: 640 },
    hasTouch: true,
    isMobile: true,
  });

  test("the bar and its menu fit at 360 px", async ({ page }) => {
    await mockConsole(page);
    await startPlaying(page);
    await expect(page.getByTestId("rp-toolbar")).toContainText("Living room");
    await noSideScroll(page);
    for (const id of ["rp-fullscreen", "rp-more", "rp-stop"]) {
      await page.getByTestId(id).scrollIntoViewIfNeeded();
      await inViewport(page, id);
    }
    await page.getByTestId("rp-more").tap();
    for (const id of [
      "rp-toggle-stats",
      "rp-screenshot",
      "rp-record",
      "rp-pip",
      "rp-open-settings",
      "rp-volume",
    ]) {
      await page.getByTestId(id).scrollIntoViewIfNeeded();
      await inViewport(page, id);
    }
    await noSideScroll(page);
    // Full screen upright: everything inside the small screen, the menu too.
    await page.getByTestId("rp-fullscreen").tap();
    for (const id of ["rp-menu-handle", "rp-touch"]) await inViewport(page, id);
    await page.getByTestId("rp-menu-handle").tap();
    for (const id of ["rp-fullscreen", "rp-stop", "rp-volume", "rp-menu-fit"])
      await inViewport(page, id);
    // Picture size changes here, in full screen, where it shows.
    await page.getByTestId("rp-menu-fit-fill").tap();
    await expect(page.getByTestId("rp-menu-fit-fill")).toHaveAttribute("aria-pressed", "true");
    await expect(page.getByTestId("rp-stage").locator("canvas")).toHaveAttribute("data-fit", "fill");
    await expect(page.getByTestId("rp-stage")).toHaveAttribute("data-full", "immersive");
    await page.screenshot({
      path: "../output/playwright/rp-360-full-menu.png",
    });
  });
});

test.describe("phone sideways", () => {
  test.use({
    viewport: { width: 844, height: 390 },
    hasTouch: true,
    isMobile: true,
  });

  test("fills the screen by itself; the handle opens the menu; taps never leave", async ({
    page,
  }) => {
    const counts = await mockConsole(page);
    await startPlaying(page);
    const stage = page.getByTestId("rp-stage");
    await expect(stage).toHaveAttribute("data-full", "immersive");
    await expect(page.getByTestId("rp-touch")).toHaveAttribute(
      "data-layout",
      "overlay",
    );
    await inViewport(page, "rp-menu-handle");
    await noSideScroll(page);

    await tapPicture(page);
    await expect(stage).toHaveAttribute("data-full", "immersive");
    // A finger never locks the pointer (Linux Chrome grants a lock from a tap, which froze every
    // later touch's position and made the controller's setPointerCapture throw).
    expect(await page.evaluate(() => document.pointerLockElement)).toBeNull();
    await page.screenshot({
      path: "../output/playwright/rp-landscape-full.png",
    });

    // The menu fits the short screen; a tap beside it closes it and nothing else.
    await page.getByTestId("rp-menu-handle").tap();
    await inViewport(page, "rp-menu");
    for (const id of [
      "rp-fullscreen",
      "rp-toggle-stats",
      "rp-screenshot",
      "rp-record",
      "rp-open-settings",
      "rp-stop",
      "rp-volume",
    ]) {
      await inViewport(page, id);
    }
    await page.screenshot({
      path: "../output/playwright/rp-landscape-menu.png",
    });
    await page
      .getByTestId("rp-menu-backdrop")
      .tap({ position: { x: 20, y: 200 } });
    await expect(page.getByTestId("rp-menu")).toHaveCount(0);
    await expect(stage).toHaveAttribute("data-full", "immersive");
    expect(counts.stops).toBe(0);

    // Stats move out of the controller's corner.
    await page.getByTestId("rp-menu-handle").tap();
    await page.getByTestId("rp-toggle-stats").tap();
    await page.getByTestId("rp-menu-close").tap();

    // Stop from the menu is the one way to end it here.
    await page.getByTestId("rp-menu-handle").tap();
    await page.getByTestId("rp-stop").tap();
    await expect.poll(() => counts.stops).toBe(1);
    await expect(page.getByTestId("rp-start")).toBeVisible();
    await expect(stage).not.toHaveAttribute("data-full", "immersive");
  });
});

test.describe("computer", () => {
  test("double-click is full screen with the bar on the picture; the bar leaves it", async ({
    page,
  }) => {
    const counts = await mockConsole(page);
    await startPlaying(page);
    const stage = page.getByTestId("rp-stage");
    await expect(page.getByTestId("rp-toolbar")).toContainText("Living room");
    await expect(page.getByTestId("rp-menu-handle")).toHaveCount(0);
    await stage.dblclick();
    await expect(stage).toHaveAttribute("data-full", "bar");
    await page.getByTestId("rp-toolbar").hover();
    await page.getByTestId("rp-fullscreen").click();
    await expect(stage).not.toHaveAttribute("data-full", /.+/);
    expect(counts.stops).toBe(0);
  });
});
