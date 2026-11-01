// #621: a game added while Workshop is converting another goes into ⑤ Queue and is made after
// it, without pressing Start ("Start the queue when you add something" is on by default).
import { expect, test } from "@playwright/test";

test("a game added while one converts is made after it", async ({ page }) => {
  await page.setViewportSize({ width: 1400, height: 1200 });
  const builds: string[] = [];
  // Each build job stays running for a few polls, so the first is still going while the
  // second game is added.
  const polls = new Map<string, number>();
  const games: Record<string, string> = { "/games/Astro": "Astro Bot", "/games/Bloodborne": "Bloodborne" };
  const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      const body = req.postDataJSON?.() as Record<string, unknown> | null;
      if (path === "/api/fpkg/inspect") {
        const source = String(body?.source ?? "");
        return route.fulfill(
          json({
            source: `folder ${source}`,
            files: 3,
            bytes: 3 << 20,
            content_id: "UP9000-PPSA01234_00-ASTROBOT00000000",
            title: games[source] ?? "Game",
            planned_size: 3 << 20,
            output_free: 1 << 40,
            checks: [{ name: "eboot.bin", ok: true, detail: "ok" }],
          }),
        );
      }
      if (path === "/api/fpkg/estimate") return route.fulfill(json({ error: "no" }, 500));
      if (path === "/api/fpkg/build") {
        builds.push(String(body?.source));
        return route.fulfill(json({ job_id: `job-${builds.length}` }));
      }
      const job = path.match(/^\/api\/jobs\/([^/]+)$/)?.[1];
      if (job) {
        const n = (polls.get(job) ?? 0) + 1;
        polls.set(job, n);
        const source = builds[Number(job.split("-")[1]) - 1];
        if (n < 8)
          return route.fulfill(
            json({
              status: "running",
              started_at_ms: Date.now(),
              bytes_sent: n,
              total_bytes: 10,
              stage: { id: "write", index: 2, count: 4, done: n, total: 10 },
            }),
          );
        return route.fulfill(
          json({
            status: "done",
            started_at_ms: 0,
            completed_at_ms: 1,
            elapsed_ms: 1,
            tx_id_hex: "PPSA01234",
            bytes_sent: 10,
            dest: `/out/${source.split("/").pop()}.pkg`,
          }),
        );
      }
      if (path === "/api/local/storage-roots") return route.fulfill(json(["/games"]));
      if (path === "/api/local/list-dir")
        return route.fulfill(
          json([
            { name: "Astro", path: "/games/Astro", is_dir: true, size: 0 },
            { name: "Bloodborne", path: "/games/Bloodborne", is_dir: true, size: 0 },
          ]),
        );
      return route.fulfill(json({ error: "down" }, 503));
    },
  );
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });

  // Game 1, checked under ① and converted.
  const source = main.locator("#fpkg-source");
  await source.fill("/games/Astro");
  await source.blur();
  await expect(main.getByText("Astro Bot").first()).toBeVisible();
  await main.getByRole("button", { name: "Convert only" }).first().click();
  await expect.poll(() => builds).toEqual(["/games/Astro"]);

  // While it converts, game 2 is picked under ①: it goes into the queue.
  await main.getByRole("button", { name: "Choose folder" }).first().click();
  const picker = page.getByRole("dialog");
  await picker.getByRole("checkbox", { name: "Include Bloodborne" }).check();
  await picker.getByRole("button", { name: "Add 1" }).click();
  await expect(main.getByText("⑤ Queue")).toBeVisible();
  await expect(main.getByText("Bloodborne", { exact: true })).toBeVisible();
  expect(builds).toEqual(["/games/Astro"]);

  // Game 1's result stays on screen (its Install / Launch / Show folder stay reachable) and
  // game 2 waits for it; putting the result away starts game 2, with no Start pressed.
  const another = main.getByRole("button", { name: "Convert another game" });
  await expect(another).toBeVisible({ timeout: 20_000 });
  expect(builds).toEqual(["/games/Astro"]);
  await another.click();
  await expect.poll(() => builds, { timeout: 20_000 }).toEqual(["/games/Astro", "/games/Bloodborne"]);
  await expect(main.getByText(/Done — \/out\/Bloodborne\.pkg/)).toBeVisible({ timeout: 20_000 });
});
