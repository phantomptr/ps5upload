import { expect, test, type Page } from "@playwright/test";

// Typing into the PS5's on-screen keyboard: when the console opens its keyboard the engine says
// so, and a text box shows over the stream, prefilled and focused. Typing goes to the console a
// moment after each pause, Enter keeps it, Esc cancels (never leaving full screen or pressing
// ○), and the console closing its keyboard closes the box. The game gets no keys meanwhile.

type Engine = {
  start: Record<string, unknown> | null;
  pads: number[];
  texts: string[];
  accepts: number;
  rejects: number;
  send: (m: unknown) => void;
};

async function mockConsole(page: Page): Promise<Engine> {
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
            wake_rp_key: "0f".repeat(16), // made up (built here so it reads as no secret)
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
      if (path === "/api/version") body = { version: "10.2.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const engine: Engine = { start: null, pads: [], texts: [], accepts: 0, rejects: 0, send: () => {} };
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    engine.send = (m) => ws.send(JSON.stringify(m));
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as {
        host?: string;
        pad?: { buttons: number };
        keyboard_text?: string;
        keyboard_accept?: boolean;
        keyboard_reject?: boolean;
      };
      if (msg.host) {
        engine.start = msg;
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
      if (msg.pad) engine.pads.push(msg.pad.buttons);
      if (typeof msg.keyboard_text === "string") engine.texts.push(msg.keyboard_text);
      if (msg.keyboard_accept) engine.accepts++;
      if (msg.keyboard_reject) engine.rejects++;
    });
  });
  return engine;
}

async function startStream(page: Page, engine: Engine) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
  await expect.poll(() => engine.start).not.toBeNull();
}

test("open, type, Enter; then open again and cancel with Esc in full screen; the console closes it", async ({
  page,
}) => {
  const engine = await mockConsole(page);
  await startStream(page, engine);
  // The page asks for the keyboard (an older engine ignores it).
  expect(engine.start?.keyboard).toBe(true);
  const stage = page.getByTestId("rp-stage");
  const box = page.getByTestId("rp-keyboard");
  const input = page.getByTestId("rp-keyboard-input");
  await expect(box).toHaveCount(0);

  // The console opens its keyboard with text in it: the box shows it, focused.
  engine.send({ keyboard: { open: true, text: "Hi" } });
  await expect(box).toBeVisible();
  await expect(input).toHaveValue("Hi");
  await expect(input).toBeFocused();

  // Typing goes to the console as the whole text, once typing pauses; the game gets no keys.
  const pads = engine.pads.length;
  await input.press("End");
  await input.pressSequentially(" thére 日本", { delay: 20 });
  await expect.poll(() => engine.texts[engine.texts.length - 1]).toBe("Hi thére 日本");
  expect(engine.texts.length).toBeLessThan(5);
  // The console's answer does not disturb the box.
  engine.send({ keyboard: { text: "Hi thére 日本" } });
  await input.press("Enter");
  await expect(box).toHaveCount(0);
  expect(engine.accepts).toBe(1);
  expect(engine.pads.slice(pads).every((b) => b === 0)).toBe(true);

  // Full screen, a new opening: typed text is cancelled with Esc, which leaves full screen alone
  // and never presses ○.
  await stage.hover();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-fullscreen").first().click();
  await expect(stage).toHaveAttribute("data-full", "bar");
  engine.send({ keyboard: { open: true, text: "" } });
  await expect(stage.getByTestId("rp-keyboard")).toBeVisible();
  await expect(input).toHaveValue("");
  await expect(input).toBeFocused();
  const sent = engine.texts.length;
  const pads2 = engine.pads.length;
  await input.pressSequentially("nope");
  await page.keyboard.down("Escape");
  await page.waitForTimeout(900); // longer than the hold that leaves full screen
  await page.keyboard.up("Escape");
  await expect(box).toHaveCount(0);
  await expect(stage).toHaveAttribute("data-full", "bar");
  expect(engine.rejects).toBe(1);
  await page.waitForTimeout(300);
  // Cancelled before the pause ran out: the text was never sent.
  expect(engine.texts.length).toBe(sent);
  expect(engine.pads.slice(pads2).every((b) => b === 0)).toBe(true);

  // The console closes its keyboard on its own: the box goes, nothing is sent.
  engine.send({ keyboard: { open: true, text: "abc" } });
  await expect(input).toHaveValue("abc");
  engine.send({ keyboard: { open: false } });
  await expect(box).toHaveCount(0);
  expect(engine.accepts).toBe(1);
  expect(engine.rejects).toBe(1);

  // Keys play again once the box is gone.
  await stage.focus();
  const pads3 = engine.pads.length;
  await page.keyboard.down("Enter");
  await expect.poll(() => engine.pads.slice(pads3).some((b) => b !== 0)).toBe(true);
  await page.keyboard.up("Enter");
});

test("Done and Cancel buttons, for a touch screen with no Enter or Esc", async ({ page }) => {
  const engine = await mockConsole(page);
  await startStream(page, engine);
  const input = page.getByTestId("rp-keyboard-input");

  engine.send({ keyboard: { open: true, text: "" } });
  await input.fill("name");
  await page.getByTestId("rp-keyboard-done").click();
  await expect(page.getByTestId("rp-keyboard")).toHaveCount(0);
  // Done sends what is waiting at once, then accepts.
  expect(engine.texts[engine.texts.length - 1]).toBe("name");
  expect(engine.accepts).toBe(1);

  engine.send({ keyboard: { open: true, text: "x" } });
  await expect(input).toHaveValue("x");
  await page.getByTestId("rp-keyboard-cancel").click();
  await expect(page.getByTestId("rp-keyboard")).toHaveCount(0);
  expect(engine.rejects).toBe(1);
});
