import { expect, test, type Route } from "@playwright/test";

// A game kept as a folder used to offer "Copy it with Upload", which only opened the Upload
// screen. Send to PS5 now queues the copy and starts it from the Collection, and the details
// say how it is going. The engine here is a fake that answers the same routes.

const HOST = "192.168.0.5";
const FOLDER = "/games/app/PPSA11386-app";

test("Send to PS5 queues a game folder from the Collection and shows its progress", async ({
  page,
}) => {
  await page.addInitScript((host) => {
    window.localStorage.setItem("ps5upload.host", host);
  }, HOST);
  const json = (route: Route, body: unknown, status = 200) =>
    route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });

  const started: { path?: string; dest?: string } = {};
  const game = {
    game_id: "PPSA11386",
    title: "007 First Light",
    platform: "PS5",
    sources: ["/games"],
    locations: [
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
    ],
    total_size_bytes: 80_900_000_000,
    copies: 1,
    is_duplicate: false,
    added_ts: 0,
  };

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

  await page.goto("/collection", { waitUntil: "domcontentloaded" });
  await page.getByTestId("collection-card").first().click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("Not on this PS5")).toBeVisible({ timeout: 30_000 });
  // The old button only navigated away; the new one opens the send panel in place.
  await dialog.getByRole("button", { name: "Send to PS5" }).first().click();
  await expect(dialog.getByText(/To \/data\/homebrew\/PPSA11386-app/)).toBeVisible();
  // Folder as is: the default for a game folder.
  await expect(dialog.getByRole("radio", { name: "As the folder" })).toBeChecked();
  await dialog.getByRole("button", { name: "Send", exact: true }).click();
  // Queued and started from here; the details follow it.
  await expect.poll(() => started.path, { timeout: 20_000 }).toBe(FOLDER);
  await expect(dialog.getByText(/Sending|Waiting in the queue/).first()).toBeVisible({ timeout: 20_000 });
  await expect(page).toHaveURL(/\/collection/);
});
