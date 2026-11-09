import { expect, test, type Page } from "@playwright/test";

// Settings in two tabs: App (this device, every console) and This PS5 (one console's own settings,
// with a picker). Each console keeps its own value, and a deep link opens a given console.

const A = { id: "console-a", name: "Living room", host: "192.168.0.5", settings: { rest_after_upload: true } };
const B = { id: "console-b", name: "Office", host: "192.168.0.6" };

async function setUp(page: Page) {
  await page.addInitScript(
    ([a, b]) => {
      if (window.localStorage.getItem("ps5upload.roster.v1")) return;
      window.localStorage.setItem("ps5upload.host", a.host);
      window.localStorage.setItem("ps5upload.roster.v1", JSON.stringify({ profiles: [a, b], active_id: a.id }));
    },
    [A, B] as const,
  );
  // No engine here: every engine route answers with nothing.
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 200, contentType: "application/json", body: "{}" }),
  );
}

const main = (page: Page) => page.locator("[data-scroll-root]:visible");
const restSwitch = (page: Page) =>
  main(page).getByRole("switch", { name: /Put the PS5 in rest mode after uploads finish/ });

for (const width of [1280, 412]) {
  test(`App and This PS5 tabs, each console its own (${width}px)`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await setUp(page);
    await page.goto("/settings", { waitUntil: "domcontentloaded" });

    const tabs = main(page).getByRole("tablist", { name: "Settings" });
    await expect(tabs.getByRole("tab", { name: "App" })).toHaveAttribute("aria-selected", "true", { timeout: 30_000 });
    await expect(main(page).getByTestId("settings-app")).toBeVisible();
    await expect(restSwitch(page)).toHaveCount(0);

    await tabs.getByRole("tab", { name: "This PS5" }).click();
    await expect(page).toHaveURL(/tab=ps5/);
    const picker = main(page).getByTestId("settings-ps5-picker");
    await expect(picker).toHaveValue(A.id);
    await expect(restSwitch(page)).toHaveAttribute("aria-checked", "true");

    // Office has no value of its own: the default (off). Turning it on leaves Living room alone.
    await picker.selectOption(B.id);
    await expect(page).toHaveURL(new RegExp(`console=${B.id}`));
    await expect(restSwitch(page)).toHaveAttribute("aria-checked", "false");
    await restSwitch(page).click();
    await expect(restSwitch(page)).toHaveAttribute("aria-checked", "true");
    await picker.selectOption(A.id);
    await expect(restSwitch(page)).toHaveAttribute("aria-checked", "true");
    await restSwitch(page).click();
    await expect(restSwitch(page)).toHaveAttribute("aria-checked", "false");
    await picker.selectOption(B.id);
    await expect(restSwitch(page)).toHaveAttribute("aria-checked", "true");

    // Kept with each console's profile.
    const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.roster.v1") ?? "{}"));
    const byId = Object.fromEntries(
      (saved.profiles as { id: string; settings?: { rest_after_upload?: boolean } }[]).map((p) => [p.id, p.settings]),
    );
    expect(byId[A.id]?.rest_after_upload).toBe(false);
    expect(byId[B.id]?.rest_after_upload).toBe(true);

    // No sideways scroll on a phone.
    const overflow = await main(page).evaluate((el) => el.scrollWidth - el.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  });
}

test("deep links: ?tab=ps5&console= opens that console, #tailscale opens App", async ({ page }) => {
  await setUp(page);
  await page.goto(`/settings?tab=ps5&console=${B.id}`, { waitUntil: "domcontentloaded" });
  await expect(main(page).getByTestId("settings-ps5-picker")).toHaveValue(B.id, { timeout: 30_000 });

  await page.goto("/settings#tailscale", { waitUntil: "domcontentloaded" });
  await expect(main(page).getByTestId("settings-app")).toBeVisible({ timeout: 30_000 });
  await expect(main(page).getByText("Tailscale on this device")).toBeVisible();
});

test("Connection links to This PS5 for the selected console", async ({ page }) => {
  await setUp(page);
  await page.goto("/connection", { waitUntil: "domcontentloaded" });
  // Consoles: one card per PS5, each with its own link.
  await main(page)
    .locator(`[data-console="${A.id}"]`)
    .getByRole("button", { name: "Settings for this PS5" })
    .click({ timeout: 30_000 });
  await expect(page).toHaveURL(new RegExp(`/settings\\?tab=ps5&console=${A.id}`));
  await expect(main(page).getByTestId("settings-ps5-picker")).toHaveValue(A.id);
});
