import { expect, test, type Page } from "@playwright/test";
import { routeEngine, seedConsole } from "./fixtures/engine";

// #941: after the app updates, the PS5 still runs the older helper, and nothing said so (the bar
// that did went away when the automatic update arrived, and the automatic update is off). The app
// now says it once per console and app version: a toast and a bell entry, a notice on Home and
// the console card's main button, each with Update helper. Nothing is sent until that is pressed.

const KERNEL = "FreeBSD 11.0 releng/11.0 #0 r218215/releases/09.60: Mon Aug 26 2024";

interface Fake {
  app: string;
  helper: string;
  sends: number;
}

async function fakeEngine(page: Page, fake: Fake) {
  await seedConsole(page);
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: fake.app, caps: {} };
    if (path === "/api/ps5/status") return { version: fake.helper, ps5_kernel: KERNEL, ucred_elevated: true };
    if (path === "/api/ps5/port-check") return { open: true, error: null };
    if (path === "/api/pkg/payload-restore") {
      // The engine sent this app's helper: the console answers with it from now on.
      fake.sends += 1;
      fake.helper = fake.app;
      return { ok: true };
    }
    return undefined;
  });
}

const notice = (page: Page) => page.getByTestId("helper-update-notice").locator("visible=true");
const toast = (page: Page) => page.getByRole("alert").filter({ hasText: "The PS5 helper is out of date" });

test("an older helper is said once, with Update helper on the toast, the bell, Home and the card", async ({ page }) => {
  const fake: Fake = { app: "12.6.31", helper: "12.6.30", sends: 0 };
  await fakeEngine(page, fake);
  await page.goto("/", { waitUntil: "domcontentloaded" });

  await expect(toast(page)).toBeVisible({ timeout: 30_000 });
  await expect(toast(page).getByRole("button", { name: "Update helper", exact: true })).toBeVisible();
  await expect(notice(page)).toContainText("This PS5 runs helper v12.6.30. This app is v12.6.31.");
  await expect(notice(page).getByTestId("helper-update-now")).toHaveText("Update helper");

  // The bell keeps it.
  await page.getByRole("button", { name: "Open notifications" }).locator("visible=true").click();
  await expect(page.getByRole("dialog").getByText("The PS5 helper is out of date")).toBeVisible();
  await page.keyboard.press("Escape");

  // Nothing was sent by itself.
  expect(fake.sends).toBe(0);

  // The console's card: the helper line and the one button that asks for a press.
  await page.goto("/connection", { waitUntil: "domcontentloaded" });
  const card = page.getByTestId("consoles-resend").locator("visible=true");
  await expect(card).toHaveText("Update helper", { timeout: 30_000 });
  await expect(card).toHaveAttribute("data-update", "true");
  await expect(page.getByText(`Helper v12.6.30 · update available`).locator("visible=true")).toBeVisible();

  // A restart of the app says nothing new: no second toast, one bell entry, the notice still there.
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(notice(page)).toBeVisible({ timeout: 30_000 });
  await expect(toast(page)).toHaveCount(0);
  const entries = await page.evaluate(
    () => JSON.parse(window.localStorage.getItem("ps5upload.notifications.v1") ?? "[]") as { title: string }[],
  );
  expect(entries.filter((e) => e.title === "The PS5 helper is out of date")).toHaveLength(1);
  expect(fake.sends).toBe(0);
});

test("Update helper on Home sends the helper, and the notice goes once it answers", async ({ page }) => {
  const fake: Fake = { app: "12.6.31", helper: "12.6.30", sends: 0 };
  await fakeEngine(page, fake);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(notice(page)).toBeVisible({ timeout: 30_000 });
  await notice(page).getByTestId("helper-update-now").click();
  await expect.poll(() => fake.sends, { timeout: 30_000 }).toBe(1);
  await expect(notice(page)).toHaveCount(0, { timeout: 60_000 });
  // The bell entry about it went with it.
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (JSON.parse(window.localStorage.getItem("ps5upload.notifications.v1") ?? "[]") as { title: string }[]).filter(
            (e) => e.title === "The PS5 helper is out of date",
          ).length,
      ),
    )
    .toBe(0);
});

test("a dismissed notice stays away until the next app version", async ({ page }) => {
  const fake: Fake = { app: "12.6.31", helper: "12.6.30", sends: 0 };
  await fakeEngine(page, fake);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(notice(page)).toBeVisible({ timeout: 30_000 });
  await notice(page).getByTestId("helper-update-dismiss").click();
  await expect(notice(page)).toHaveCount(0);

  await page.reload({ waitUntil: "domcontentloaded" });
  // The console answers again, and the notice is not back.
  await expect(page.getByRole("heading", { name: "Your PS5 is ready" }).locator("visible=true")).toBeVisible({
    timeout: 30_000,
  });
  await expect(notice(page)).toHaveCount(0);
  await expect(toast(page)).toHaveCount(0);

  // The app updates again: said again.
  fake.app = "12.6.32";
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(notice(page)).toContainText("This app is v12.6.32.", { timeout: 30_000 });
  await expect(toast(page)).toBeVisible();
  expect(fake.sends).toBe(0);
});

test("a console that is asleep is not nagged about its helper", async ({ page }) => {
  await seedConsole(page);
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === "/api/ps5/status")
        return route.fulfill({
          status: 502,
          contentType: "application/json",
          body: JSON.stringify({ error: "the console is in rest mode", reason: "console_rest_mode", power: "rest" }),
        });
      return route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(path === "/api/version" ? { version: "12.6.31", caps: {} } : {}),
      });
    },
  );
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.getByText("In rest mode").locator("visible=true").first()).toBeVisible({ timeout: 30_000 });
  await expect(notice(page)).toHaveCount(0);
  await expect(toast(page)).toHaveCount(0);
});
