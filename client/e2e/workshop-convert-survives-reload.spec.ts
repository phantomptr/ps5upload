// #954: a convert started in the web UI keeps running in the engine when the page is closed or
// reloaded. The reopened page follows the same job in the same task, never "Interrupted", and
// shows its result when it ends. The engine here is a fake that answers the same routes.
import { expect, test, type Page } from "@playwright/test";

type StoredTask = { id: string; kind: string; status: string; engineJobId?: string };

async function convertTasks(page: Page): Promise<StoredTask[]> {
  return page.evaluate(() => {
    const raw = window.localStorage.getItem("ps5upload.tasks.v1");
    const tasks = raw ? (JSON.parse(raw).tasks as StoredTask[]) : [];
    return tasks.filter((t) => t.kind === "fpkg-convert");
  });
}

test("a convert keeps going through a page reload and is followed again (#954)", async ({ page }) => {
  await page.setViewportSize({ width: 1400, height: 1200 });
  const builds: string[] = [];
  // The engine's one build job: running until the test lets it finish.
  const job = { done: false, polls: 0, startedAt: Date.now() };
  const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      const body = req.postDataJSON?.() as Record<string, unknown> | null;
      if (path === "/api/fpkg/inspect")
        return route.fulfill(
          json({
            source: `folder ${String(body?.source ?? "")}`,
            files: 3,
            bytes: 3 << 20,
            content_id: "UP9000-PPSA01234_00-ASTROBOT00000000",
            title: "Astro Bot",
            planned_size: 3 << 20,
            output_free: 1 << 40,
            checks: [{ name: "eboot.bin", ok: true, detail: "ok" }],
          }),
        );
      if (path === "/api/fpkg/estimate") return route.fulfill(json({ error: "no" }, 500));
      if (path === "/api/fpkg/build") {
        builds.push(String(body?.source));
        return route.fulfill(json({ job_id: "job-1" }));
      }
      // The Workshop jobs running now, as the engine lists them for a page that did not start them.
      if (path === "/api/workshop/jobs")
        return route.fulfill(
          json(job.done || !builds.length ? [] : [{ job_id: "job-1", kind: "package", source: "/games/Astro", started_at_ms: job.startedAt }]),
        );
      if (path === "/api/jobs/job-1") {
        job.polls++;
        if (!job.done)
          return route.fulfill(
            json({
              status: "running",
              started_at_ms: job.startedAt,
              bytes_sent: job.polls,
              total_bytes: 1000,
              stage: { id: "write", index: 2, count: 4, done: job.polls, total: 1000 },
            }),
          );
        return route.fulfill(
          json({
            status: "done",
            started_at_ms: job.startedAt,
            completed_at_ms: Date.now(),
            elapsed_ms: 1,
            tx_id_hex: "PPSA01234",
            bytes_sent: 1000,
            dest: "/out/Astro.pkg",
          }),
        );
      }
      if (path === "/api/local/storage-roots") return route.fulfill(json(["/games"]));
      if (path === "/api/local/list-dir")
        return route.fulfill(json([{ name: "Astro", path: "/games/Astro", is_dir: true, size: 0 }]));
      return route.fulfill(json({ error: "down" }, 503));
    },
  );
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });

  const source = main.locator("#fpkg-source");
  await source.fill("/games/Astro");
  await source.blur();
  await expect(main.getByText("Astro Bot").first()).toBeVisible();
  await main.getByRole("button", { name: "Convert only" }).first().click();
  await expect.poll(() => builds).toEqual(["/games/Astro"]);
  await expect.poll(async () => (await convertTasks(page)).map((t) => [t.status, t.engineJobId])).toEqual([
    ["running", "job-1"],
  ]);
  const [before] = await convertTasks(page);

  // The page goes away mid-build and comes back: the engine's job never stopped.
  await page.reload();
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });
  // Followed again in the task it had: one task, running, not "Interrupted".
  await expect
    .poll(async () => (await convertTasks(page)).map((t) => [t.id, t.status]), { timeout: 20_000 })
    .toEqual([[before.id, "running"]]);
  await expect(page.getByText("Interrupted")).toHaveCount(0);
  const pollsAfterReload = job.polls;
  await expect.poll(() => job.polls).toBeGreaterThan(pollsAfterReload);
  expect(builds).toEqual(["/games/Astro"]);

  // It ends: the reopened page shows its result, and the task says done.
  job.done = true;
  await expect(main.getByRole("button", { name: "Convert another game" })).toBeVisible({ timeout: 20_000 });
  await expect.poll(async () => (await convertTasks(page)).map((t) => t.status)).toEqual(["done"]);
  expect(builds).toEqual(["/games/Astro"]);
});
