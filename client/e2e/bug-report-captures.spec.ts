import { expect, test } from "@playwright/test";

// The desktop app's Capture button (status bar, bottom right) saves screenshots for bug reports.
// The Bug report page says where that button is, lists every capture to pick from, and a capture
// taken while a report is being written is added to it.

const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
const shot = (n: number) => ({
  name: `screenshot-2026100${n}-120000-000.png`,
  path: `/shots/screenshot-2026100${n}-120000-000.png`,
  bytes: 70,
  data_url: `data:image/png;base64,${PNG}`,
});

test("captures are listed to pick from, and a new capture joins the report", async ({ page }) => {
  await page.addInitScript(
    ([a, b, saved]) => {
      const w = window as unknown as Record<string, unknown>;
      let id = 0;
      const list = [a, b];
      w.__TAURI_INTERNALS__ = {
        invoke: async (cmd: string) => {
          if (cmd === "screenshot_list") return list;
          if (cmd === "screenshot_save") {
            list.unshift(saved);
            return saved;
          }
          return null;
        },
        transformCallback: () => ++id,
        unregisterCallback: () => {},
        convertFileSrc: (p: string) => p,
        metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
        plugins: {},
      };
      w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    },
    [shot(1), shot(2), shot(3)],
  );
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 200, contentType: "application/json", body: "{}" }),
  );

  await page.goto("/bug-report", { waitUntil: "domcontentloaded" });
  const main = page.getByRole("main");
  await expect(main.getByTestId("br-capture-hint")).toContainText("bottom right", { timeout: 30_000 });

  const captures = main.getByTestId("br-captures").getByRole("button");
  await expect(captures).toHaveCount(2);
  await expect(captures.first()).toHaveAttribute("aria-pressed", "false");
  await captures.first().click();
  await expect(captures.first()).toHaveAttribute("aria-pressed", "true");

  await page.getByRole("button", { name: "Capture", exact: true }).click();
  await expect(captures).toHaveCount(3);
  await expect(main.getByRole("button", { name: /screenshot-20261003/ })).toHaveAttribute("aria-pressed", "true");
});
