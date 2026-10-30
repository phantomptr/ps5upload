import { expect, test, type Page } from "@playwright/test";

// The Game panel over the stream (9.6): the running game, its cheats, a save snapshot, the
// console's temperatures and a fan boost — read only while the panel is open.

interface Calls {
  status: number;
  toggles: unknown[];
  snapshots: unknown[];
  boosts: unknown[];
}

async function streaming(page: Page, calls: Calls) {
  let boostOn = false;
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
            wake_rp_key: "00112233445566778899aabbccddeeff",
          },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.6.0", caps: {} };
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/cheats/status") {
        calls.status++;
        body = {
          enabled: false,
          patches_last: 0,
          patches_total: 0,
          game_running: true,
          game_title_id: "PPSA01234",
          game_pid: 77,
          held: false,
        };
      }
      if (path === "/api/ps5/cheats/get")
        body = { mods: [{ index: 3, name: "Infinite health", desc: "Never die", type: "", on: false }] };
      if (path === "/api/ps5/cheats/toggle") {
        calls.toggles.push(req.postDataJSON());
        body = { ok: true };
      }
      if (path === "/api/ps5/apps/installed")
        body = { titles: [{ title_id: "PPSA01234", title_name: "Astro Test", origin: "pkg" }] };
      if (path === "/api/ps5/hw/temps")
        body = { cpu_temp: 61, soc_temp: 58, m2_temp: 0, cpu_freq_mhz: 800, soc_clock_mhz: 0, soc_power_mw: 0, cpu_usage_pct: -1, fan_duty_pct: -1, product_shape: -1, fan_pinned_c: 0 };
      if (path === "/api/ps5/fan-boost") {
        if (req.method() === "POST") {
          calls.boosts.push(req.postDataJSON());
          boostOn = (req.postDataJSON() as { on: boolean }).on;
        }
        body = boostOn ? { on: true, until_ms: Date.now() + 30 * 60_000, boost_c: 55 } : { on: false, boost_c: 55 };
      }
      if (path === "/api/save-backup/snapshot") {
        calls.snapshots.push(req.postDataJSON());
        body = { ok: true, files: ["PPSA01234_user10000000_2026-10-30_120000_snap.zip"] };
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      if ((JSON.parse(String(m)) as { host?: string }).host) ws.send(JSON.stringify({ stage: "first_frame" }));
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
}

test("the Game panel shows the running game, switches its cheats and snapshots its save", async ({ page }) => {
  const calls: Calls = { status: 0, toggles: [], snapshots: [], boosts: [] };
  await streaming(page, calls);
  // Nothing is read while the panel is closed.
  await page.waitForTimeout(500);
  expect(calls.status).toBe(0);

  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-game").click();
  const panel = page.getByTestId("rp-game-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByTestId("rp-game-now")).toContainText("Astro Test");
  await expect(panel.getByTestId("rp-game-now")).toContainText("PPSA01234");

  // Snapshot: the running game's save, through the engine.
  await panel.getByTestId("rp-game-snapshot").click();
  await expect(panel.getByTestId("rp-game-snapshot-result")).toContainText("Save backed up");
  expect(calls.snapshots).toEqual([{ addr: "192.168.1.100", title_id: "PPSA01234" }]);

  // Cheats: the running game's, switched from the picture.
  await panel.getByTestId("rp-game-tab-cheats").click();
  const cheat = panel.getByRole("switch", { name: /Infinite health/ });
  await expect(cheat).toBeVisible();
  await cheat.click();
  await expect.poll(() => calls.toggles).toEqual([
    expect.objectContaining({ title_id: "PPSA01234", index: 3, on: true }),
  ]);
  await expect(cheat).toHaveAttribute("aria-checked", "true");
  // Switching one on turns the engine on (the helper does it): the panel says so at once.
  await expect(panel.getByTestId("rp-game-cheats")).toContainText("Cheat engine is on");

  // Esc closes the panel; reading stops with it.
  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);
  const after = calls.status;
  await page.waitForTimeout(3500);
  expect(calls.status).toBe(after);
});

test("the Console tab shows temperatures and turns a fan boost on and off", async ({ page }) => {
  const calls: Calls = { status: 0, toggles: [], snapshots: [], boosts: [] };
  await streaming(page, calls);
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-game").click();
  const panel = page.getByTestId("rp-game-panel");
  await panel.getByTestId("rp-game-tab-console").click();
  await expect(panel.getByTestId("rp-console-cpu")).toContainText("61 °C", { timeout: 15_000 });
  await expect(panel.getByTestId("rp-console-fan-target")).toContainText("the console's own setting");

  const boost = panel.getByRole("switch", { name: /Fan boost/ });
  await boost.click();
  await expect(boost).toHaveAttribute("aria-checked", "true");
  await expect(panel.getByTestId("rp-game-console")).toContainText("Keeping it at 55 °C");
  await boost.click();
  await expect(boost).toHaveAttribute("aria-checked", "false");
  expect(calls.boosts).toEqual([
    { addr: "192.168.1.100", on: true },
    { addr: "192.168.1.100", on: false },
  ]);
});

test.describe("on a phone's page", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test("the Game panel opens from More, under the picture", async ({ page }) => {
    const calls: Calls = { status: 0, toggles: [], snapshots: [], boosts: [] };
    await streaming(page, calls);
    await page.getByTestId("rp-more").tap();
    await page.getByTestId("rp-menu-game").tap();
    const panel = page.getByTestId("rp-game-panel");
    await expect(panel).toBeVisible();
    await expect(panel.getByTestId("rp-game-now")).toContainText("Astro Test");
    // Under the picture, not over it: the picture stays in view.
    const stage = await page.getByTestId("rp-stage").filter({ visible: true }).boundingBox();
    const box = await panel.boundingBox();
    expect(box!.y).toBeGreaterThanOrEqual(stage!.y + stage!.height - 1);
    await panel.getByTestId("rp-game-close").tap();
    await expect(panel).toHaveCount(0);
  });
});
