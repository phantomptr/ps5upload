import { expect, test, type Page } from "@playwright/test";

// Getting the keyboard and mouse back from the PS5 (useNativeKbm.ts, lib/kbmShortcuts.ts): the
// mode (how they are used) and the capture (whether the PS5 has them now) are separate. A click
// on the picture gives them to the PS5; Ctrl+Option / Ctrl+Alt pressed and let go alone gives
// them back; the shortcuts can be changed in Settings › Controls › Shortcuts; the release key is
// shown where people look. On a phone with nothing attached, the screen is a touchpad and ⌨ types.

const SHOTS = process.env.KBM_SHOTS_DIR ?? "";
const MAC_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

interface Engine {
  pads: number[];
  kbm: Array<{ on: boolean; mouse?: boolean; keyboard?: boolean }>;
  reports: number[][];
}

async function mockConsole(page: Page, caps = { mouse: true, keyboard: true }): Promise<Engine> {
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
      if (path === "/api/version") body = { version: "10.3.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/vinput/caps") body = { supported: true, ...caps, open: false, backend: "ps5", companion: "running" };
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
      const msg = JSON.parse(m) as { host?: string; pad?: { buttons: number }; kbm?: Engine["kbm"][number] };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
      if (msg.pad) engine.pads.push(msg.pad.buttons);
      if (msg.kbm) {
        engine.kbm.push(msg.kbm);
        const on = msg.kbm.on;
        ws.send(JSON.stringify({ kbm: { on, mouse: on && !!msg.kbm.mouse, keyboard: on && !!msg.kbm.keyboard, err: null, reason: null } }));
      }
    });
  });
  return engine;
}

/** Keyboard & mouse chosen for the home screen before the page loads. */
async function nativeMode(page: Page) {
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.rp_kbm.v1", JSON.stringify({ modes: { home: "native" }, hinted: true }));
  });
}

async function start(page: Page) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-stage").filter({ visible: true })).toBeVisible();
}

async function capture(page: Page) {
  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  const box = (await stage.boundingBox())!;
  await page.mouse.click(box.x + 40, box.y + box.height - 40);
  return stage;
}

/** Ctrl+Option / Ctrl+Alt pressed and let go with nothing in between. */
async function chord(page: Page) {
  await page.keyboard.down("Control");
  await page.keyboard.down("Alt");
  await page.keyboard.up("Alt");
  await page.keyboard.up("Control");
}

const keysOf = (r: number[]) => r.slice(8).filter((k) => k !== 0);
const last = (e: Engine) => e.reports[e.reports.length - 1];

async function shot(page: Page, name: string) {
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}.png` });
}

test.describe("desktop", () => {
  test.use({ userAgent: MAC_UA });

  test("a click captures, the chord releases, keys stay on the computer while released", async ({ page }) => {
    await nativeMode(page);
    const engine = await mockConsole(page);
    await start(page);
    await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: true });
    const stage = page.getByTestId("rp-stage").filter({ visible: true });
    await stage.focus();

    // Released (never captured): keys are this computer's.
    engine.reports.length = 0;
    await page.keyboard.press("KeyA");
    await page.waitForTimeout(150);
    expect(engine.reports.filter((r) => keysOf(r).length)).toEqual([]);

    await capture(page);
    await expect(page.getByTestId("rp-kbm-chip")).toHaveCount(0); // the first card has the top
    const card = page.getByTestId("rp-kbm-card");
    await expect(card).toHaveAttribute("data-reason", "first");
    await expect(card).toContainText("Your keyboard & mouse now control the PS5. Press ⌃ ⌥ to get them back.");
    await expect(card.getByTestId("rp-keycaps")).toHaveText("⌃⌥");
    await shot(page, "hint-card");
    // It goes by itself; the faint chip stays while captured.
    await expect(card).toHaveCount(0, { timeout: 6000 });
    const chip = page.getByTestId("rp-kbm-chip");
    await expect(chip).toHaveText("⌃ ⌥ to release");
    await shot(page, "release-chip");

    // Captured: keys reach the PS5, and Ctrl+Alt+<key> too (the modifiers go as they happen).
    engine.reports.length = 0;
    await page.keyboard.press("KeyA");
    await expect.poll(() => engine.reports.some((r) => keysOf(r).includes(0x04))).toBe(true);
    engine.reports.length = 0;
    await page.keyboard.press("Control+Alt+KeyD");
    await expect.poll(() => engine.reports.some((r) => r[1] === 0x05 && keysOf(r).includes(0x07))).toBe(true);
    await expect(chip).toBeVisible(); // not released by a combination

    // The chord: everything let go, the capture ends, the mode stays.
    await chord(page);
    await expect(chip).toHaveCount(0);
    await expect.poll(() => last(engine)).toEqual(new Array(16).fill(0));
    await expect(page.getByTestId("rp-capture-hint")).toHaveText("Click the picture to capture the mouse");
    expect(await page.evaluate(() => document.pointerLockElement)).toBeNull();
    expect(engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: true });
    engine.reports.length = 0;
    await page.keyboard.press("KeyB");
    await page.waitForTimeout(150);
    expect(engine.reports.filter((r) => keysOf(r).length)).toEqual([]);

    // Captured again: no first card this time; the mode switch says the mode and the release key.
    await capture(page);
    await expect(chip).toBeVisible();
    await expect(card).toHaveCount(0);
    await page.keyboard.press("Control+Alt+KeyM");
    await expect(page.getByTestId("rp-kbm-toast")).toHaveText("Mixed · ⌃ ⌥ to release");
    await expect(chip).toBeVisible(); // switching never releases
    await expect(page.getByTestId("rp-kbm-lock")).toHaveCount(1);
  });

  test("trying to get out shows the release key again; the chip can be hidden", async ({ page }) => {
    await nativeMode(page);
    await page.addInitScript(() => {
      // As WKWebView: the lock is accepted and never comes (the mouse is sent while over the picture).
      Element.prototype.requestPointerLock = function () {
        return Promise.resolve();
      } as typeof Element.prototype.requestPointerLock;
    });
    await page.addInitScript(() => {
      window.localStorage.setItem("ps5upload.rp_kbm.v1", JSON.stringify({ modes: { home: "native" }, hinted: true, captureHinted: true }));
    });
    const engine = await mockConsole(page);
    await start(page);
    await expect.poll(() => engine.kbm.length).toBeGreaterThan(0);
    await capture(page);
    await expect(page.getByTestId("rp-kbm-chip")).toBeVisible();
    await expect(page.getByTestId("rp-kbm-card")).toHaveCount(0);
    // Esc twice in a second (a refused lock here: the browser's own lock would end on Esc).
    await page.keyboard.press("Escape");
    await page.keyboard.press("Escape");
    const card = page.getByTestId("rp-kbm-card");
    await expect(card).toHaveAttribute("data-reason", "attempt");
    await expect(card).toContainText("To get your keyboard & mouse back, press ⌃ ⌥.");
    await shot(page, "hint-card-attempt");
    await chord(page);
    await expect(page.getByTestId("rp-kbm-card")).toHaveCount(0);

    // Settings › Controls › Shortcuts: the chip off.
    await page.getByTestId("rp-toolbar").hover();
    await page.getByTestId("rp-settings-button").click();
    await page.getByTestId("rp-settings-tab-controls").click();
    await page.getByTestId("rp-shortcuts").getByRole("switch", { name: "Show the release key while playing" }).click();
    await page.getByTestId("rp-settings-close").click();
    await capture(page);
    await page.waitForTimeout(300);
    await expect(page.getByTestId("rp-kbm-chip")).toHaveCount(0);
    expect(JSON.parse((await page.evaluate(() => localStorage.getItem("ps5upload.rp_prefs.v1")))!).kbmChip).toBe(false);
  });

  test("the shortcuts can be changed, are checked, persist and work", async ({ page }) => {
    await nativeMode(page);
    const engine = await mockConsole(page);
    await start(page);
    await expect.poll(() => engine.kbm.length).toBeGreaterThan(0);
    await page.getByTestId("rp-toolbar").hover();
    await page.getByTestId("rp-settings-button").click();
    await page.getByTestId("rp-settings-tab-controls").click();
    const release = page.getByTestId("rp-sc-release");
    const cycle = page.getByTestId("rp-sc-cycle");
    await expect(release.getByTestId("rp-sc-release-keys")).toHaveText("⌃⌥");
    await expect(cycle.getByTestId("rp-sc-cycle-keys")).toHaveText("⌃⌥M");
    await expect(release.getByTestId("rp-sc-release-reset")).toBeDisabled();
    await page.getByTestId("rp-shortcuts").scrollIntoViewIfNeeded();
    await shot(page, "shortcut-settings");

    // A lone letter is refused.
    await release.getByTestId("rp-sc-release-change").click();
    await expect(release.getByTestId("rp-sc-release-listening")).toHaveText("Press the keys… (Esc cancels)");
    await page.keyboard.press("KeyQ");
    await expect(release.getByTestId("rp-sc-release-problem")).toHaveText(
      "A key alone would type in games. Add Ctrl, Alt or another modifier.",
    );
    // The command palette's shortcut is refused for the mode switch.
    await cycle.getByTestId("rp-sc-cycle-change").click();
    await page.keyboard.press("Control+KeyK");
    await expect(cycle.getByTestId("rp-sc-cycle-problem")).toHaveText("The app or the system already uses this.");
    await expect(page.getByRole("dialog", { name: /command/i })).toHaveCount(0);
    // Esc cancels a recording and does not close Settings.
    await cycle.getByTestId("rp-sc-cycle-change").click();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("rp-settings")).toBeVisible();
    await expect(cycle.getByTestId("rp-sc-cycle-keys")).toHaveText("⌃⌥M");

    // F8 releases, Ctrl+Shift+N switches.
    await release.getByTestId("rp-sc-release-change").click();
    await page.keyboard.press("F8");
    await expect(release.getByTestId("rp-sc-release-keys")).toHaveText("F8");
    await cycle.getByTestId("rp-sc-cycle-change").click();
    await page.keyboard.press("Control+Shift+KeyN");
    await expect(cycle.getByTestId("rp-sc-cycle-keys")).toHaveText("⌃⇧N");
    await shot(page, "shortcut-settings-custom");
    const saved = JSON.parse((await page.evaluate(() => localStorage.getItem("ps5upload.rp_kbm_keys.v1")))!);
    expect(saved.release).toEqual({ ctrl: false, alt: false, shift: false, meta: false, code: "F8" });
    await page.getByTestId("rp-settings-close").click();

    await capture(page);
    await expect(page.getByTestId("rp-kbm-card")).toContainText("Press F8 to get them back.");
    await page.getByTestId("rp-kbm-card-ok").click({ force: true }).catch(() => {});
    // The old chord no longer releases; F8 does.
    await chord(page);
    await page.waitForTimeout(200);
    await expect(page.getByTestId("rp-kbm-chip").or(page.getByTestId("rp-kbm-card"))).toBeVisible();
    await page.keyboard.press("Control+Shift+KeyN");
    await expect(page.getByTestId("rp-kbm-toast")).toHaveText("Mixed · F8 to release");
    await page.keyboard.press("F8");
    await expect(page.getByTestId("rp-kbm-chip")).toHaveCount(0);
    await expect(page.getByTestId("rp-capture-hint")).toBeVisible();

    // Reset brings the default back.
    await page.getByTestId("rp-toolbar").hover();
    await page.getByTestId("rp-settings-button").click();
    await page.getByTestId("rp-settings-tab-controls").click();
    await release.getByTestId("rp-sc-release-reset").click();
    await expect(release.getByTestId("rp-sc-release-keys")).toHaveText("⌃⌥");
  });
});

test.describe("phone, nothing attached", () => {
  test.use({ viewport: { width: 844, height: 390 }, hasTouch: true, isMobile: true });

  test("the touchpad moves, clicks and scrolls; ⌨ types on the PS5's keyboard", async ({ page }) => {
    const engine = await mockConsole(page);
    await start(page);
    await expect(page.getByTestId("rp-touch")).toBeVisible();
    const toggle = page.getByTestId("rp-touch-mode");
    await expect(toggle.getByTestId("rp-touch-mode-controller")).toHaveAttribute("aria-checked", "true");
    await toggle.getByTestId("rp-touch-mode-touchpad").tap();
    const pad = page.getByTestId("rp-touchpad");
    await expect(pad).toBeVisible();
    await expect(page.getByTestId("rp-touch")).toHaveCount(0);
    await expect(page.getByTestId("rp-touchpad-tips")).toBeVisible();
    // The devices are wanted in Controller mode too: the touchpad is the mouse.
    await expect.poll(() => engine.kbm[engine.kbm.length - 1]).toEqual({ on: true, mouse: true, keyboard: true });
    await shot(page, "phone-touchpad");

    const fire = (events: Array<[string, number, number, number]>) =>
      pad.evaluate((el, evs) => {
        for (const [type, id, x, y] of evs) {
          el.dispatchEvent(
            new PointerEvent(type, { pointerId: id, pointerType: "touch", clientX: x, clientY: y, bubbles: true, isPrimary: id === 1 }),
          );
        }
      }, events);

    // One finger drag: relative movement (times the speed, 1.5).
    engine.reports.length = 0;
    await fire([
      ["pointerdown", 1, 400, 200],
      ["pointermove", 1, 420, 200],
      ["pointermove", 1, 440, 210],
      ["pointerup", 1, 440, 210],
    ]);
    await expect.poll(() => engine.reports.reduce((s, r) => s + (r[2] | (r[3] << 8)), 0)).toBeGreaterThan(0);
    expect(engine.reports.every((r) => r[0] === 0)).toBe(true); // no click

    // A tap: left click.
    engine.reports.length = 0;
    await fire([
      ["pointerdown", 2, 300, 200],
      ["pointerup", 2, 300, 200],
    ]);
    await expect.poll(() => engine.reports.map((r) => r[0])).toEqual([1, 0]);

    // ⌨: the phone's keyboard; characters become key presses (Shift for capitals).
    await page.waitForTimeout(400); // not a double tap
    await page.getByTestId("rp-tp-keyboard").tap();
    await expect(page.getByTestId("rp-tp-keyboard-input")).toBeFocused();
    engine.reports.length = 0;
    await page.keyboard.type("Hi!");
    await expect.poll(() => engine.reports.filter((r) => keysOf(r).length).map((r) => [r[1], keysOf(r)[0]])).toEqual([
      [0x02, 0x0b], // H
      [0x00, 0x0c], // i
      [0x02, 0x1e], // !
    ]);
    await expect.poll(() => last(engine)).toEqual(new Array(16).fill(0));
    engine.reports.length = 0;
    await page.keyboard.press("Backspace");
    await page.keyboard.press("Enter");
    await expect.poll(() => engine.reports.filter((r) => keysOf(r).length).map((r) => keysOf(r)[0])).toEqual([0x2a, 0x28]);
    // A letter the PS5 keyboard has no key for: a hint, nothing sent.
    engine.reports.length = 0;
    await page.keyboard.insertText("é");
    await expect(page.getByTestId("rp-tp-untypeable")).toBeVisible();
    expect(engine.reports.filter((r) => keysOf(r).length)).toEqual([]);

    // Back to the controller: the on-screen controller returns.
    await page.getByTestId("rp-touch-mode-controller").tap();
    await expect(page.getByTestId("rp-touch")).toBeVisible();
    await expect(pad).toHaveCount(0);
  });

  test("without a native mouse the touchpad is off, with the reason", async ({ page }) => {
    await mockConsole(page, { mouse: false, keyboard: false });
    await start(page);
    const tp = page.getByTestId("rp-touch-mode-touchpad");
    await expect(tp).toHaveAttribute("aria-disabled", "true");
    await expect(tp).toHaveAttribute("title", /isn't available on this console yet/);
    await tp.tap({ force: true });
    await expect(page.getByTestId("rp-touch")).toBeVisible();
    await expect(page.getByTestId("rp-touchpad")).toHaveCount(0);
  });
});
