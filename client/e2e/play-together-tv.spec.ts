import { readFile } from "node:fs/promises";
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";
import { routeEngine } from "./fixtures/engine";

// #926: Play Together on Android TV, with nothing but the TV's remote: the arrows move from
// button to button, OK (Enter) presses one, Back closes what is open and then goes back. The
// /join page (any browser, a TV's too) and the app on a TV (MainActivity's PS5UploadTv bridge,
// stubbed here) are both driven with the keyboard only; nothing is clicked.

const INVITE = "00112233445566778899aabbccddeeff";
const LINK = `http://192.168.1.9:19113/join#i=${INVITE}&n=Ann`;

/** The guest page as the engine serves it (the committed build, as play-together.spec.ts). */
async function serveJoinPage(page: Page) {
  const html = await readFile(
    new URL(
      "../../engine/crates/ps5upload-engine/src/join_page.html",
      import.meta.url,
    ),
    "utf8",
  );
  await page.route("**/join", (route) =>
    route.fulfill({ contentType: "text/html", body: html }),
  );
}

const focusedId = (page: Page) =>
  page.evaluate(
    () =>
      document.activeElement?.getAttribute("data-testid") ??
      document.activeElement?.tagName ??
      null,
  );

/** The focused control's ring, in CSS px (0: none). */
const ringWidth = (page: Page) =>
  page.evaluate(() => {
    const s = getComputedStyle(document.activeElement!);
    return s.outlineStyle === "none" ? 0 : parseFloat(s.outlineWidth) || 0;
  });

/** Whether the focus is inside the element with this test id. */
const focusInside = (page: Page, id: string) =>
  page.evaluate(
    (id) => !!document.activeElement?.closest(`[data-testid="${id}"]`),
    id,
  );

/** Press `key` until the control `id` has the focus (at most `max` times); the ids passed on the way. */
async function pressUntil(
  page: Page,
  key: string,
  id: string,
  max = 8,
): Promise<string[]> {
  const seen: string[] = [];
  for (let i = 0; i < max; i++) {
    const at = await focusedId(page);
    if (at) seen.push(at);
    if (at === id) return seen;
    await page.keyboard.press(key);
  }
  throw new Error(`${key} never reached ${id}: ${seen.join(", ")}`);
}

/** The buttons a pad frame holds (its little-endian word after the header). */
const held = (f: number[]) =>
  new DataView(Uint8Array.from(f).buffer).getUint32(5, true);

test("the /join page with a TV's remote: arrows, OK and Back reach everything, and the game keeps its arrows", async ({
  page,
}) => {
  await serveJoinPage(page);
  await page.addInitScript(() =>
    window.localStorage.setItem("ps5upload.join.name", "Ben"),
  );
  const frames: number[][] = [];
  const sockets: WebSocketRoute[] = [];
  let allow: () => void = () => {};
  await page.routeWebSocket(/\/api\/join\/ws/, (ws) => {
    sockets.push(ws);
    ws.onMessage((m) => {
      if (typeof m !== "string") return void frames.push([...m]);
      const v = JSON.parse(m) as { hello?: unknown; ping?: number };
      if (v.ping !== undefined) ws.send(JSON.stringify({ pong: v.ping }));
      if (!v.hello) return;
      ws.send(JSON.stringify({ stage: "asking" }));
      allow = () => {
        ws.send(
          JSON.stringify({
            stage: "joined",
            player: 2,
            token: "TOKEN-1",
            hevc: false,
          }),
        );
        ws.send(
          JSON.stringify({
            status: { player: 2, state: "playing", rtt_ms: 12, host: "Ann" },
          }),
        );
      };
    });
  });
  await page.goto(`/join#i=${INVITE}&n=Ann`, { waitUntil: "domcontentloaded" });

  // "Open in the app?": the remote starts on the first button, Down goes to the browser.
  await expect(page.getByTestId("open-in-app")).toBeFocused({
    timeout: 30_000,
  });
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId("open-in-browser")).toBeFocused();
  // A ring to see from the couch.
  expect(await ringWidth(page)).toBeGreaterThanOrEqual(3);
  await page.keyboard.press("Enter");

  // The name is there from last time: Join has the focus; Up is the name, Down Join again.
  await expect(page.getByTestId("join-go")).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(page.getByTestId("join-name")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByTestId("join-go")).toBeFocused();
  await page.keyboard.press("Enter");

  // Waiting: Cancel is the one thing to press, and has the focus.
  await expect(page.getByTestId("join-stage")).toHaveText(
    "Waiting for Ann to let you in",
    { timeout: 30_000 },
  );
  await expect(page.getByTestId("join-cancel")).toBeFocused();

  allow();
  await expect(page.getByTestId("join-player")).toHaveText("You are Player 2");
  // In the game the focus is the game's: the arrows press the D-pad, they don't move the focus.
  await expect(page.getByTestId("join-game")).toBeFocused();
  expect(await ringWidth(page)).toBe(0);
  frames.length = 0;
  await page.keyboard.down("ArrowUp");
  await expect.poll(() => frames.some((f) => held(f) !== 0)).toBe(true);
  await page.keyboard.up("ArrowUp");
  await expect(page.getByTestId("join-game")).toBeFocused();

  // The first-time guide: Back (Esc here) closes it first, and the game keeps the focus.
  await expect(page.getByTestId("join-guide")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("join-guide")).toHaveCount(0);
  await expect(page.getByTestId("join-game")).toBeFocused();

  // Then the buttons, with the focus on them, every one reachable with the arrows.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("join-bar")).toHaveAttribute("data-shown", "1");
  expect(await focusInside(page, "join-bar")).toBe(true);
  expect(await ringWidth(page)).toBeGreaterThanOrEqual(3);
  const passed = await pressUntil(page, "ArrowRight", "join-leave");
  expect(passed).toContain("join-full");
  expect(passed).toContain("join-check-open");

  // Check your controller: opens on OK, the arrows stay inside it, Esc closes it back onto its button.
  await pressUntil(page, "ArrowLeft", "join-check-open");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("join-pad-check")).toBeVisible();
  expect(await focusInside(page, "join-pad-check")).toBe(true);
  for (const k of [
    "ArrowUp",
    "ArrowLeft",
    "ArrowDown",
    "ArrowRight",
    "ArrowDown",
  ]) {
    await page.keyboard.press(k);
    expect(await focusInside(page, "join-pad-check"), k).toBe(true);
  }
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("join-pad-check")).toHaveCount(0);
  await expect(page.getByTestId("join-check-open")).toBeFocused();

  // Esc from the buttons is back to the game.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("join-game")).toBeFocused();

  // The browser's Back (a TV's remote) from the game brings the buttons and stays on the page.
  await page.evaluate(() => history.back());
  await expect.poll(() => focusInside(page, "join-bar")).toBe(true);
  await expect(page.getByTestId("join-player")).toBeVisible();

  // Leave, with the remote alone.
  await pressUntil(page, "ArrowRight", "join-leave");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("join-name-step")).toBeVisible();
  await expect(page.getByTestId("join-go")).toBeFocused();
  expect(sockets.length).toBeGreaterThan(0);
});

/** The Android app on a TV: MainActivity's bridge says so (scripts/release/android-postinit-patch.sh). */
async function onATv(page: Page) {
  await page.addInitScript(() => {
    (window as unknown as { PS5UploadTv: { isTv(): boolean } }).PS5UploadTv = {
      isTv: () => true,
    };
    window.localStorage.setItem("ps5upload.join.name", "Ben");
    window.localStorage.setItem("ps5upload.join.guided", "1");
  });
}

/** The app's system Back (MainActivity asks the page first): whether the page used it. */
const systemBack = (page: Page) =>
  page.evaluate(
    () =>
      !!(window as unknown as { __ps5uBack?: () => boolean }).__ps5uBack?.(),
  );

test("the app on a TV: Join Play Together from the first screen, and play with the remote", async ({
  page,
}) => {
  await onATv(page);
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.29", caps: {} };
    if (path === "/api/ps5/rp/session") return { host: null };
    return {};
  });
  const frames: number[][] = [];
  const hellos: Array<Record<string, unknown>> = [];
  await page.routeWebSocket(/\/api\/join\/relay/, (ws) => {
    ws.onMessage((m) => {
      if (typeof m !== "string") return void frames.push([...m]);
      const v = JSON.parse(m) as {
        hello?: Record<string, unknown>;
        ping?: number;
      };
      if (v.ping !== undefined) ws.send(JSON.stringify({ pong: v.ping }));
      if (!v.hello) return;
      hellos.push(v.hello);
      ws.send(
        JSON.stringify({
          stage: "joined",
          player: 2,
          token: "APP",
          hevc: false,
        }),
      );
      ws.send(
        JSON.stringify({
          status: { player: 2, state: "playing", rtt_ms: 12, host: "Ann" },
        }),
      );
    });
  });

  // A new install, no PS5 of its own: the first screen offers joining a friend, focused.
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("tv-join-go")).toBeFocused({ timeout: 30_000 });
  // Add a PS5 doesn't open over it on a TV; it is a button away.
  await expect(page.getByTestId("consoles-empty-add")).toBeVisible();
  expect(await ringWidth(page)).toBeGreaterThanOrEqual(3);
  await page.keyboard.press("Enter");

  // Join a friend, the remote on the link (the name is there from last time).
  await expect(page.getByTestId("join-a-friend")).toBeVisible();
  await expect(page.getByTestId("join-link")).toBeFocused();
  await page.keyboard.type(LINK);
  await pressUntil(page, "ArrowDown", "join-app-go");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("join-player")).toHaveText(
    "Playing at Ann's · You are Player 2",
    { timeout: 30_000 },
  );
  expect(hellos[0]).toEqual({ name: "Ben", invite: INVITE });

  // On a TV the game fills the screen at once, and has the focus: the remote's arrows play.
  const stage = page.getByTestId("join-stage-view");
  await expect(stage).toHaveClass(/fixed/);
  await expect(page.getByTestId("join-game")).toBeFocused();
  await expect(page.getByTestId("join-tv-back")).toHaveText(
    "Press Back on the remote for the buttons.",
  );
  frames.length = 0;
  await page.keyboard.down("ArrowLeft");
  await expect.poll(() => frames.some((f) => held(f) !== 0)).toBe(true);
  await page.keyboard.up("ArrowLeft");
  await expect(page.getByTestId("join-game")).toBeFocused();

  // Back: the buttons, the focus on them. Full screen covers the app: the arrows never leave it
  // for the app behind.
  expect(await systemBack(page)).toBe(true);
  expect(await focusInside(page, "join-bar")).toBe(true);
  for (const k of [
    "ArrowUp",
    "ArrowLeft",
    "ArrowLeft",
    "ArrowDown",
    "ArrowDown",
    "ArrowRight",
    "ArrowRight",
    "ArrowRight",
  ]) {
    await page.keyboard.press(k);
    expect(await focusInside(page, "join-stage-view"), k).toBe(true);
  }

  // Back on the buttons leaves full screen, the focus kept on them; Full screen is reachable and
  // OK turns it on again.
  await pressUntil(page, "ArrowLeft", "join-full");
  expect(await systemBack(page)).toBe(true);
  await expect(stage).not.toHaveClass(/fixed/);
  await expect(page.getByTestId("join-full")).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(stage).toHaveClass(/fixed/);
  await expect(page.getByTestId("join-full")).toBeFocused();

  // Then the app's own Back takes over, still in the game.
  expect(await systemBack(page)).toBe(true);
  await expect(stage).not.toHaveClass(/fixed/);
  expect(await systemBack(page)).toBe(false);
  await expect(page.getByTestId("join-player")).toBeVisible();
});

test("away from a TV the first screen is as before", async ({ page }) => {
  await routeEngine(page, (path) =>
    path === "/api/version" ? { version: "12.6.29", caps: {} } : {},
  );
  await page.goto("/connection", { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("consoles-list")).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.getByTestId("tv-join")).toHaveCount(0);
});
