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
  /** Times the page asked GET /api/ps5/vinput/caps. */
  capsAsked: number;
  /** The `err` the engine's {"kbm"} answer carries while on (the console's helper starting…). */
  err: string | null;
}

async function mockConsole(
  page: Page,
  caps: { mouse: boolean; keyboard: boolean },
  extra: Record<string, unknown> = {},
): Promise<Engine> {
  const engine: Engine = { pads: [], kbm: [], reports: [], capsAsked: 0, err: null };
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
      if (path === "/api/ps5/vinput/caps") {
        engine.capsAsked++;
        body = {
          supported: true,
          ...caps,
          open: false,
          backend: caps.mouse ? "ps5" : "stub",
          ...(caps.mouse || caps.keyboard ? {} : { reason: "vinput_unsupported" }),
          ...extra,
        };
      }
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
              err: on ? engine.err : null,
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

// The stream bar's keyboard & mouse button (KbmBarButton in StreamPanel.tsx), before Game.
async function barButton(page: Page) {
  await page.getByTestId("rp-toolbar").hover();
  const b = page.getByTestId("rp-kbm-button");
  await expect(b).toBeVisible();
  return b;
}
const iconOf = (page: Page) => page.getByTestId("rp-kbm-button-icon");

test("the bar button shows the mode and cycles it like Ctrl+Alt+M", async ({ page }) => {
  const engine = await mockConsole(page, { mouse: true, keyboard: true }, { companion: "idle" });
  await start(page);
  const b = await barButton(page);
  // Right before Game on the toolbar.
  const order = await page
    .getByTestId("rp-toolbar")
    .evaluate((bar) => [...bar.querySelectorAll("[data-testid]")].map((e) => e.getAttribute("data-testid")));
  expect(order.indexOf("rp-game") - order.indexOf("rp-kbm-button")).toBe(2); // the button, its icon, Game
  await expect(b).toHaveAttribute("aria-label", "Controller — applies to the home screen");
  await expect(b).not.toHaveAttribute("aria-disabled", "true");
  await expect(iconOf(page)).toHaveAttribute("data-kbm-icon", "controller");

  await b.click();
  await expect(b).toHaveAttribute("aria-label", "Keyboard & mouse — applies to the home screen");
  await expect(iconOf(page)).toHaveAttribute("data-kbm-icon", "native");
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: true });

  await b.click();
  await expect(b).toHaveAttribute("aria-label", "Mixed — applies to the home screen");
  await expect(iconOf(page)).toHaveAttribute("data-kbm-icon", "mixed");
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: false });
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_kbm.v1") ?? "{}"));
  expect(saved.modes.home).toBe("mixed");

  await b.click();
  await expect(b).toHaveAttribute("aria-label", "Controller — applies to the home screen");
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: false });
});

test("the bar button is off, with the reason, when the console has no payload loader", async ({ page }) => {
  const engine = await mockConsole(
    page,
    { mouse: false, keyboard: false },
    { reason: "vinput_needs_loader", companion: "unavailable" },
  );
  await start(page);
  const b = await barButton(page);
  await expect(b).toHaveAttribute("aria-disabled", "true");
  await expect(b).toHaveAttribute("aria-label", "Needs a payload loader on port 9021");
  await expect(b).toHaveAttribute("title", "Needs a payload loader on port 9021");
  await expect(b).toBeDisabled(); // aria-disabled: still focusable, so the reason can be read
  await b.click({ force: true });
  await page.waitForTimeout(600);
  expect(engine.kbm).toEqual([]);
  await expect(iconOf(page)).toHaveAttribute("data-kbm-icon", "controller");
  // Settings › Controls says the same.
  const kbm = await openControls(page);
  await expect(kbm.getByTestId("rp-kbm-unavailable")).toHaveText("Needs a payload loader on port 9021");
});

test("the bar button spins while the console's keyboard & mouse helper starts", async ({ page }) => {
  const engine = await mockConsole(page, { mouse: true, keyboard: true }, { companion: "idle" });
  engine.err = "vinput_starting";
  await start(page);
  const b = await barButton(page);
  await expect.poll(() => engine.capsAsked).toBeGreaterThanOrEqual(1);
  const asked = engine.capsAsked;
  await b.click();
  await expect(iconOf(page)).toHaveAttribute("data-kbm-icon", "spinner");
  await expect(iconOf(page).locator(".animate-spin")).toHaveCount(1);
  await expect(b).toHaveAttribute("aria-label", "Starting keyboard & mouse on the PS5…");
  // The error changed: the console is asked again.
  await expect.poll(() => engine.capsAsked).toBeGreaterThan(asked);
  // Up: the mode's icon is back.
  engine.err = null;
  await expect(iconOf(page)).toHaveAttribute("data-kbm-icon", "native", { timeout: 5_000 });
  await expect(b).toHaveAttribute("aria-label", "Keyboard & mouse — applies to the home screen");
});

// Capturing the mouse (lib/pointerCapture.ts). WKWebView in the macOS app accepts
// requestPointerLock and never locks: the app then grabs the window's cursor (Tauri's
// setCursorGrab / setCursorVisible) and mousemove carries the movement. A browser that refuses the
// lock sends the mouse while it is over the picture instead.

/** requestPointerLock that never locks, as in WKWebView. */
async function noPointerLock(page: Page) {
  await page.addInitScript(() => {
    Element.prototype.requestPointerLock = function () {
      return Promise.resolve();
    } as typeof Element.prototype.requestPointerLock;
  });
}

/** The macOS desktop app's IPC, recording the window's cursor calls. */
async function desktopApp(page: Page) {
  await page.addInitScript(() => {
    const ua = navigator.userAgent.replace(/\([^)]*\)/, "(Macintosh; Intel Mac OS X 10_15_7)");
    Object.defineProperty(navigator, "userAgent", { get: () => ua });
    const w = window as unknown as Record<string, unknown>;
    const calls: Array<[string, unknown]> = [];
    w.__cursorCalls = calls;
    let id = 0;
    w.__TAURI_INTERNALS__ = {
      invoke: async (cmd: string, args: { value?: unknown }) => {
        if (cmd.startsWith("plugin:window|set_cursor_")) calls.push([cmd.slice("plugin:window|".length), args?.value]);
        return null;
      },
      transformCallback: () => ++id,
      unregisterCallback: () => {},
      convertFileSrc: (p: string) => p,
      metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
      plugins: {},
    };
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  });
}
const cursorCalls = (page: Page) =>
  page.evaluate(() => (window as unknown as { __cursorCalls: Array<[string, unknown]> }).__cursorCalls.slice());
const GRAB = [
  ["set_cursor_grab", true],
  ["set_cursor_visible", false],
];
const UNGRAB = [
  ["set_cursor_grab", false],
  ["set_cursor_visible", true],
];
/** Reports that moved the mouse. */
const moves = (e: Engine) => e.reports.filter((r) => r[2] | r[3] | r[4] | r[5]);

async function nativeMode(page: Page, engine: Engine) {
  const kbm = await openControls(page);
  await kbm.getByTestId("rp-kbm-mode-native").click();
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: true });
  await closeSettings(page);
}

test("the desktop app grabs the cursor when no pointer lock comes, sends the mouse, and always gives it back", async ({
  page,
}) => {
  await desktopApp(page);
  await noPointerLock(page);
  const engine = await mockConsole(page, { mouse: true, keyboard: true });
  await start(page);
  await nativeMode(page, engine);
  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  const hint = page.getByTestId("rp-capture-hint");
  await expect(hint).toHaveText("Click the picture to capture the mouse");

  // Before a click nothing is sent.
  const box = (await stage.boundingBox())!;
  const cx = box.x + box.width / 2;
  const cy = box.y + box.height / 2;
  await page.mouse.move(cx, cy);
  engine.reports.length = 0;
  await page.mouse.move(cx + 20, cy + 10);
  await page.waitForTimeout(200);
  expect(moves(engine)).toEqual([]);

  await stage.click();
  await expect.poll(() => cursorCalls(page)).toEqual(GRAB);
  await expect(hint).toHaveCount(0);
  await expect(stage).toHaveAttribute("data-mouse-grab", "");
  // Moves reach the PS5's mouse.
  engine.reports.length = 0;
  await page.mouse.move(cx + 40, cy + 25);
  await expect.poll(() => moves(engine).length).toBeGreaterThan(0);

  // Esc held gives the mouse back.
  await page.keyboard.down("Escape");
  await expect.poll(() => cursorCalls(page)).toEqual([...GRAB, ...UNGRAB]);
  await page.keyboard.up("Escape");
  await expect(hint).toBeVisible();
  await expect(stage).not.toHaveAttribute("data-mouse-grab", "");
  engine.reports.length = 0;
  await page.mouse.move(cx, cy);
  await page.waitForTimeout(200);
  expect(moves(engine)).toEqual([]);

  // Again; the window losing the focus gives it back.
  await stage.click();
  await expect.poll(() => cursorCalls(page)).toEqual([...GRAB, ...UNGRAB, ...GRAB]);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await expect.poll(() => cursorCalls(page)).toEqual([...GRAB, ...UNGRAB, ...GRAB, ...UNGRAB]);

  // Settings opening gives it back.
  await stage.click();
  await expect.poll(async () => (await cursorCalls(page)).length).toBe(10);
  await openControls(page);
  await expect.poll(async () => (await cursorCalls(page)).slice(-2)).toEqual(UNGRAB);
  // Controller mode gives it back.
  await page.getByTestId("rp-kbm").getByTestId("rp-kbm-mode-mixed").click();
  await closeSettings(page);
  await stage.click();
  await expect.poll(async () => (await cursorCalls(page)).slice(-2)).toEqual(GRAB);
  await page.keyboard.press("Control+Alt+KeyM"); // Mixed → Controller
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: false });
  await expect.poll(async () => (await cursorCalls(page)).slice(-2)).toEqual(UNGRAB);

  // Leaving the Play screen gives it back.
  await page.keyboard.press("Control+Alt+KeyM"); // → Keyboard & mouse
  await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: true });
  await stage.click();
  await expect.poll(async () => (await cursorCalls(page)).slice(-2)).toEqual(GRAB);
  await page.evaluate(() => {
    window.history.pushState({}, "", "/connection");
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  await expect.poll(async () => (await cursorCalls(page)).slice(-2)).toEqual(UNGRAB);
});

test("a browser that refuses the pointer lock sends the mouse while it is over the picture", async ({ page }) => {
  await noPointerLock(page);
  const engine = await mockConsole(page, { mouse: true, keyboard: true });
  await start(page);
  await nativeMode(page, engine);
  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  await expect(page.getByTestId("rp-capture-hint")).toHaveText("Click the picture to capture the mouse");
  await stage.click();
  await page.waitForTimeout(300); // the lock's wait runs out
  await expect(page.getByTestId("rp-capture-hint")).toBeVisible();
  const box = (await stage.boundingBox())!;
  engine.reports.length = 0;
  await page.mouse.move(box.x + box.width / 2 + 30, box.y + box.height / 2 + 15);
  await expect.poll(() => moves(engine).length).toBeGreaterThan(0);
  // Off the picture, nothing.
  await page.mouse.move(box.x + box.width / 2, box.y - 5);
  await page.waitForTimeout(100);
  engine.reports.length = 0;
  await page.mouse.move(box.x + box.width / 2 + 10, box.y - 30);
  await page.waitForTimeout(200);
  expect(moves(engine)).toEqual([]);
});
