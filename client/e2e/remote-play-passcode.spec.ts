import { expect, test, type Page } from "@playwright/test";

// The PS5 user's sign-in passcode (#608): the console asks for it before it starts the stream,
// the engine says so ({"stage":"passcode"}), and a four-digit prompt shows over the stream. The
// answer goes to the engine and nowhere else; a wrong one asks again; Cancel ends the start.

type Engine = {
  start: Record<string, unknown> | null;
  passcodes: string[];
  cancels: number;
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
      if (path === "/api/version") body = { version: "10.5.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  const engine: Engine = { start: null, passcodes: [], cancels: 0, send: () => {} };
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    engine.send = (m) => ws.send(JSON.stringify(m));
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string; passcode?: string; passcode_cancel?: boolean };
      if (msg.host) {
        engine.start = msg;
        // The console wants the passcode before the session id.
        ws.send(JSON.stringify({ stage: "nonce", ms: 5 }));
        ws.send(JSON.stringify({ stage: "passcode", retry: false, wait_s: 180 }));
      }
      if (typeof msg.passcode === "string") engine.passcodes.push(msg.passcode);
      if (msg.passcode_cancel) engine.cancels++;
    });
  });
  return engine;
}

async function startStream(page: Page, engine: Engine) {
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect.poll(() => engine.start).not.toBeNull();
}

test("the passcode prompt sends four digits, asks again after a wrong one, and goes when the stream starts", async ({
  page,
}) => {
  const engine = await mockConsole(page);
  await startStream(page, engine);
  const box = page.getByTestId("rp-passcode");
  const input = page.getByTestId("rp-passcode-input");
  const ok = page.getByTestId("rp-passcode-ok");
  await expect(box).toBeVisible();
  await expect(input).toBeFocused();
  // A number pad on a phone, masked.
  await expect(input).toHaveAttribute("inputmode", "numeric");
  await expect(input).toHaveAttribute("type", "password");
  await expect(page.getByTestId("rp-passcode-wrong")).toHaveCount(0);

  // Only digits, at most four; OK waits for all four.
  await input.pressSequentially("1a2");
  await expect(input).toHaveValue("12");
  await expect(ok).toBeDisabled();
  await input.pressSequentially("345");
  await expect(input).toHaveValue("1234");
  await input.press("Enter");
  await expect.poll(() => engine.passcodes).toEqual(["1234"]);
  await expect(box).toHaveCount(0);

  // Wrong: asked again, empty, with the reason.
  engine.send({ stage: "passcode", retry: true, wait_s: 180 });
  await expect(box).toBeVisible();
  await expect(page.getByTestId("rp-passcode-wrong")).toBeVisible();
  await expect(input).toHaveValue("");
  await input.fill("5678");
  await ok.click();
  await expect.poll(() => engine.passcodes).toEqual(["1234", "5678"]);
  await expect(box).toHaveCount(0);

  // Asked a third time, then the console takes it after all: the prompt goes by itself.
  engine.send({ stage: "passcode", retry: true, wait_s: 180 });
  await expect(box).toBeVisible();
  engine.send({ stage: "session", ms: 900 });
  await expect(box).toHaveCount(0);
  expect(engine.cancels).toBe(0);

  // Never kept anywhere the page stores things.
  const stored = await page.evaluate(() => JSON.stringify({ ...window.localStorage }) + JSON.stringify({ ...window.sessionStorage }));
  expect(stored).not.toContain("1234");
  expect(stored).not.toContain("5678");
});

test("Cancel ends the start and says why", async ({ page }) => {
  const engine = await mockConsole(page);
  await startStream(page, engine);
  const box = page.getByTestId("rp-passcode");
  await expect(box).toBeVisible();
  await page.getByTestId("rp-passcode-cancel").click();
  await expect.poll(() => engine.cancels).toBe(1);
  await expect(box).toHaveCount(0);
  expect(engine.passcodes).toEqual([]);
  // The engine ends the start with its own code.
  engine.send({ stage: "error", code: "passcode_cancelled", error: "the login passcode prompt was cancelled" });
  await expect(page.getByText("Start cancelled: the PS5 needs this user's passcode")).toBeVisible();
});

test("Esc cancels too, and a timeout says to start again", async ({ page }) => {
  const engine = await mockConsole(page);
  await startStream(page, engine);
  await expect(page.getByTestId("rp-passcode")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect.poll(() => engine.cancels).toBe(1);
  engine.send({ stage: "error", code: "passcode_timeout", error: "no login passcode was entered in time" });
  await expect(page.getByText("The PS5 stopped waiting for the passcode")).toBeVisible();
});
