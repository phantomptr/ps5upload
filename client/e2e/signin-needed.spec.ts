import { expect, test } from "@playwright/test";

// "The PS5 is waiting at sign-in": a feature that needs a signed-in user (here, Play on a game)
// says so once, with Sign in, which runs Wake & sign in and its passcode box. Phones used as
// controllers only hear that the PS5 waits for the passcode on the computer.

const status = {
  command_count: 1,
  instance_id: 1,
  prior_instance: "clean",
  ps5_kernel: "r229358/releases/13.60",
  ucred_elevated: true,
  version: "10.5.0",
};

test("Play with nobody signed in offers Sign in, which asks for the passcode", async ({ page }) => {
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
  let wake: Record<string, unknown> | null = null;
  const answers: unknown[] = [];
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let code = 200;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.5.0", caps: {} };
      if (path === "/api/ps5/status") body = status;
      if (path === "/api/ps5/apps/installed")
        body = { titles: [{ title_id: "PPSA01234", title_name: "Astro Bot", origin: "pkg" }] };
      if (path === "/api/ps5/process/list") body = { truncated: false, processes: [] };
      if (path === "/api/ps5/app/launch") {
        code = 502;
        body = { error: "payload rejected APP_LAUNCH: launch_no_user: rc=0x80940005" };
      }
      if (path === "/api/ps5/power/wake-login") {
        wake = route.request().postDataJSON() as Record<string, unknown>;
        body = { state: "passcode", id: "tok-9", retry: false, wait_s: 180 };
      }
      if (path === "/api/ps5/power/wake-login/passcode") {
        answers.push(route.request().postDataJSON());
        body = { ok: true, signed_in: true };
      }
      await route.fulfill({ status: code, contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.goto("/games", { waitUntil: "domcontentloaded" });
  const play = page.getByTestId("collection-card").filter({ hasText: "Astro Bot" }).getByTestId("card-play");
  await expect(play).toBeVisible({ timeout: 30_000 });
  await play.click();
  const dialog = page.getByRole("dialog", { name: "The PS5 is waiting at sign-in" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("This needs a signed-in user.");
  await page.getByTestId("signin-needed-go").click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => wake?.ask_passcode).toBe(true);
  const input = page.getByTestId("wake-passcode-input");
  await expect(input).toBeVisible();
  await input.fill("4321");
  await input.press("Enter");
  await expect(page.getByTestId("wake-passcode")).toHaveCount(0);
  expect(answers).toEqual([{ id: "tok-9", passcode: "4321" }]);
  // One notification, the sign-in (the launch failure was not reported as an error).
  await page.getByRole("button", { name: "Open notifications" }).click();
  await expect(page.getByText("Woken and signed in").first()).toBeVisible();
  await expect(page.getByText("No one is signed in on the PS5")).toHaveCount(0);
});

test("a phone controller hears the PS5 waits for the passcode on the computer", async ({ page }) => {
  await page.setViewportSize({ width: 844, height: 390 });
  const target = "ws://192.168.1.20:19114/api/ps5/rp/pad-ws?key=abc";
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      const body = path === "/api/version" ? { version: "10.5.0", caps: {} } : {};
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  let sent: string[] = [];
  await page.routeWebSocket(/\/api\/ps5\/rp\/pad-relay/, (ws) => {
    ws.onMessage((m) => sent.push(String(m)));
    ws.send(JSON.stringify({ stage: "joined", session: { host: "192.168.1.100", playing: false, passcode: true } }));
    setTimeout(
      () => ws.send(JSON.stringify({ stage: "joined", session: { host: "192.168.1.100", playing: true, passcode: false } })),
      1500,
    );
  });
  await page.goto(`/play/pad?url=${encodeURIComponent(target)}&name=Office%20PC`, { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("pad-stage")).toHaveText("Waiting for the passcode on Office PC", { timeout: 30_000 });
  // No box on the phone, and nothing it sends carries a passcode.
  await expect(page.getByTestId("rp-passcode")).toHaveCount(0);
  await expect(page.getByTestId("wake-passcode")).toHaveCount(0);
  await expect(page.getByTestId("pad-stage")).toHaveText("Controller for 192.168.1.100");
  expect(sent.join("\n")).not.toContain("passcode");
  sent = [];
});
