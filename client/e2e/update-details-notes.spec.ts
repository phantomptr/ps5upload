import { expect, test, type Page } from "@playwright/test";

// Discord: Details on the "Update available" notice showed nothing. An update check that came
// without notes left the dialog empty; it now shows the release's notes from GitHub. The desktop
// app is faked, and GitHub.

async function fakeApp(page: Page, notes: string) {
  await page.addInitScript((n) => {
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
  }, notes);
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 200, contentType: "application/json", body: "{}" }),
  );
}

test("Details shows the notes the update check brought", async ({ page }) => {
  await fakeApp(page, "Games and formats:\n\n- New: zstd asset packs in Convert");
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Details", exact: true }).click({ timeout: 30_000 });
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
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Details", exact: true }).click({ timeout: 30_000 });
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
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Details", exact: true }).click({ timeout: 30_000 });
  await expect(page.getByTestId("release-notes-unavailable")).toContainText("could not be read from GitHub");
});
