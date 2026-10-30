import { expect, test, type Page } from "@playwright/test";

// Phase 2 of the Remote Play companion (9.6): start a game on the stream from the Game panel,
// and drop a package on the picture to install it (by default once the stream ends).

const status = {
  command_count: 1,
  instance_id: 1,
  prior_instance: "clean",
  ps5_kernel: "r229358/releases/13.60",
  ucred_elevated: true,
  version: "9.6.0",
};

interface Seen {
  launched: string[];
  uploads: number;
  stops: number;
}

async function streaming(page: Page, seen: Seen) {
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
            wake_rp_key: "00000000000000000000000000000001",
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
      if (path === "/api/ps5/status") body = status;
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === "/api/ps5/cheats/status")
        body = {
          enabled: false,
          game_running: seen.launched.length > 0,
          game_title_id: seen.launched[0] ?? "",
          game_pid: 0,
          patches_last: 0,
          patches_total: 0,
        };
      if (path === "/api/ps5/cheats/get") body = { mods: [] };
      if (path === "/api/ps5/apps/installed")
        body = {
          titles: [
            { title_id: "PPSA01234", title_name: "Astro Bot", origin: "pkg" },
            { title_id: "CUSA00001", title_name: "Bloodborne", origin: "pkg" },
            { title_id: "NPXS40000", title_name: "Settings", origin: "pkg", system: true },
          ],
        };
      if (path === "/api/ps5/process/list")
        body = {
          truncated: false,
          processes: seen.launched.map((t, i) => ({
            pid: 70 + i,
            name: "eboot",
            comm: "eboot.bin",
            title_id: t,
            app_id: 5 + i,
            memory_mib: 1,
            threads: 1,
            kind: "app",
          })),
        };
      if (path === "/api/ps5/app/launch") {
        seen.launched.push((req.postDataJSON() as { title_id: string }).title_id);
        body = { ok: true };
      }
      if (path === "/api/pkg/upload") {
        seen.uploads++;
        // Refused here: the test checks when the upload is asked for, not the install itself.
        return route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: "test" }) });
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string; stop?: boolean };
      if (msg.host) ws.send(JSON.stringify({ stage: "first_frame" }));
      if (msg.stop) {
        seen.stops++;
        ws.send(JSON.stringify({ stage: "ended" }));
        void ws.close({ code: 1000, reason: "ended" });
      }
    });
  });
  await page.goto("/play", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Play", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByTestId("rp-start").click();
  await expect(page.getByTestId("rp-toolbar")).toBeVisible();
}

/** Drop `name` (a file the browser holds) on the picture, as a drag from the desktop does. */
async function dropFile(page: Page, name: string) {
  await page.getByTestId("rp-stage").filter({ visible: true }).evaluate((stage, fileName) => {
    const dt = new DataTransfer();
    dt.items.add(new File([new Uint8Array(16)], fileName, { type: "application/octet-stream" }));
    stage.dispatchEvent(new DragEvent("dragover", { dataTransfer: dt, bubbles: true, cancelable: true }));
    stage.dispatchEvent(new DragEvent("drop", { dataTransfer: dt, bubbles: true, cancelable: true }));
  }, name);
}

test("Play something else starts a game on the stream", async ({ page }) => {
  const seen: Seen = { launched: [], uploads: 0, stops: 0 };
  await streaming(page, seen);
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-game").click();
  const panel = page.getByTestId("rp-game-panel");
  const picker = panel.getByTestId("rp-game-play-other");
  await expect(picker).toBeVisible();
  // The console's games, not its system apps.
  await expect(picker.getByTestId("rp-game-play-title")).toHaveText(["Astro Bot", "Bloodborne"]);
  await picker.getByTestId("rp-game-search").fill("blood");
  await expect(picker.getByTestId("rp-game-play-title")).toHaveText(["Bloodborne"]);
  await picker.getByTestId("rp-game-play-title").click();
  // Its progress shows on the picture, then it is the running game.
  await expect.poll(() => seen.launched).toEqual(["CUSA00001"]);
  await expect(page.getByTestId("rp-launch-status")).toHaveCount(0, { timeout: 15_000 });
  await expect(panel.getByTestId("rp-game-now")).toContainText("Bloodborne", { timeout: 10_000 });
});

test("a package dropped on the stream installs when the stream ends, or now", async ({ page }) => {
  const seen: Seen = { launched: [], uploads: 0, stops: 0 };
  await streaming(page, seen);

  // Not a package: nothing is asked.
  await dropFile(page, "notes.txt");
  await expect(page.getByTestId("rp-drop-choice")).toHaveCount(0);

  // A package: asked, and "when I stop playing" holds it.
  await dropFile(page, "game.pkg");
  const choice = page.getByTestId("rp-drop-choice");
  await expect(choice).toContainText("Install on Living room?");
  await expect(choice).toContainText("game.pkg");
  await choice.getByTestId("rp-drop-later").click();
  await expect(choice).toHaveCount(0);
  await page.waitForTimeout(500);
  expect(seen.uploads).toBe(0);

  // "Install now" goes at once.
  await dropFile(page, "update.pkg");
  await page.getByTestId("rp-drop-now").click();
  await expect.poll(() => seen.uploads).toBe(1);

  // The stream ends: the held one goes.
  await page.getByTestId("rp-toolbar").hover();
  await page.getByTestId("rp-toolbar").getByRole("button", { name: "Stop" }).click();
  await expect.poll(() => seen.stops).toBe(1);
  await expect.poll(() => seen.uploads).toBe(2);
});

test("Play on Remote Play on a game's page starts the stream, then the game", async ({ page }) => {
  const ID = "PPSA01234";
  const launched: string[] = [];
  const starts: unknown[] = [];
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
            wake_rp_key: "00000000000000000000000000000001",
          },
        ],
      }),
    );
  });
  const now = Math.floor(Date.now() / 1000);
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.6.0", caps: {} };
      if (path === "/api/ps5/status") body = status;
      if (path === "/api/ps5/rp/session") body = { host: null };
      if (path === `/api/games/${ID}`)
        body = {
          title_id: ID,
          title: "Astro Bot",
          platform: "PS5",
          cover: null,
          copies: [],
          consoles: [{ host: "192.168.1.100", read_at: now, installed: true, version: "01.000", dlc_missing: [] }],
        };
      if (path === "/api/ps5/apps/installed")
        body = { titles: [{ title_id: ID, title_name: "Astro Bot", origin: "pkg" }] };
      if (path === "/api/ps5/process/list")
        body = {
          truncated: false,
          processes: launched.map((t) => ({ pid: 77, name: "eboot", comm: "eboot.bin", title_id: t, app_id: 5, memory_mib: 1, threads: 1, kind: "app" })),
        };
      if (path === "/api/ps5/app/launch") {
        launched.push((req.postDataJSON() as { title_id: string }).title_id);
        body = { ok: true };
      }
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    },
  );
  await page.routeWebSocket(/\/api\/ps5\/rp\/ws/, (ws) => {
    ws.onMessage((m) => {
      const msg = JSON.parse(String(m)) as { host?: string };
      if (msg.host) {
        starts.push(msg);
        ws.send(JSON.stringify({ stage: "first_frame" }));
      }
    });
  });
  await page.goto(`/games/${ID}`, { waitUntil: "domcontentloaded" });
  const playStream = page.getByTestId("game-play-stream");
  await expect(playStream).toBeVisible({ timeout: 30_000 });
  await playStream.click();
  await expect(page).toHaveURL(/\/play$/);
  // The stream starts by itself, then the game on it.
  await expect.poll(() => starts.length, { timeout: 15_000 }).toBe(1);
  await expect.poll(() => launched, { timeout: 15_000 }).toEqual([ID]);
  await expect(page.getByTestId("rp-launch-status")).toHaveCount(0, { timeout: 15_000 });
});
