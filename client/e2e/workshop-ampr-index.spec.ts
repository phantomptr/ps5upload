// #621: Workshop's "Rebuild AMPR index" for a libSceAmpr game folder: the engine job runs, the
// result says what changed, and the game is checked again.
import { expect, test } from "@playwright/test";

test("rebuilds a game folder's AMPR index", async ({ page }) => {
  await page.setViewportSize({ width: 1400, height: 1200 });
  const rebuilt: string[] = [];
  let inspects = 0;
  let polls = 0;
  const json = (body: unknown, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname;
      const body = req.postDataJSON?.() as Record<string, unknown> | null;
      if (path === "/api/fpkg/inspect") {
        inspects++;
        const amprGame = String(body?.source) === "/games/Spider";
        return route.fulfill(
          json({
            source: `folder ${body?.source}`,
            files: 5,
            bytes: 5 << 20,
            content_id: "UP9000-PPSA03016_00-MARVELSSPIDERMAN",
            title: amprGame ? "Spider-Man 2" : "Astro Bot",
            planned_size: 5 << 20,
            output_free: 1 << 40,
            checks: [{ name: "eboot.bin", ok: true, detail: "ok" }],
            ampr_packs: amprGame ? { kind: "ok", runtime_version: "0.4.2.1" } : { kind: "no_ampr" },
          }),
        );
      }
      if (path === "/api/fpkg/ampr-index") {
        rebuilt.push(String(body?.source));
        return route.fulfill(json({ job_id: "idx-1" }, 202));
      }
      if (path === "/api/jobs/idx-1") {
        polls++;
        if (polls < 3)
          return route.fulfill(
            json({ status: "running", started_at_ms: 0, stage: { id: "list", index: 0, count: 3, done: 0, total: 0 } }),
          );
        return route.fulfill(
          json({
            status: "done",
            started_at_ms: 0,
            completed_at_ms: 1,
            elapsed_ms: 1,
            tx_id_hex: "",
            bytes_sent: 0,
            dest: "/games/Spider/ampr_emu.index",
            files_sent: 5,
            commit_ack: { files: 5, differed: true, had_index: true, backup: "/games/Spider/ampr_emu.index.bak" },
          }),
        );
      }
      return route.fulfill(json({ error: "down" }, 503));
    },
  );
  await page.goto("/workshop");
  const main = page.getByRole("main");
  await expect(main.getByText("① Game")).toBeVisible({ timeout: 30_000 });
  const source = main.locator("#fpkg-source");
  const rebuild = main.getByTestId("ampr-index-rebuild");

  // A game that does not read through libSceAmpr: offered off, with why.
  await source.fill("/games/Astro");
  await source.blur();
  await expect(main.getByText("Astro Bot").first()).toBeVisible();
  await expect(rebuild).toBeDisabled();
  await expect(main.getByTestId("ampr-index-status")).toContainText("This one does not.");

  // An AMPR game folder: rebuilt in place, the result said, the game checked again.
  await source.fill("/games/Spider");
  await source.blur();
  await expect(main.getByText("Spider-Man 2").first()).toBeVisible();
  await expect(main.getByTestId("ampr-index-status")).toHaveCount(0);
  const checked = inspects;
  await rebuild.click();
  await expect(main.getByTestId("ampr-index-status")).toHaveText(
    "Rebuilt: 5 files. The old index was different. The old one is kept as ampr_emu.index.bak.",
    { timeout: 10_000 },
  );
  expect(rebuilt).toEqual(["/games/Spider"]);
  await expect.poll(() => inspects).toBeGreaterThan(checked);
});
