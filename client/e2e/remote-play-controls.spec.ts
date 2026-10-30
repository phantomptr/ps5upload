import { expect, test } from "@playwright/test";

// Per-game controls (9.11): the Game panel's Controls tab sets the running game's turbo buttons,
// mouse aim and macros; they change what the console gets, and each game keeps its own.

const status = {
  command_count: 1,
  instance_id: 1,
  prior_instance: "clean",
  ps5_kernel: "r229358/releases/13.60",
  ucred_elevated: true,
  version: "9.11.0",
};
const CROSS = 1;

test("turbo repeats a held button, and a macro key plays its presses", async ({ page }) => {
  const pads: number[] = [];
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        active_id: "p1",
        profiles: [
          { id: "p1", name: "Living room", host: "192.168.1.100", wake_regist_key: "00112233", wake_rp_key: "00000000000000000000000000000001" },
        ],
      }),
    );
  });
  await page.route(
    (url) => url.pathname.startsWith("/api/"),
    async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.11.0", caps: {} };
      if (path === "/api/ps5/status") body = status;
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/process/list")
        body = {
          truncated: false,
          processes: [{ pid: 77, name: "eboot", comm: "eboot.bin", title_id: "CUSA00001", app_id: 5, memory_mib: 1, threads: 1, kind: "app" }],
        };
      if (path === "/api/ps5/apps/installed") body = { titles: [{ title_id: "CUSA00001", title_name: "Bloodborne", origin: "pkg" }] };
      if (path === "/api/ps5/cheats/status")
        body = { enabled: false, game_running: true, game_title_id: "CUSA00001", game_pid: 77, patches_last: 0, patches_total: 0 };
      if (path === "/api/ps5/cheats/get") body = { mods: [] };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string; pad?: { buttons: number } };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
      if (msg.pad) pads.push(msg.pad.buttons);
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Remote Play" })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-game").click();
  const panel = page.getByTestId("rp-game-panel");
  await panel.getByTestId("rp-game-tab-controls").click();
  // The running game's controls (known from the running-games watch).
  await expect(panel.getByTestId("rp-controls")).toContainText("Bloodborne", { timeout: 15_000 });

  // Turbo on ✕, then hold Enter (✕): the console sees ✕ pressed and let go, again and again.
  await panel.getByTestId("rp-controls-turbo").getByRole("button", { name: "✕" }).click();
  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);
  const stage = page.getByTestId("rp-stage").filter({ visible: true });
  await stage.focus();
  pads.length = 0;
  await page.keyboard.down("Enter");
  await page.waitForTimeout(600);
  await page.keyboard.up("Enter");
  const flips = pads.filter((b, i) => i > 0 && (b & CROSS) !== (pads[i - 1] & CROSS)).length;
  expect(flips).toBeGreaterThanOrEqual(6);

  // A macro: Q plays ✕ for 100 ms.
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-game").click();
  await panel.getByTestId("rp-game-tab-controls").click();
  await panel.getByTestId("rp-controls-turbo").getByRole("button", { name: "✕" }).click(); // turbo off
  await panel.getByTestId("rp-controls-macro-add").click();
  await expect(panel.getByTestId("rp-controls-macro-key")).toHaveText("Press a key…");
  await page.keyboard.press("KeyQ");
  await expect(panel.getByTestId("rp-controls-macro-key")).toHaveText("Key: Q");
  await page.keyboard.press("Escape");
  await stage.focus();
  pads.length = 0;
  await page.keyboard.press("KeyQ");
  await expect.poll(() => pads.includes(CROSS)).toBe(true);
  await expect.poll(() => pads.at(-1)).toBe(0);

  // Kept for this game, on this device.
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("ps5upload.rp_controls.v1") ?? "{}"));
  expect(saved.CUSA00001.macros[0]).toMatchObject({ key: "KeyQ", steps: [{ buttons: CROSS, ms: 100 }] });
});
