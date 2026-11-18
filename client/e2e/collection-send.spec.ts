import { expect, test, type Page, type Route } from "@playwright/test";

// A game kept as a folder used to offer "Copy it with Upload", which only opened the Upload
// screen. Send to PS5 now queues the copy and starts it from the game's page (opened from the
// Collection), and the console's row says how it is going. The engine here is a fake that
// answers the same routes.

const HOST = "192.168.0.5";
const FOLDER = "/games/app/PPSA11386-app";

type Started = { path?: string; dest?: string };

/** The fake engine for one game with `locations`; `kraken`: Kraken Mount runs on the console. */
async function fakeEngine(
  page: Page,
  locations: Record<string, unknown>[],
  kraken = false,
): Promise<{ started: Started; game: { game_id: string } }> {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const started: Started = {};
  const game = {
    game_id: "PPSA11386",
    title: "007 First Light",
    platform: "PS5",
    sources: ["/games"],
    locations,
    total_size_bytes: 80_900_000_000,
    copies: 1,
    is_duplicate: false,
    added_ts: 0,
  };

  const json = (route: Route, body: unknown, status = 200) =>
    route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });

  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      const p = url.pathname;
      if (p === "/api/version") return json(route, { caps: { rar: true }, version: "6.5.0" });
      if (p === "/api/ps5/status")
        return json(route, {
          command_count: 1,
          instance_id: 1,
          prior_instance: "clean",
          ps5_kernel: "r229358/releases/13.60",
          ucred_elevated: true,
          version: "6.5.0",
        });
      if (p === "/api/ps5/volumes")
        return json(route, {
          volumes: [
            {
              path: "/data",
              fs_type: "nullfs",
              total_bytes: 900_000_000_000,
              free_bytes: 400_000_000_000,
              allocatable_bytes: 398_000_000_000,
              writable: true,
              is_placeholder: false,
            },
          ],
        });
      if (p === "/api/collection/settings")
        return json(route, { roots: ["/games"], refresh_secs: null, sweep_sidecars: false, trash_available: true });
      if (p === "/api/collection/scan")
        return json(route, { running: false, deep: false, found: 1, done: 1, started_ms: 0, finished_ms: 1, error: null, games: 1, locations: 1 });
      if (p === "/api/collection/library")
        return json(route, {
          roots: ["/games"],
          generated_at: "2026-10-08T00:00:00Z",
          summary: { total_games: 1, total_locations: 1, total_size_bytes: game.total_size_bytes, duplicates_count: 0, reclaimable_bytes: 0 },
          games: { [game.game_id]: game },
        });
      if (p === "/api/collection/console")
        return json(route, {
          games: [{ game_id: game.game_id, installed: false, dlc_missing: [], non_package_copy: true }],
        });
      if (p === `/api/games/${game.game_id}`)
        return json(route, {
          title_id: game.game_id,
          title: game.title,
          platform: "PS5",
          cover: null,
          copies: game.locations,
          consoles: [{ host: HOST, read_at: Math.floor(Date.now() / 1000), installed: false, dlc_missing: [] }],
        });
      if (p === `/api/games/${game.game_id}/refresh`)
        return json(route, { host: HOST, read_at: Math.floor(Date.now() / 1000), installed: false, dlc_missing: [] });
      if (p === "/api/ps5/smp/status")
        return json(route, {
          installed: false,
          running: false,
          mounted_images: [],
          errors: [],
          mounter: kraken ? "kraken" : "none",
          kraken: kraken
            ? {
                installed: true,
                running: true,
                api: true,
                version: "0.2.1",
                waiting: null,
                games: [],
                title_ids: [],
                ampr: null,
                settings: { inject_ampr: false, ampr_auto_update: false, scan_paths: [] },
                scan_roots: ["/data/homebrew"],
                errors: [],
              }
            : undefined,
        });
      if (p === "/api/transfer/file") {
        const body = req.postDataJSON() as { src?: string; dest?: string };
        started.path = body.src;
        started.dest = body.dest;
        return json(route, { job_id: "j1" });
      }
      if (p === "/api/transfer/dir" || p === "/api/transfer/dir-reconcile") {
        const body = req.postDataJSON() as { src_dir?: string; dest_root?: string; src?: string; dest?: string };
        started.path = body.src_dir ?? body.src;
        started.dest = body.dest_root ?? body.dest;
        return json(route, { job_id: "j1" });
      }
      if (p === "/api/pkg/links") return json(route, []);
      if (p.startsWith("/api/jobs"))
        return json(route, { status: "running", bytes_sent: 20_000_000_000, total_bytes: 80_900_000_000 });
      if (p === "/api/ps5/list-dir")
        return json(route, {
          path: url.searchParams.get("path") ?? "/data",
          entries: [],
          truncated: false,
          total_scanned: 0,
          returned: 0,
        });
      if (p === "/api/ps5/process/list") return json(route, { processes: [] });
      if (p === "/api/engine-logs") return json(route, { lines: [] });
      if (p === "/api/ps5/readiness") return json(route, { ready: true, detail: "" });
      return json(route, {});
    },
  );

  return { started, game };
}

test("Send to PS5 queues a game folder from the Collection and shows its progress", async ({
  page,
}) => {
  const { started } = await fakeEngine(page, [
    {
      root: "/games",
      container: "app",
      name: "PPSA11386-app",
      type: "folder",
      path: "app/PPSA11386-app",
      absolute_path: FOLDER,
      size_bytes: 80_900_000_000,
      added_ts: 0,
      pkg: { kind: "base", version: "1.18.0", region: "Europe", content_id: "EP3969-PPSA11386_00-007FIRSTLIGHT000" },
    },
  ]);
  await page.goto("/games?tab=all", { waitUntil: "domcontentloaded" });
  await page.getByTestId("collection-card").first().click();
  await expect(page).toHaveURL(/\/games\/PPSA11386$/);
  const row = page.getByTestId(`game-console-${HOST}`);
  await expect(row).toContainText("Not installed", { timeout: 30_000 });
  // The old button only navigated away; this one opens the send panel in place.
  await row.getByRole("button", { name: "Send to PS5" }).click();
  await expect(row.getByText(/To \/data\/homebrew\/PPSA11386-app/)).toBeVisible();
  // Folder as is: the default for a game folder.
  await expect(row.getByRole("radio", { name: "As the folder" })).toBeChecked();
  await row.getByRole("button", { name: "Send", exact: true }).click();
  // Queued and started from here; the console's row follows it.
  await expect.poll(() => started.path, { timeout: 20_000 }).toBe(FOLDER);
  await expect(row.getByText(/Sending|Waiting in the queue/).first()).toBeVisible({ timeout: 20_000 });
});

// Kraken Mount mounts a debug package from a folder it scans, so a package copy can go to the PS5
// as a plain file there instead of being installed.
const PKG = "/games/Love is all around 2 - PPSA11386 - v1.0.3 - AS - BASE.pkg";
const pkgCopy = {
  root: "/games",
  container: "",
  name: "Love is all around 2 - PPSA11386 - v1.0.3 - AS - BASE.pkg",
  type: "pkg",
  path: "Love is all around 2 - PPSA11386 - v1.0.3 - AS - BASE.pkg",
  absolute_path: PKG,
  size_bytes: 12_000_000_000,
  added_ts: 0,
  pkg: { kind: "base", platform: "PS5", version: "1.03", min_fw: "10.00", content_id: "EP3969-PPSA11386_00-LOVE000000000000" },
};

test("a package sends as a plain file to Kraken Mount's folder when it runs", async ({ page }) => {
  const { started } = await fakeEngine(page, [pkgCopy], true);
  await page.goto("/games/PPSA11386", { waitUntil: "domcontentloaded" });
  const copy = page.getByTestId("game-copy").first();
  const send = copy.getByRole("button", { name: "Send to PS5" });
  await expect(send).toBeVisible({ timeout: 30_000 });
  await expect(send).toHaveAttribute("title", "Kraken Mount mounts it from /data/homebrew");
  // On a phone-width window the button stays inside its row.
  await page.setViewportSize({ width: 360, height: 800 });
  const [cb, bb] = [await copy.boundingBox(), await send.boundingBox()];
  expect(bb!.x + bb!.width).toBeLessThanOrEqual(cb!.x + cb!.width);
  await page.setViewportSize({ width: 1280, height: 800 });
  // Install is still one click away, in the menu.
  await copy.getByRole("button", { name: "More actions for this copy" }).click();
  await expect(page.getByRole("menuitem", { name: "Install", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await send.click();
  const row = page.getByTestId(`game-console-${HOST}`);
  await expect(row.getByText(`To /data/homebrew/${pkgCopy.name}`)).toBeVisible();
  await expect(row.getByText(/Kraken Mount mounts it from this folder/)).toBeVisible();
  await row.getByRole("button", { name: "Send", exact: true }).click();
  await expect.poll(() => started.path, { timeout: 20_000 }).toBe(PKG);
  expect(started.dest).toBe(`/data/homebrew/${pkgCopy.name}`);
});

test("a package installs first without Kraken Mount, and can still go as a file", async ({ page }) => {
  await fakeEngine(page, [pkgCopy], false);
  await page.goto("/games/PPSA11386", { waitUntil: "domcontentloaded" });
  const copy = page.getByTestId("game-copy").first();
  await expect(copy.getByRole("button", { name: "Install" })).toBeVisible({ timeout: 30_000 });
  await expect(copy.getByRole("button", { name: "Send to PS5" })).toHaveCount(0);
  await copy.getByRole("button", { name: "More actions for this copy" }).click();
  await page.getByRole("menuitem", { name: "Send to PS5 as a file" }).click();
  const row = page.getByTestId(`game-console-${HOST}`);
  await expect(row.getByText(`To /data/homebrew/${pkgCopy.name}`)).toBeVisible();
  await expect(row.getByText("Goes as a plain file, nothing is installed.")).toBeVisible();
});
