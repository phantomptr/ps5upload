import { expect, test, type Page } from "@playwright/test";

// Discord: Details on the "Update available" notice showed nothing. An update check that came
// without notes left the dialog empty; it now shows the release's notes from GitHub. The desktop
// app is faked, and GitHub.
//
// Since 12.6.15 a routine update is quiet (Reddit: "a little notification dot on settings rather
// than a massive banner"): a dot on Settings, and the "Update available" row at the top of
// Settings, which is where Details is now.

async function fakeApp(page: Page, notes: string, helper: string | null = null) {
  await page.addInitScript(([n, h]) => {
    const w = window as unknown as Record<string, unknown>;
    let id = 0;
    w.__TAURI_INTERNALS__ = {
      invoke: async (cmd: string) => {
        if (cmd === "update_check")
          return {
            available: true,
            current_version: "11.3.0",
            latest_version: "11.4.0",
            notes: n,
            pub_date: "",
            download_url: "https://example.invalid/PS5Upload.zip",
            download_filename: "PS5Upload.zip",
            sha256: "",
          };
        if (cmd === "plugin:app|version") return "11.3.0";
        // The PS5's helper, when the test gives it one (another device sent it).
        if (cmd === "payload_check" && h)
          return { reachable: true, loaded: true, engine: true, status: { version: h, ps5_kernel: "", ucred_elevated: true } };
        return null;
      },
      transformCallback: () => ++id,
      unregisterCallback: () => {},
      convertFileSrc: (p: string) => p,
      metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
      plugins: {},
    };
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    // A PS5 on the list: with none, the app opens on Add a PS5, which covers the notice.
    window.localStorage.setItem("ps5upload.host", "192.0.2.10");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({ profiles: [{ id: "ps5", name: "PS5", host: "192.0.2.10" }], active_id: "ps5" }),
    );
  }, [notes, helper] as const);
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 200, contentType: "application/json", body: "{}" }),
  );
}

/** Settings, then Details on its update row. */
async function openDetails(page: Page) {
  await page.goto("/settings", { waitUntil: "domcontentloaded" });
  await page.getByTestId("settings-update-row").getByRole("button", { name: "Details", exact: true }).click({ timeout: 30_000 });
}

test("a routine update is a dot on Settings, not a banner", async ({ page }) => {
  await fakeApp(page, "Notes");
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const settings = page.locator("aside").getByRole("link", { name: /^Settings/ });
  await expect(settings.getByLabel("Update available")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("update-banner")).toHaveCount(0);
  await settings.click();
  await expect(page.getByTestId("settings-update-row")).toContainText("Update available");
  await expect(page.getByTestId("settings-update-row")).toContainText("v11.4.0");
});

test("on a phone the dot is on More and nothing covers Home", async ({ page }) => {
  await page.setViewportSize({ width: 412, height: 915 });
  await fakeApp(page, "Notes");
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const more = page.locator("[data-bottom-nav]").getByRole("link", { name: /^More/ });
  await expect(more).toHaveAttribute("data-update-dot", "1", { timeout: 30_000 });
  await expect(page.getByTestId("update-banner")).toHaveCount(0);
  await more.click();
  await page.getByRole("link", { name: /^Settings/ }).first().click();
  const row = page.getByTestId("settings-update-row");
  await expect(row).toContainText("v11.4.0");
  // The row fits the phone: no sideways scroll.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});

test("a PS5 helper newer than this app is one line across the top, merged with the update", async ({ page }) => {
  await page.setViewportSize({ width: 412, height: 915 });
  await fakeApp(page, "Notes", "11.4.0");
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const banner = page.getByTestId("update-banner");
  await expect(banner).toContainText("Update this app to v11.4.0 to match your PS5", { timeout: 30_000 });
  await expect(banner.getByRole("button", { name: "Install" })).toBeVisible();
  // One banner, one line, at most ~56 px on a phone.
  await expect(banner).toHaveCount(1);
  const box = await banner.boundingBox();
  expect(box?.height ?? 0).toBeLessThanOrEqual(56);
  // Dismissed for this version pair: gone, and gone after a reload too.
  await banner.getByRole("button", { name: "Dismiss" }).click();
  await expect(banner).toHaveCount(0);
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator("[data-bottom-nav]").getByRole("link", { name: /^More/ })).toHaveAttribute(
    "data-update-dot",
    "1",
    { timeout: 30_000 },
  );
  await expect(page.getByTestId("update-banner")).toHaveCount(0);
});

test("Details shows the notes the update check brought", async ({ page }) => {
  await fakeApp(page, "Games and formats:\n\n- New: zstd asset packs in Convert");
  await openDetails(page);
  await expect(page.getByRole("dialog").getByText("New: zstd asset packs in Convert")).toBeVisible();
});

test("Details without notes in the check reads them from the release on GitHub", async ({ page }) => {
  await fakeApp(page, "");
  const asked: string[] = [];
  await page.route("https://api.github.com/**", (route) => {
    asked.push(route.request().url());
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ tag_name: "v11.4.0", body: "Servers:\r\n\r\n- New: NFS exports (NFSv3)" }),
    });
  });
  await openDetails(page);
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("New: NFS exports (NFSv3)")).toBeVisible();
  // React runs a dev build's effects twice: the same one request, maybe made twice.
  expect([...new Set(asked)]).toEqual(["https://api.github.com/repos/phantomptr/ps5upload/releases/tags/v11.4.0"]);
});

test("Details says so when GitHub cannot be read", async ({ page }) => {
  await fakeApp(page, "");
  await page.route("https://api.github.com/**", (route) =>
    route.fulfill({ status: 403, contentType: "application/json", body: "{}" }),
  );
  await openDetails(page);
  await expect(page.getByTestId("release-notes-unavailable")).toContainText("could not be read from GitHub");
});
