import { expect, test } from "@playwright/test";

// The Send payload IP box used to switch the selected console on every keystroke: each half-typed
// address became the "current console", the box snapped back, and the other consoles' kept screens
// were pushed out. Typing a different address there now only sets where this send goes.

const A = { id: "console-a", name: "Living room", host: "192.168.0.5" };
const B = { id: "console-b", name: "Office", host: "192.168.0.6" };

test("typing an address in Send payload does not switch consoles", async ({ page }) => {
  // Payloads is a desktop-only screen: stand in for the Tauri runtime, every native call
  // answering with nothing, which is all this screen needs to render.
  await page.addInitScript(() => {
    const w = window as unknown as Record<string, unknown>;
    let id = 0;
    w.__TAURI_INTERNALS__ = {
      invoke: async () => null,
      transformCallback: () => ++id,
      unregisterCallback: () => {},
      convertFileSrc: (p: string) => p,
      metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
      plugins: {},
    };
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  });
  await page.addInitScript(
    ([a, b]) => {
      if (window.localStorage.getItem("ps5upload.roster.v1")) return;
      window.localStorage.setItem("ps5upload.host", a.host);
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({ profiles: [a, b], active_id: a.id }),
      );
    },
    [A, B],
  );
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 200, contentType: "application/json", body: "{}" }),
  );

  await page.goto("/payloads?tab=send", { waitUntil: "domcontentloaded" });
  const tabA = page.locator(`[data-console-id="${A.id}"]:visible`);
  await expect(tabA).toHaveAttribute("aria-current", "page", { timeout: 30_000 });

  const box = page.locator("[data-scroll-root]:visible").getByLabel("PS5 IP address");
  await expect(box).toHaveValue(A.host);
  await box.fill("");
  await box.pressSequentially("10.0.0.77", { delay: 20 });

  // Every keystroke kept, the box still focused, and the selected console unchanged.
  await expect(box).toHaveValue("10.0.0.77");
  await expect(box).toBeFocused();
  await expect(tabA).toHaveAttribute("aria-current", "page");
  expect(await page.evaluate(() => window.localStorage.getItem("ps5upload.host"))).toBe(A.host);
  await expect(page.locator("[data-scroll-root]:visible").getByText("10.0.0.77:9021")).toBeVisible();
});

test("typing an address in First run keeps every keystroke and the console", async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as Record<string, unknown>;
    let id = 0;
    w.__TAURI_INTERNALS__ = {
      invoke: async () => null,
      transformCallback: () => ++id,
      unregisterCallback: () => {},
      convertFileSrc: (p: string) => p,
      metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
      plugins: {},
    };
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  });
  await page.addInitScript(
    ([a, b]) => {
      if (window.localStorage.getItem("ps5upload.roster.v1")) return;
      window.localStorage.setItem("ps5upload.host", a.host);
      window.localStorage.setItem(
        "ps5upload.roster.v1",
        JSON.stringify({ profiles: [a, b], active_id: a.id }),
      );
    },
    [A, B],
  );
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => route.fulfill({ status: 200, contentType: "application/json", body: "{}" }),
  );

  await page.goto("/first-run", { waitUntil: "domcontentloaded" });
  const tabA = page.locator(`[data-console-id="${A.id}"]:visible`);
  await expect(tabA).toHaveAttribute("aria-current", "page", { timeout: 30_000 });
  const box = page.locator("[data-scroll-root]:visible").getByPlaceholder("192.168.1.50");
  await box.fill("");
  await box.pressSequentially("10.0.0.77", { delay: 20 });
  await expect(box).toHaveValue("10.0.0.77");
  await expect(box).toBeFocused();
  await expect(tabA).toHaveAttribute("aria-current", "page");
});
