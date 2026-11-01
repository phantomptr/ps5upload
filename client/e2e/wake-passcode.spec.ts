import { expect, test, type Page } from "@playwright/test";

// Wake & sign in when the PS5 user has a login passcode (#608): the engine holds the sign-in and
// answers {"state":"passcode","id"}; the app shows the passcode box over everything and continues
// through /api/ps5/power/wake-login/passcode with that id. Wrong asks again, Cancel leaves the PS5
// at its sign-in screen, five wrong ones say to enter it on the PS5.

type Engine = { wake: Record<string, unknown> | null; answers: Record<string, unknown>[] };

async function sleepingConsole(page: Page, onAnswer: (n: number, a: Record<string, unknown>) => unknown) {
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
  const engine: Engine = { wake: null, answers: [] };
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let status = 200;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.5.0", caps: {} };
      if (path === "/api/ps5/power/ddp-status") body = { code: 620, status_text: "Server Standby" };
      if (path === "/api/ps5/power/wake-login") {
        engine.wake = route.request().postDataJSON() as Record<string, unknown>;
        body = { state: "passcode", id: "tok-1", retry: false, wait_s: 180 };
      }
      if (path === "/api/ps5/power/wake-login/passcode") {
        const a = route.request().postDataJSON() as Record<string, unknown>;
        engine.answers.push(a);
        const r = onAnswer(engine.answers.length, a) as { status?: number; body: unknown };
        status = r.status ?? 200;
        body = r.body;
      }
      await route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  return engine;
}

async function pressWake(page: Page, engine: Engine) {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Wake & sign in" }).first().click({ timeout: 30_000 });
  await expect.poll(() => engine.wake).not.toBeNull();
  expect(engine.wake!.ask_passcode).toBe(true);
}

test("Wake & sign in asks for the passcode, asks again after a wrong one, then signs in", async ({ page }) => {
  const engine = await sleepingConsole(page, (n) =>
    n === 1
      ? { body: { state: "passcode", id: "tok-1", retry: true, wait_s: 180 } }
      : { body: { ok: true, signed_in: true } },
  );
  await pressWake(page, engine);
  const box = page.getByTestId("wake-passcode");
  const input = page.getByTestId("wake-passcode-input");
  await expect(box).toBeVisible();
  await expect(input).toBeFocused();
  await expect(input).toHaveAttribute("type", "password");
  await expect(page.getByTestId("wake-passcode-wrong")).toHaveCount(0);
  await expect(page.getByTestId("wake-passcode-ok")).toBeDisabled();
  await input.fill("0000");
  await input.press("Enter");
  await expect(page.getByTestId("wake-passcode-wrong")).toHaveText("Wrong passcode, try again.");
  // A fresh, empty box.
  await expect(input).toHaveValue("");
  await input.fill("1234");
  await page.getByTestId("wake-passcode-ok").click();
  await expect(box).toHaveCount(0);
  expect(engine.answers).toEqual([
    { id: "tok-1", passcode: "0000" },
    { id: "tok-1", passcode: "1234" },
  ]);
  await expect(page.getByText("Woken and signed in").first()).toBeVisible();
  // Never kept in the page's storage.
  const stored = await page.evaluate(() => JSON.stringify(window.localStorage));
  expect(stored).not.toContain("1234");
});

test("Cancel leaves the PS5 at its sign-in screen and says so", async ({ page }) => {
  const engine = await sleepingConsole(page, () => ({
    status: 502,
    body: { error: "the login passcode prompt was cancelled", code: "passcode_cancelled" },
  }));
  await pressWake(page, engine);
  await expect(page.getByTestId("wake-passcode")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("wake-passcode")).toHaveCount(0);
  expect(engine.answers).toEqual([{ id: "tok-1", cancel: true }]);
  await expect(page.getByText("The PS5 stays at its sign-in screen.").first()).toBeVisible();
});

test("too many wrong passcodes say to enter it on the PS5", async ({ page }) => {
  const engine = await sleepingConsole(page, () => ({
    status: 502,
    body: { error: "too many wrong login passcodes", code: "passcode_too_many" },
  }));
  await pressWake(page, engine);
  await page.getByTestId("wake-passcode-input").fill("9999");
  await page.getByTestId("wake-passcode-input").press("Enter");
  await expect(page.getByTestId("wake-passcode")).toHaveCount(0);
  await expect(page.getByText("Too many wrong passcodes; enter it on the PS5.").first()).toBeVisible();
});
