import { expect, test, type Page } from "@playwright/test";

// Native keyboard & mouse (lib/kbmInput.ts): Settings › Controls picks Controller, Keyboard & mouse or
// Mixed; the console's answer gates them; keys go to exactly one sink; everything is let go on blur;
// Ctrl+Alt+M cycles. A console without the devices (today's helpers) keeps controller emulation
// exactly as before and gets no keyboard & mouse messages at all.

const status = {
  command_count: 1,
  instance_id: 1,
  prior_instance: "clean",
  ps5_kernel: "r229358/releases/13.60",
  ucred_elevated: true,
  version: "10.2.1",
};
const CROSS = 1;

interface Engine {
  pads: number[];
  kbm: Array<{ on: boolean; mouse?: boolean; keyboard?: boolean }>;
  reports: number[][];
}

async function mockConsole(page: Page, caps: { mouse: boolean; keyboard: boolean }): Promise<Engine> {
  const engine: Engine = { pads: [], kbm: [], reports: [] };
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [
          { id: "p1", name: "Living room", host: "192.168.1.100", wake_regist_key: "00112233", wake_rp_key: "00000000000000000000000000000001" },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.2.1", caps: {} };
      if (path === "/api/ps5/status") body = status;
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/vinput/caps")
        body = {
          supported: true,
          ...caps,
          open: false,
          backend: caps.mouse ? "ps5" : "stub",
          ...(caps.mouse || caps.keyboard ? {} : { reason: "vinput_unsupported" }),
        };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if (typeof m !== "string") {
        const b = [...new Uint8Array(m as Buffer)];
        if (b[0] === 8) engine.reports.push(b.slice(1));
        return;
      }
      const msg = JSON.parse(m) as {
        host?: string;
        pad?: { buttons: number };
        kbm?: { on: boolean; mouse?: boolean; keyboard?: boolean };
      };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
      if (msg.pad) engine.pads.push(msg.pad.buttons);
      if (msg.kbm) {
        engine.kbm.push(msg.kbm);
        // What the PS5 sees: what was asked, where the console can make it.
        const on = msg.kbm.on;
        ws.send(
          JSON.stringify({
            kbm: {
              on,
              mouse: on && !!msg.kbm.mouse && caps.mouse,
              keyboard: on && !!msg.kbm.keyboard && caps.keyboard,
              err: null,
              reason: on ? null : "off",
            },
          }),
        );
      }
    });
  });
  return engine;
}

async function start(page: Page) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-stage").filter({ visible: true })).toBeVisible();
}

async function openControls(page: Page) {
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-settings-button").click();
  await page.getByTestId("rp-settings-tab-controls").click();
  return page.getByTestId("rp-kbm");
}

async function closeSettings(page: Page) {
  await page.getByTestId("rp-settings-close").click();
  await expect(page.getByTestId("rp-settings")).toHaveCount(0);
}

const keysOf = (r: number[]) => r.slice(8).filter((k) => k !== 0);

test("a console without the devices: native modes are off, controller emulation is unchanged", async ({ page }) => {
  const engine = await mockConsole(page, { mouse: false, keyboard: false });
  await start(page);
  const kbm = await openControls(page);
  await expect(kbm.getByTestId("rp-kbm-unavailable")).toHaveText(
    "Native keyboard & mouse isn't available on this console yet.",
  );
  await expect(kbm.getByTestId("rp-kbm-mode-controller")).toHaveAttribute("aria-checked", "true");
  await expect(kbm.getByTestId("rp-kbm-mode-native")).toBeDisabled();
  await expect(kbm.getByTestId("rp-kbm-mode-mixed")).toBeDisabled();
  await expect(kbm.getByTestId("rp-kbm-status")).toHaveText("PS5 sees: mouse – keyboard –");
  await closeSettings(page);

  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  await stage.focus();
  engine.pads.length = 0;
  await page.keyboard.press("Enter");
  await expect.poll(() => engine.pads.includes(CROSS)).toBe(true);
  await expect.poll(() => engine.pads[engine.pads.length - 1]).toBe(0);
  // The hotkey has nothing to switch to, and no keyboard & mouse message ever went out.
  await page.keyboard.press("Control+Alt+KeyM");
  await page.waitForTimeout(700);
  expect(engine.kbm).toEqual([]);
  expect(engine.reports).toEqual([]);
});

test("Keyboard & mouse sends raw keys, Mixed sends the pad, the hotkey cycles and blur lets go", async ({ page }) => {
  const engine = await mockConsole(page, { mouse: true, keyboard: true });
  await start(page);
  const kbm = await openControls(page);
  await expect(kbm.getByTestId("rp-kbm-mode-native")).toBeEnabled();
  await kbm.getByTestId("rp-kbm-mode-native").click();
  await expect(kbm.getByTestId("rp-kbm-mode-native")).toHaveAttribute("aria-checked", "true");
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: true });
  await expect(kbm.getByTestId("rp-kbm-status")).toHaveText("PS5 sees: mouse ✓ keyboard ✓");
  // The first pick says what to do if the game ignores the mouse.
  await expect(kbm.getByTestId("rp-kbm-hint")).toHaveText("If the game ignores the mouse, switch back to Controller.");
  // While Settings is open, keys are the app's.
  engine.reports.length = 0;
  await page.keyboard.press("KeyA");
  expect(engine.reports.filter((r) => keysOf(r).length)).toEqual([]);
  await closeSettings(page);

  // Keys go to the PS5's keyboard as HID usages, and never to the pad.
  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  await stage.focus();
  engine.pads.length = 0;
  engine.reports.length = 0;
  await page.keyboard.press("Enter");
  await expect.poll(() => engine.reports.some((r) => keysOf(r).includes(0x28))).toBe(true);
  await expect.poll(() => keysOf(engine.reports[engine.reports.length - 1])).toEqual([]);
  expect(engine.pads.filter((b) => b & CROSS)).toEqual([]);

  // The heartbeat keeps going.
  const beats = engine.kbm.length;
  await page.waitForTimeout(1200);
  expect(engine.kbm.length).toBeGreaterThanOrEqual(beats + 2);

  // A key held when the window loses the focus is let go.
  await page.keyboard.down("KeyW");
  await expect.poll(() => keysOf(engine.reports[engine.reports.length - 1])).toEqual([0x1a]);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await expect.poll(() => engine.reports[engine.reports.length - 1]).toEqual(new Array(16).fill(0));
  await page.keyboard.up("KeyW");

  // Ctrl+Alt+M: Mixed (mouse only; keys are the pad again), remembered for the home screen.
  await stage.focus();
  await page.keyboard.press("Control+Alt+KeyM");
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: false });
  engine.pads.length = 0;
  engine.reports.length = 0;
  await page.keyboard.press("Enter");
  await expect.poll(() => engine.pads.includes(CROSS)).toBe(true);
  expect(engine.reports.filter((r) => keysOf(r).length)).toEqual([]);
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_kbm.v1") ?? "{}"));
  expect(saved.modes.home).toBe("mixed");

  // Once more: Controller, and the devices are let go.
  await page.keyboard.press("Control+Alt+KeyM");
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: false });
});
