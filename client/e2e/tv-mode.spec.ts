import { expect, test, type Page } from "@playwright/test";
import { LIVING_ROOM, routeEngine } from "./fixtures/engine";

// #976: TV mode. On a TV (MainActivity's PS5UploadTv bridge, stubbed here) the app opens on one
// card for the console with a big Play; everything is reached with the remote's arrows, OK
// (Enter) and Back (the app's system Back, asked of the page first). Nothing is clicked on the
// TV. Elsewhere it is a switch in Settings, and the keyboard's arrows and Esc do the same.

test.use({ viewport: { width: 1920, height: 1080 } });

const BEDROOM = { id: "p2", name: "Bedroom", host: "192.168.1.101" };

/** Saved consoles (Living room selected) before the page loads. */
async function seedConsoles(page: Page, profiles: Array<Record<string, unknown>> = [LIVING_ROOM]) {
  await page.addInitScript((p) => {
    window.localStorage.setItem("ps5upload.host", String(p[0].host));
    window.localStorage.setItem("ps5upload.roster.v1", JSON.stringify({ active_id: p[0].id, profiles: p }));
  }, profiles);
}

/** The Android app on a TV; `leave()` counts the times the app went to the background. */
async function onATv(page: Page) {
  await page.addInitScript(() => {
    const w = window as unknown as { PS5UploadTv: unknown; __left: number };
    w.__left = 0;
    w.PS5UploadTv = { isTv: () => true, leave: () => void (w.__left += 1) };
  });
}

/** An engine that answers, a console asleep, and a Remote Play socket that plays at once. */
async function fakeEngine(page: Page): Promise<{ starts: () => number }> {
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.43", caps: {} };
    if (path === "/api/ps5/rp/session") return { host: null };
    return undefined;
  });
  // Registered last, so it answers first: the helper's port refused, DDP says rest mode.
  await page.route(
    (u) => u.pathname === "/api/ps5/status",
    (route) =>
      route.fulfill({
        status: 502,
        contentType: "application/json",
        body: JSON.stringify({ error: "the console is in rest mode", reason: "console_rest_mode", power: "rest" }),
      }),
  );
  let starts = 0;
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as Record<string, unknown>;
      if ("host" in msg) {
        starts++;
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
      if (msg.stop) {
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
    });
  });
  return { starts: () => starts };
}

const focusedId = (page: Page) =>
  page.evaluate(() => document.activeElement?.getAttribute("data-testid") ?? document.activeElement?.tagName ?? null);

/** Press `key` until the control `id` has the focus (at most `max` times). */
async function pressUntil(page: Page, key: string, id: string, max = 8): Promise<void> {
  const seen: string[] = [];
  for (let i = 0; i < max; i++) {
    const at = await focusedId(page);
    seen.push(String(at));
    if (at === id) return;
    await page.keyboard.press(key);
  }
  throw new Error(`${key} never reached ${id}: ${seen.join(", ")}`);
}

/** The focused control's ring, in CSS px (0: none). */
const ringWidth = (page: Page) =>
  page.evaluate(() => {
    const s = getComputedStyle(document.activeElement!);
    return s.outlineStyle === "none" ? 0 : parseFloat(s.outlineWidth) || 0;
  });

/** The app's system Back (MainActivity asks the page first): whether the page used it. */
const systemBack = (page: Page) =>
  page.evaluate(() => !!(window as unknown as { __ps5uBack?: () => boolean }).__ps5uBack?.());

const left = (page: Page) => page.evaluate(() => (window as unknown as { __left: number }).__left);

const home = (page: Page) => page.locator('[data-testid="tv-home"]:visible');

test("on a TV: the console's card, Play with the remote, full screen, Back to Resume, Back asks before leaving", async ({
  page,
}) => {
  await onATv(page);
  await seedConsoles(page);
  const engine = await fakeEngine(page);
  await page.goto("/", { waitUntil: "domcontentloaded" });

  // TV mode's home, the remote on Play. No sidebar, tab bar or status bar around it.
  await expect(home(page)).toBeVisible({ timeout: 30_000 });
  await expect(page).toHaveURL(/\/tv$/);
  await expect(page.getByTestId("tv-play")).toBeFocused();
  expect(await ringWidth(page)).toBeGreaterThanOrEqual(3);
  await expect(page.getByTestId("tv-console-name")).toHaveText("Living room");
  await expect(page.getByTestId("tv-console-state")).toContainText("In rest mode. Play wakes it.", { timeout: 30_000 });
  await expect(page.locator("[data-top-row]")).toHaveCount(0);
  await expect(page.locator("nav[aria-label]:visible")).toHaveCount(0);

  // Play settings, one press to the right: the essentials, Back closes them.
  await page.keyboard.press("ArrowRight");
  await expect(page.getByTestId("tv-settings-open")).toBeFocused();
  await page.keyboard.press("Enter");
  const settings = page.getByTestId("tv-settings");
  await expect(settings).toBeVisible();
  for (const id of ["rp-res", "rp-fps", "rp-bitrate", "rp-picture-enhance", "rp-audio-buffer", "rp-volume", "tv-settings-tailscale"])
    await expect(settings.getByTestId(id)).toBeVisible();
  await expect(settings.getByRole("switch", { name: "Controller vibration" })).toBeVisible();
  // The remote moves through them, never out of the dialog.
  for (let i = 0; i < 6; i++) {
    await page.keyboard.press("ArrowDown");
    expect(await page.evaluate(() => !!document.activeElement?.closest('[role="dialog"]'))).toBe(true);
  }
  expect(await systemBack(page)).toBe(true);
  await expect(settings).toHaveCount(0);
  await expect(page.getByTestId("tv-settings-open")).toBeFocused();

  // Play: the stream starts (it wakes the console) and fills the screen.
  await pressUntil(page, "ArrowLeft", "tv-play");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/play$/);
  await expect.poll(engine.starts).toBeGreaterThan(0);
  const stage = page.locator('[data-testid="rp-stage"]:visible');
  await expect(stage).toHaveAttribute("data-full", /.+/);
  await expect(stage).toBeFocused();

  // Back: TV mode's home, the game still on, the remote on Resume.
  expect(await systemBack(page)).toBe(true);
  await expect(page).toHaveURL(/\/tv$/);
  await expect(page.getByTestId("tv-console")).toHaveAttribute("data-state", "playing");
  await expect(page.getByTestId("tv-resume")).toBeFocused();
  await expect(page.getByTestId("rp-mini")).toHaveCount(0);

  // Resume: full screen again, on the same stream.
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/play$/);
  await expect(stage).toHaveAttribute("data-full", /.+/);
  expect(engine.starts()).toBe(1);
  expect(await systemBack(page)).toBe(true);
  await expect(page.getByTestId("tv-resume")).toBeFocused();

  // Back on the home asks first; Back again (Stay) keeps the app.
  expect(await systemBack(page)).toBe(true);
  const ask = page.getByRole("alertdialog").or(page.getByRole("dialog"));
  await expect(ask).toContainText("Leave PS5Upload?");
  expect(await systemBack(page)).toBe(true);
  await expect(ask).toHaveCount(0);
  expect(await left(page)).toBe(0);
  // Asked again, Leave: the app goes to the background.
  expect(await systemBack(page)).toBe(true);
  await page.getByRole("button", { name: "Leave", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect.poll(() => left(page)).toBe(1);

  // Stop, after asking: the card is back to Play.
  await pressUntil(page, "ArrowRight", "tv-stop");
  await page.keyboard.press("Enter");
  await page.getByRole("button", { name: "Stop", exact: true }).last().focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("tv-console")).toHaveAttribute("data-state", "rest", { timeout: 30_000 });
});

test("on a TV: Full app and back, and Switch console", async ({ page }) => {
  await onATv(page);
  await seedConsoles(page, [LIVING_ROOM, BEDROOM]);
  await fakeEngine(page);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("tv-play")).toBeFocused({ timeout: 30_000 });

  // Switch console: the picker, the remote down to Bedroom, OK. Its card (not paired: setup).
  await pressUntil(page, "ArrowRight", "tv-switch");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("tv-console-list")).toBeVisible();
  await page.getByTestId("tv-console-p1").focus();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId("tv-console-p2")).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/tv$/);
  const card = page.locator('[data-testid="tv-console"]:visible');
  await expect(card.getByTestId("tv-console-name")).toHaveText("Bedroom");
  await expect(card).toHaveAttribute("data-state", "setup");
  await expect(page.locator('[data-testid="tv-setup"]:visible')).toBeFocused();

  // Not paired: a short way into the pairing setup, and Back comes home.
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/play\/setup$/);
  await expect(page.locator('[data-screen="/play/setup"] h1:visible')).toBeVisible({ timeout: 30_000 });
  expect(await systemBack(page)).toBe(false); // the app moves the history, as MainActivity does
  await page.goBack();
  await expect(page).toHaveURL(/\/tv$/);

  // Full app: the usual app, with a way back.
  await pressUntil(page, "ArrowUp", "tv-full-app");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/home$/);
  await expect(page.locator('[data-testid="tv-return-bar"]:visible')).toBeVisible();
  await page.locator('[data-testid="tv-return"]:visible').focus();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/tv$/);
  await expect(page.locator('[data-testid="tv-return-bar"]:visible')).toHaveCount(0);
});

test("on a computer: the switch in Settings, arrows and Esc, and off again", async ({ page }) => {
  await seedConsoles(page);
  await fakeEngine(page);
  await page.goto("/settings", { waitUntil: "domcontentloaded" });
  const toggle = page.getByTestId("settings-tv-mode").getByRole("switch");
  await expect(toggle).toHaveAttribute("aria-checked", "false", { timeout: 30_000 });
  await toggle.click();
  await expect(page).toHaveURL(/\/tv$/);
  await expect(page.locator("html")).toHaveAttribute("data-tv-mode", "1");
  await expect(page.getByTestId("tv-play")).toBeFocused();
  expect(await ringWidth(page)).toBeGreaterThanOrEqual(4);

  // The arrows move focus here as a remote's do.
  await page.keyboard.press("ArrowRight");
  await expect(page.getByTestId("tv-settings-open")).toBeFocused();
  await pressUntil(page, "ArrowDown", "tv-join-go");
  // Join a friend's game, then Esc is Back: home again.
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("join-a-friend")).toBeVisible();
  // Esc in a field leaves the field (what was typed stays), not the screen.
  const code = page.locator('[data-testid="join-code"]:visible');
  await code.click();
  await page.keyboard.type("ABCD");
  await page.keyboard.press("Escape");
  await expect(code).not.toBeFocused();
  await expect(code).toHaveValue(/ABCD/i);
  await expect(page).toHaveURL(/\/play/);
  await page.keyboard.press("Escape");
  await expect(page).toHaveURL(/\/tv$/);

  // Esc on the home leaves nothing (a browser has no app to leave).
  await page.keyboard.press("Escape");
  await expect(page).toHaveURL(/\/tv$/);

  // Off again in Settings: the usual landing and no bar back.
  await page.getByTestId("tv-full-app").click();
  await expect(page.locator('[data-testid="tv-return-bar"]:visible')).toBeVisible();
  await page.goto("/settings", { waitUntil: "domcontentloaded" });
  await page.getByTestId("settings-tv-mode").getByRole("switch").click();
  await expect(page.locator('[data-testid="tv-return-bar"]:visible')).toHaveCount(0);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page).not.toHaveURL(/\/tv$/);
});

// A wide font, as on Linux and some Android TVs: labels must not be cut.
const WIDE_FONT = "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}";

for (const [label, viewport] of [
  ["TV", { width: 960, height: 540 }],
  ["phone", { width: 360, height: 780 }],
] as const) {
  test(`TV mode's labels fit on a ${label} in German and Russian, in a wide font`, async ({ page }) => {
    await page.setViewportSize(viewport);
    await onATv(page);
    await seedConsoles(page, [LIVING_ROOM, BEDROOM]);
    await fakeEngine(page);
    for (const lang of ["de", "ru"]) {
      await page.addInitScript((l) => window.localStorage.setItem("ps5upload.lang", l), lang);
      await page.goto("/tv", { waitUntil: "domcontentloaded" });
      await expect(page.getByTestId("tv-play")).toBeVisible({ timeout: 30_000 });
      await page.addStyleTag({ content: WIDE_FONT });
      const bad = await page.evaluate(() => {
        const out: string[] = [];
        for (const b of document.querySelectorAll<HTMLElement>('[data-testid="tv-home"] button')) {
          const r = b.getBoundingClientRect();
          if (b.scrollWidth > b.clientWidth + 1 || r.left < -1 || r.right > window.innerWidth + 1) out.push(b.innerText);
        }
        if (document.documentElement.scrollWidth > window.innerWidth + 1) out.push("page scrolls sideways");
        return out;
      });
      expect(bad, lang).toEqual([]);
    }
  });
}
