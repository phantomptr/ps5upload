import { expect, test, type Page } from "@playwright/test";

// 10.0.0: Play (was Remote Play) sits under Home at the top of the app, Home shows a Play card,
// and Play › Set up is a checklist that ticks each step by itself from what the console says.

const HOST = "192.168.1.100";

interface Console {
  readiness: Record<string, unknown>;
  paired: number;
  enabled: string[];
}

/** A console part-way set up: the user signed in on an activated account, the offline sign-in
 *  done, Remote Play off, and this app not paired yet. */
async function consoleHalfSetUp(page: Page, opts: { paired?: boolean } = {}): Promise<Console> {
  const c: Console = {
    readiness: {
      fw_magic: 0x13600000,
      has_per_user: 1,
      foreground_uid: 0x10000000,
      account_uid: 0x10000000,
      account_via: "foreground",
      user_slot: 1,
      account_id_b64: "AAAAAAAAAAE=",
      account_id_raw: 1,
      account_type: "np",
      service_enabled: 0,
      user_enabled: 0,
      symbols_ok: 1,
      registry_err: 0,
    },
    paired: 0,
    enabled: [],
  };
  await page.addInitScript(
    ([host, paired]) => {
      window.localStorage.setItem("ps5upload.host", host);
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({
          active_id: "p1",
          profiles: [
            {
              id: "p1",
              name: "Living room",
              host,
              ...(paired
                ? { wake_regist_key: "00000000000000000000000000000001", wake_rp_key: "00000000000000000000000000000002" }
                : {}),
            },
          ],
        }),
      );
    },
    [HOST, !!opts.paired] as const,
  );
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "10.0.0", caps: {} };
      if (path === "/api/ps5/status") body = { ok: true };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/remoteplay/readiness") body = c.readiness;
      if (path === "/api/ps5/remoteplay/enable") {
        const scope = (req.postDataJSON() as { scope: string }).scope;
        c.enabled.push(scope);
        if (scope === "service") c.readiness = { ...c.readiness, service_enabled: 1 };
        else c.readiness = { ...c.readiness, user_enabled: 1 };
        body = c.readiness;
      }
      if (path === "/api/profile/info")
        body = {
          ok: true,
          uid: 0x10000000,
          uid_hex: "0x10000000",
          username: "Alice",
          users: [{ uid: 0x10000000, uid_hex: "0x10000000", username: "Alice" }],
          slots: [{ slot: 1, name: "Alice", type: "np", flags: 4098, id: "0x0000000000000001", activated: true, offline_activated: true }],
        };
      if (path === "/api/profile/np/state")
        body = {
          ok: true, slot: 1, uid: 0x10000000, backup: false, backup_slot: 0, online_id: "alice_ps",
          dob: "1983-04-12", auth_dat: true, config_dat: true, sub_account: 0, offline_adult: 1, age: 43, login_flag: 1,
        };
      if (path === "/api/ps5/power/pair") {
        c.paired++;
        body = {
          credential: "1234567890",
          regist_key: "00000000000000000000000000000001",
          rp_key: "00000000000000000000000000000002",
          nickname: "PS5-Living",
          mac: "",
          account_id_b64: "AAAAAAAAAAE=",
          account_via: "foreground",
        };
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  return c;
}

test("Play sits under Home at the top of the sidebar, not in Console", async ({ page }) => {
  await consoleHalfSetUp(page);
  await page.goto("/home", { waitUntil: "domcontentloaded" });
  const nav = page.getByRole("navigation").first();
  const home = nav.locator('a[href="/home"]').first();
  const play = nav.locator('a[href="/play"]');
  const bug = nav.locator('a[href="/bug-report"]').first();
  await expect(play).toHaveCount(1, { timeout: 30_000 });
  const y = async (l: typeof home) => (await l.boundingBox())!.y;
  expect(await y(home)).toBeLessThan(await y(play));
  expect(await y(play)).toBeLessThan(await y(bug));
  await expect(play).toContainText("Play");
});

test("the checklist ticks steps by itself and does the ones it can", async ({ page }) => {
  const c = await consoleHalfSetUp(page);
  await page.goto("/home", { waitUntil: "domcontentloaded" });

  // Home: not set up yet, so the Play card offers the setup with its progress.
  const card = page.getByTestId("home-play");
  await expect(card).toHaveAttribute("data-state", "setup", { timeout: 30_000 });
  await card.getByTestId("home-play-setup").click();
  await expect(page).toHaveURL(/\/play\/setup$/);

  const step = (k: string) => page.locator(`[data-step="${k}"]`);
  // Read from the console: connected, the user, the account and the offline sign-in are done.
  for (const k of ["connect", "user", "account", "signin"]) {
    await expect(step(k)).toHaveAttribute("data-state", "ok", { timeout: 15_000 });
  }
  // Remote Play is off: the open step, with the button that turns it on.
  await expect(step("service")).toHaveAttribute("data-state", "todo");
  await step("service").getByRole("button", { name: "Turn on" }).click();
  // The console also asks per user on this firmware: Allow, then it ticks.
  await step("service").getByRole("button", { name: "Allow" }).click();
  await expect(step("service")).toHaveAttribute("data-state", "ok");
  expect(c.enabled).toEqual(["service", "user"]);

  // Rest mode: only the console shows it, so the user ticks it.
  await page.getByTestId("play-setup-rest-done").click();
  await expect(step("rest")).toHaveAttribute("data-state", "ok");

  // Pair, right here.
  await page.getByTestId("play-setup-pair").click();
  await expect(step("pair")).toHaveAttribute("data-state", "ok");
  expect(c.paired).toBe(1);
  await expect(step("play")).toHaveAttribute("data-state", "todo");
  await expect(page.getByTestId("play-setup")).toContainText("7 of 8 done");

  // Home now offers Play itself (in-app navigation: a reload would rerun this test's setup).
  await page.getByRole("navigation").first().locator('a[href="/home"]').first().click();
  await expect(page.getByTestId("home-play")).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  await expect(page.getByTestId("home-play-start")).toBeVisible();
});

test("a step ticks by itself when the console changes", async ({ page }) => {
  const c = await consoleHalfSetUp(page);
  c.readiness = { ...c.readiness, account_uid: -1 };
  await page.goto("/play/setup", { waitUntil: "domcontentloaded" });
  const user = page.locator('[data-step="user"]');
  await expect(user).toHaveAttribute("data-state", "todo", { timeout: 30_000 });
  // Someone signs in on the console: the next read ticks it, no button pressed.
  c.readiness = { ...c.readiness, account_uid: 0x10000000 };
  await expect(user).toHaveAttribute("data-state", "ok", { timeout: 12_000 });
});

test("Home's Play card can be hidden, and comes back from Play › Set up", async ({ page }) => {
  await consoleHalfSetUp(page, { paired: true });
  await page.goto("/home", { waitUntil: "domcontentloaded" });
  const card = page.getByTestId("home-play");
  await expect(card).toHaveAttribute("data-state", "ready", { timeout: 30_000 });
  await card.getByTestId("home-play-hide").click();
  await expect(card).toHaveCount(0);
  // The rest of Home is still there.
  await expect(page.getByTestId("home-greeting")).toBeVisible();
  await page.goto("/play/setup", { waitUntil: "domcontentloaded" });
  await page.getByRole("switch", { name: "Show Play on Home" }).click();
  await page.goto("/home", { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId("home-play")).toBeVisible({ timeout: 30_000 });
});
