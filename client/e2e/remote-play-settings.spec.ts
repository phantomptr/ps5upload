import { expect, test, type Page } from "@playwright/test";

// Remote Play's settings (10.3.0): one drawer over the stream from every Settings button. At the
// right edge on a computer, a sheet from the bottom on a phone; in full screen it opens inside
// the picture, so full screen stays. ✕, Esc, a press outside, the Settings button again and a
// swipe down (phone) close it. While it is open the console gets no keys or controller.

type Engine = { starts: number; stops: number; pads: number[] };

async function mockConsole(page: Page, opts: { pad?: boolean } = {}): Promise<Engine> {
  await page.addInitScript((pad) => {
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
            wake_rp_key: "0f".repeat(16), // made up (built here so it reads as no secret)
          },
        ],
      }),
    );
    if (!pad) return;
    // A standard controller the test presses: window.__pad.buttons[i] = 1.
    const state = { buttons: new Array(17).fill(0) as number[] };
    (window as unknown as { __pad: typeof state }).__pad = state;
    navigator.getGamepads = () =>
      [
        {
          id: "Wireless Controller (STANDARD GAMEPAD Vendor: 054c Product: 0ce6)",
          index: 0,
          connected: true,
          mapping: "standard",
          timestamp: performance.now(),
          axes: [0, 0, 0, 0],
          buttons: state.buttons.map((v) => ({ pressed: v > 0.5, touched: v > 0, value: v })),
          vibrationActuator: null,
        },
      ] as unknown as (Gamepad | null)[];
  }, !!opts.pad);
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.2.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const engine: Engine = { starts: 0, stops: 0, pads: [] };
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string; stop?: boolean; pad?: { buttons: number } };
      if (msg.host) {
        engine.starts++;
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
      if (msg.pad) engine.pads.push(msg.pad.buttons);
      if (msg.stop) {
        engine.stops++;
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
    });
  });
  return engine;
}

async function openPlay(page: Page) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
}

const pressPad = (page: Page, i: number, v: number) =>
  page.evaluate(([i, v]) => ((window as unknown as { __pad: { buttons: number[] } }).__pad.buttons[i] = v), [i, v]);

test.describe("computer", () => {
  test("before Start: the Settings button opens the drawer; ✕, Esc, outside and the button close it", async ({
    page,
  }) => {
    await mockConsole(page);
    await openPlay(page);
    const button = page.getByTestId("rp-settings-button");
    const drawer = page.getByTestId("rp-settings");

    // Opens at the right edge, on Picture, with the focus on its tab.
    await button.click();
    await expect(drawer).toBeVisible();
    await expect(drawer).toHaveAttribute("data-layout", "side");
    await expect(button).toHaveAttribute("aria-expanded", "true");
    await expect(page.getByRole("tab", { name: "Picture" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("tab", { name: "Picture" })).toBeFocused();
    const box = (await drawer.boundingBox())!;
    expect(box.x + box.width).toBeGreaterThan(page.viewportSize()!.width - 20);
    // Not streaming: nothing says it applies later.
    await expect(drawer.getByText("Applies when you start again")).toHaveCount(0);

    // ✕ closes it and gives the focus back to the button.
    await page.getByTestId("rp-settings-close").click();
    await expect(drawer).toHaveCount(0);
    await expect(button).toBeFocused();

    // Esc.
    await button.click();
    await expect(drawer).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(drawer).toHaveCount(0);

    // The button again.
    await button.click();
    await expect(drawer).toBeVisible();
    await button.click();
    await expect(drawer).toHaveCount(0);

    // A press outside.
    await button.click();
    await expect(drawer).toBeVisible();
    await page.getByRole("heading", { name: "Play", exact: true }).click();
    await expect(drawer).toHaveCount(0);
  });

  test("tabs move with the arrow keys, Tab stays inside, and the last tab is remembered", async ({ page }) => {
    await mockConsole(page);
    await openPlay(page);
    await page.getByTestId("rp-settings-button").click();
    const drawer = page.getByTestId("rp-settings");
    await expect(page.getByRole("tab", { name: "Picture" })).toBeFocused();
    await page.keyboard.press("ArrowRight");
    await expect(page.getByRole("tab", { name: "Sound" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("tab", { name: "Sound" })).toBeFocused();
    await expect(drawer.getByText("Sound buffer")).toBeVisible();
    await page.keyboard.press("End");
    await expect(page.getByRole("tab", { name: "Session" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByTestId("rp-replay-setting")).toBeVisible();
    await page.keyboard.press("ArrowLeft");
    await expect(page.getByRole("tab", { name: "Controls" })).toHaveAttribute("aria-selected", "true");
    // Controllers, Controller buttons and Keyboard controls are under Controls.
    for (const h of ["Controllers", "Controller buttons", "Keyboard controls"])
      await expect(drawer.getByRole("heading", { name: h })).toBeVisible();

    // Tab never leaves the drawer.
    for (let i = 0; i < 60; i++) await page.keyboard.press("Tab");
    expect(await page.evaluate(() => !!document.activeElement?.closest('[data-testid="rp-settings"]'))).toBe(true);

    // Remembered: closing, opening and a reload come back to Controls.
    await page.keyboard.press("Escape");
    await page.getByTestId("rp-settings-button").click();
    await expect(page.getByRole("tab", { name: "Controls" })).toHaveAttribute("aria-selected", "true");
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.getByTestId("rp-settings-button").click();
    await expect(page.getByRole("tab", { name: "Controls" })).toHaveAttribute("aria-selected", "true");
  });

  test("streaming: the drawer opens in full screen and stays there; Esc closes it, not full screen", async ({
    page,
  }) => {
    const engine = await mockConsole(page);
    await openPlay(page);
    await page.getByTestId("rp-start").click();
    const stage = page.getByTestId("rp-stage");
    const toolbar = page.getByTestId("rp-toolbar");
    await expect(toolbar).toBeVisible();

    // On the page: the bar's Settings opens the drawer; what applies on the next start says so.
    await toolbar.hover();
    await toolbar.getByTestId("rp-settings-button").click();
    const drawer = page.getByTestId("rp-settings");
    await expect(drawer).toBeVisible();
    await expect(page.getByTestId("rp-settings-paused")).toBeVisible();
    await expect(drawer.getByText("Applies when you start again")).toHaveCount(4);
    await expect(page.getByTestId("rp-res")).toBeEnabled();
    // Picture size applies at once: no note under it.
    await expect(page.getByTestId("rp-fit")).not.toHaveAttribute("aria-describedby", /.+/);
    await page.getByTestId("rp-settings-close").click();
    await expect(drawer).toHaveCount(0);

    // Full screen: Settings opens inside it, the picture's full-screen view.
    await toolbar.hover();
    await page.getByTestId("rp-fullscreen").click();
    await expect(stage).toHaveAttribute("data-full", "bar");
    await stage.hover();
    await toolbar.getByTestId("rp-settings-button").click();
    await expect(drawer).toBeVisible();
    await expect(stage).toHaveAttribute("data-full", "bar");
    await expect(stage.getByTestId("rp-settings")).toBeVisible();
    // Under the bar, which stays usable.
    const bar = (await toolbar.boundingBox())!;
    const box = (await drawer.boundingBox())!;
    expect(box.y).toBeGreaterThanOrEqual(bar.y + 40);

    // Esc closes the drawer: full screen stays, and the game never gets its ○.
    const before = engine.pads.length;
    await page.keyboard.down("Escape");
    await page.waitForTimeout(900); // longer than the hold that leaves full screen
    await page.keyboard.up("Escape");
    await expect(drawer).toHaveCount(0);
    await expect(stage).toHaveAttribute("data-full", "bar");
    await page.waitForTimeout(300);
    expect(engine.pads.slice(before).every((b) => b === 0)).toBe(true);

    // A press on the light backdrop closes it too (and is not play).
    await stage.hover();
    await toolbar.getByTestId("rp-settings-button").click();
    await expect(drawer).toBeVisible();
    await page.getByTestId("rp-settings-backdrop").click({ position: { x: 40, y: 200 } });
    await expect(drawer).toHaveCount(0);
    await expect(stage).toHaveAttribute("data-full", "bar");
    expect(engine.stops).toBe(0);
  });

  test("while Settings is open the console gets no keys or controller; they play again on close", async ({
    page,
  }) => {
    const engine = await mockConsole(page, { pad: true });
    await openPlay(page);
    await page.getByTestId("rp-start").click();
    const stage = page.getByTestId("rp-stage");
    const toolbar = page.getByTestId("rp-toolbar");
    await expect(toolbar).toBeVisible();
    await stage.focus();

    // Playing: a key and a controller button reach the console.
    await page.keyboard.down("Enter");
    await expect.poll(() => engine.pads.some((b) => b !== 0)).toBe(true);
    await page.keyboard.up("Enter");
    await expect.poll(() => engine.pads[engine.pads.length - 1]).toBe(0);

    // Settings open: arrows move between its tabs, a controller presses nothing.
    await toolbar.hover();
    await toolbar.getByTestId("rp-settings-button").click();
    await expect(page.getByTestId("rp-settings")).toBeVisible();
    const from = engine.pads.length;
    await pressPad(page, 0, 1);
    await page.keyboard.press("ArrowRight");
    await expect(page.getByRole("tab", { name: "Sound" })).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("Enter");
    await page.waitForTimeout(500);
    expect(engine.pads.slice(from).every((b) => b === 0)).toBe(true);

    // Closed: the held button plays again.
    await page.getByTestId("rp-settings-close").click();
    await expect.poll(() => engine.pads.slice(from).some((b) => b !== 0)).toBe(true);
    await pressPad(page, 0, 0);
  });
});

test.describe("phone sideways", () => {
  test.use({ viewport: { width: 915, height: 412 }, hasTouch: true, isMobile: true });

  test("the on-screen controller's menu opens Settings as a sheet over the picture; a swipe down closes it", async ({
    page,
  }) => {
    const engine = await mockConsole(page);
    await openPlay(page);
    await page.getByTestId("rp-start").click();
    const stage = page.getByTestId("rp-stage");
    await expect(stage).toHaveAttribute("data-full", "immersive");
    await expect(page.getByTestId("rp-touch")).toHaveAttribute("data-layout", "overlay");

    await page.getByTestId("rp-menu-handle").tap();
    await page.getByTestId("rp-open-settings").tap();
    const drawer = page.getByTestId("rp-settings");
    await expect(drawer).toBeVisible();
    await expect(drawer).toHaveAttribute("data-layout", "sheet");
    await expect(stage.getByTestId("rp-settings")).toBeVisible();
    await expect(stage).toHaveAttribute("data-full", "immersive");
    // At the bottom, inside the screen, with the picture still showing above it.
    const vp = page.viewportSize()!;
    await expect.poll(async () => (await drawer.boundingBox())!.y + (await drawer.boundingBox())!.height).toBeLessThanOrEqual(vp.height + 0.5);
    expect((await drawer.boundingBox())!.y).toBeGreaterThan(20);

    // A swipe down on its top closes it.
    const grip = (await page.getByTestId("rp-settings-grip").boundingBox())!;
    const x = grip.x + grip.width / 2;
    const y = grip.y + grip.height / 2;
    const cdp = await page.context().newCDPSession(page);
    const touch = (type: string, ty: number) =>
      cdp.send("Input.dispatchTouchEvent", {
        type,
        touchPoints: type === "touchEnd" ? [] : [{ x, y: ty }],
      });
    await touch("touchStart", y);
    for (let i = 1; i <= 8; i++) await touch("touchMove", y + i * 20);
    await touch("touchEnd", y + 160);
    await expect(drawer).toHaveCount(0);
    await expect(stage).toHaveAttribute("data-full", "immersive");

    // A tap beside it closes it too, and nothing else. Chromium swallows the first tap after a
    // swipe (it stops a possible scroll fling), so the handle is tapped until the menu opens.
    await expect(async () => {
      await page.getByTestId("rp-menu-handle").tap();
      await expect(page.getByTestId("rp-open-settings")).toBeVisible({ timeout: 1_000 });
    }).toPass({ timeout: 10_000 });
    await page.getByTestId("rp-open-settings").tap();
    await expect(drawer).toBeVisible();
    await page.getByTestId("rp-settings-backdrop").tap({ position: { x: 20, y: 20 } });
    await expect(drawer).toHaveCount(0);
    await expect(stage).toHaveAttribute("data-full", "immersive");
    expect(engine.stops).toBe(0);
  });
});

test.describe("phone upright", () => {
  test.use({ viewport: { width: 412, height: 915 }, hasTouch: true, isMobile: true });

  test("Settings before Start is a sheet from the bottom", async ({ page }) => {
    await mockConsole(page);
    await openPlay(page);
    await page.getByTestId("rp-settings-button").tap();
    const drawer = page.getByTestId("rp-settings");
    await expect(drawer).toHaveAttribute("data-layout", "sheet");
    const vp = page.viewportSize()!;
    await expect.poll(async () => (await drawer.boundingBox())!.y + (await drawer.boundingBox())!.height).toBeLessThanOrEqual(vp.height + 0.5);
    const box = (await drawer.boundingBox())!;
    expect(Math.round(box.x)).toBe(0);
    expect(Math.round(box.width)).toBe(vp.width);
    await page.getByTestId("rp-settings-close").tap();
    await expect(drawer).toHaveCount(0);
  });
});
