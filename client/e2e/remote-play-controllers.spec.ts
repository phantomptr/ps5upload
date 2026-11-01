import { expect, test, type Page } from "@playwright/test";

// Controllers in Remote Play (9.5.0): mapping a controller the browser does not recognize (phone
// controllers like a GameSir G8 often are), a computer taking a phone as a controller, and a phone
// being one.

async function consoleSetUp(page: Page) {
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
}

/** A controller the test presses: window.__pad.buttons[i] = 1. Reported with no standard mapping. */
async function fakeController(page: Page) {
  await page.addInitScript(() => {
    const state = { buttons: new Array(20).fill(0) as number[], axes: [0, 0, 0, 0, -1, -1] };
    (window as unknown as { __pad: typeof state }).__pad = state;
    navigator.getGamepads = () =>
      [
        {
          id: "GameSir G8 Galileo (Vendor: 3537 Product: 1022)",
          index: 0,
          connected: true,
          mapping: "",
          timestamp: performance.now(),
          axes: [...state.axes],
          buttons: state.buttons.map((v) => ({ pressed: v > 0.5, touched: v > 0, value: v })),
          vibrationActuator: null,
        },
      ] as unknown as (Gamepad | null)[];
  });
}

const press = (page: Page, i: number, v: number) =>
  page.evaluate(([i, v]) => ((window as unknown as { __pad: { buttons: number[] } }).__pad.buttons[i] = v), [i, v]);

test("a controller the browser does not recognize is mapped by pressing its buttons", async ({ page }) => {
  await consoleSetUp(page);
  await fakeController(page);
  const pads: number[] = [];
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.5.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string; pad?: { buttons: number } };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
      if (msg.pad) pads.push(msg.pad.buttons);
    });
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("tab", { name: "Controls" }).click();
  const row = page.getByTestId("rp-pad");
  await expect(row).toContainText("GameSir G8 Galileo");
  await expect(row).toContainText("Not recognized");

  // ✕ is button 5 on this one; everything else is skipped (keeps the usual layout).
  await row.getByRole("button", { name: "Map buttons" }).click();
  await expect(page.getByTestId("rp-pad-step")).toHaveText("Press ✕");
  await press(page, 5, 1);
  await expect(page.getByTestId("rp-pad-step")).toHaveText("Press ○");
  await press(page, 5, 0);
  for (let i = 1; i < 22; i++) await page.getByTestId("rp-pad-skip").click();
  await expect(page.getByTestId("rp-pad-wizard")).toHaveCount(0);
  await expect(row).toContainText("Plays with your mapping");
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_pad_profiles.v1") ?? "{}"));
  expect(saved["GameSir G8 Galileo (Vendor: 3537 Product: 1022)"].buttons).toContainEqual([1, { b: 5 }]);

  // Playing: button 5 is ✕ on the console now.
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
  await page.getByTestId("rp-stage").focus();
  await press(page, 5, 1);
  await expect.poll(() => pads.includes(1)).toBe(true);
  await press(page, 5, 0);
  await expect.poll(() => pads[pads.length - 1]).toBe(0);
});

test("the computer shows a QR code for a phone, and lets a phone that asks in", async ({ page }) => {
  await consoleSetUp(page);
  const decided: unknown[] = [];
  let pending = [{ id: 7, name: "Pixel 8" }];
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.5.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/rp/pad-link/decide") {
        decided.push(req.postDataJSON());
        pending = [];
        body = { ok: true };
      } else if (path === "/api/ps5/rp/pad-link") {
        body = {
          open: true,
          url: "http://192.168.1.9:19113/pad#t=0123456789abcdef0123456789abcdef",
          // On two networks: the PS5's, and the default route's (the phone's Wi-Fi).
          urls: [
            "http://192.168.1.9:19113/pad#t=0123456789abcdef0123456789abcdef",
            "http://192.168.0.34:19113/pad#t=0123456789abcdef0123456789abcdef",
          ],
          pad_ws: "ws://192.168.1.9:19113/api/pad/ws",
          pads: pending.length ? [] : [{ id: 7, name: "Pixel 8" }],
          pending,
        };
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if ((JSON.parse(String(m)) as { host?: string }).host) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });

  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();

  // A phone asking shows on the picture.
  const ask = page.getByTestId("rp-pad-ask");
  await expect(ask).toContainText("Pixel 8 wants to be a controller");
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-phones").click();
  await expect(page.getByTestId("rp-pad-qr")).toBeVisible();
  await expect(page.getByTestId("rp-pad-url")).toHaveText(/^http:\/\/192\.168\.1\.9:19113\/pad#t=/);
  // The other network's address is one press away, and the QR code follows it.
  const addresses = page.getByTestId("rp-pad-address");
  await expect(addresses).toHaveText(["192.168.1.9", "192.168.0.34"]);
  await expect(addresses.first()).toHaveAttribute("aria-pressed", "true");
  await addresses.nth(1).click();
  await expect(page.getByTestId("rp-pad-url")).toHaveText(/^http:\/\/192\.168\.0\.34:19113\/pad#t=/);
  await expect(addresses.nth(1)).toHaveAttribute("aria-pressed", "true");
  await page.getByTestId("rp-pad-allow").click();
  await expect.poll(() => decided).toEqual([{ id: 7, allow: true }]);
  await expect(page.getByTestId("rp-pad-list")).toContainText("Pixel 8");
  await expect(ask).toHaveCount(0);
});

test.describe("a phone as another device's controller", () => {
  test.use({ viewport: { width: 844, height: 390 }, hasTouch: true, isMobile: true });

  test("asks, then plays the console from its on-screen controller", async ({ page }) => {
    const sent: Array<{ buttons: number }> = [];
    let relayUrl = "";
    await page.route(
      (url) => url.pathname.startsWith("/api/"),
      async (route) => {
        const path = new URL(route.request().url()).pathname;
        const body = path === "/api/version" ? { version: "9.5.0", caps: {} } : {};
        await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
      },
    );
    await page.routeWebSocket(/\/api\/ps5\/rp\/pad-relay/, (ws) => {
      relayUrl = ws.url();
      ws.send(JSON.stringify({ stage: "asking" }));
      ws.onMessage((m) => {
        const msg = JSON.parse(String(m)) as { pad?: { buttons: number } };
        if (msg.pad) sent.push(msg.pad);
      });
      setTimeout(() => ws.send(JSON.stringify({ stage: "joined", session: { host: "192.168.1.100", playing: true } })), 600);
    });

    const target = "ws://192.168.1.20:19113/api/pad/ws";
    await page.goto(`/play/pad?url=${encodeURIComponent(target)}&name=Office%20PC`, { waitUntil: "domcontentloaded" });
    await expect(page.getByTestId("pad-stage")).toHaveText("Waiting for Office PC to allow this phone…", { timeout: 30_000 });
    // The app's page reaches the computer through its own engine.
    expect(decodeURIComponent(relayUrl)).toContain(`pad-relay?url=${target}`);

    await expect(page.getByTestId("pad-stage")).toHaveText("Controller for 192.168.1.100");
    await page.getByRole("button", { name: "✕" }).tap();
    await expect.poll(() => sent.some((p) => p.buttons & 1)).toBe(true);
    await expect.poll(() => sent[sent.length - 1]?.buttons).toBe(0);

    // A refusal says so (not "lost").
    await page.getByTestId("pad-leave").tap();
    await expect(page).toHaveURL(/\/play$/);
  });
});
